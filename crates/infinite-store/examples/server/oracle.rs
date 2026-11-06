//! Compact validation history, separate from network caches. Final convergence
//! additionally compares exact rows; these fingerprints check intermediate frames.
use infinite_core::*;
use std::collections::BTreeMap;
use super::simulation::{Simulation,full_view};
#[derive(Clone,Copy,Default,Debug,PartialEq,Eq)]
pub struct Fingerprint{sum:u64,xor:u64,count:usize}
impl Fingerprint{
    fn add(&mut self,hash:u64){self.sum=self.sum.wrapping_add(hash);self.xor^=hash.rotate_left((hash%63)as u32);self.count+=1;}
    fn combine(mut self,other:Self)->Self{self.sum=self.sum.wrapping_add(other.sum);self.xor^=other.xor;self.count+=other.count;self}
}
fn hash(words:impl IntoIterator<Item=u64>)->u64{words.into_iter().fold(14695981039346656037,|h,v|(h^v).wrapping_mul(1099511628211))}
fn motion_row(row:&MotionState)->u64{hash([u64::from(row.entity.index),u64::from(row.entity.generation),row.position.x as u64,row.position.y as u64,row.movement as u64])}
fn vital_row(row:&VitalState)->u64{
    let mut words=vec![u64::from(row.entity.index),u64::from(row.entity.generation)];
    if let Some(h)=row.health{words.extend([1,u64::from(h.current),u64::from(h.maximum.get())]);}else{words.push(0);}
    if let Some(w)=row.windup{words.extend([1,u64::from(w.target.index),u64::from(w.target.generation),w.started_at.0,w.hit_at.0]);}else{words.push(0);}
    hash(words)
}
fn component_row(row:&VisibleComponent)->u64{
    let r=row.record;let mut words=vec![u64::from(row.entity.index),u64::from(row.entity.generation),r.key.kind as u64,
        u64::from(r.key.instance),r.scope as u64,u64::from(r.fields.mask()),u64::from(r.fields.reference_mask())];
    for i in 0..COMPONENT_FIELDS{if let Some(v)=r.fields.get(i){words.push(u64::from(v));}}
    hash(words)
}
pub fn motion(rows:&[MotionState])->Fingerprint{let mut result=Fingerprint::default();for row in rows{result.add(motion_row(row));}result}
pub fn vital(rows:&[VitalState])->Fingerprint{let mut result=Fingerprint::default();for row in rows{result.add(vital_row(row));}result}
pub fn components(rows:&[VisibleComponent])->Fingerprint{let mut result=Fingerprint::default();for row in rows{result.add(component_row(row));}result}
pub struct Snapshot{pub tick:Tick,pub motion:Fingerprint,pub vital:Fingerprint,common:Fingerprint,private:BTreeMap<(u32,u32),Fingerprint>}
impl Snapshot{
    pub fn capture(sim:&Simulation,view:ViewRect)->Self{
        let all=sim.components.motion_frame().visible(full_view()).unwrap();
        let mut result=Self{tick:sim.world.tick(),motion:Fingerprint::default(),vital:Fingerprint::default(),common:Fingerprint::default(),private:BTreeMap::new()};
        for row in all{
            let visible=view.contains(row.position);
            if visible{
                result.motion.add(motion_row(&row));
                let state=VitalState{entity:row.entity,health:sim.world.health(row.entity).ok(),windup:sim.world.attack_state(row.entity).unwrap().windup
                    .filter(|w|sim.world.position(w.target).ok().is_some_and(|p|view.contains(p)))
                    .map(|w|WindupView{target:w.target,started_at:w.started_at,hit_at:w.hit_at})};
                result.vital.add(vital_row(&state));
            }
            for &record in sim.world.components(row.entity).unwrap(){
                let hash=component_row(&VisibleComponent{entity:row.entity,record});
                match record.scope{
                    ComponentScope::Owner=>result.private.entry((row.entity.index,row.entity.generation)).or_default().add(hash),
                    ComponentScope::Map=>result.common.add(hash),
                    ComponentScope::Nearby=>if visible{result.common.add(hash);},
                }
            }
        }
        result
    }
    pub fn components(&self,owner:EntityId)->Fingerprint{self.common.combine(self.private.get(&(owner.index,owner.generation)).copied().unwrap_or_default())}
}
