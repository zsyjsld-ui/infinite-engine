//! Bounded committed projectiles. Pure impact policy supplies faction checks,
//! per-hit scaling and armor formula; actual shields/health use World.damage_at.
use std::num::{NonZeroU16,NonZeroU32};
use crate::*;
pub const MAX_PROJECTILES:usize=4096;
pub const MAX_PROJECTILE_CASTS:usize=1024;
pub const MAX_PROJECTILE_HITS:usize=256;
pub const MAX_CAST_PROJECTILE_HITS:usize=4096;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
pub struct ProjectileId {pub index:u32,pub generation:u32}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
pub struct ProjectileCastId {pub index:u32,pub generation:u32}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ProjectileError {InvalidConfig,InvalidOwner,InvalidCast,InvalidProjectile,SealedCast,Full,CastReservationFull,TimeExhausted,StaleActivation,AlreadyResolved,UnfinishedTick,SkippedTick,EventLimit,EndLimit,Query(CombatQueryError),Spatial(SyncError)}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ProjectileLaunch {pub cast:ProjectileCastId,pub from:GridPosition,pub to:GridPosition,pub activate_at:Tick,pub travel_ticks:NonZeroU16,pub radius:u32,pub max_hits:NonZeroU16,pub damage:NonZeroU32,pub strict_ends:bool,pub require_sight:bool}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ProjectileState {pub id:ProjectileId,pub cast:ProjectileCastId,pub position:GridPosition,pub steps:u16,pub hits:u16,pub activate_at:Tick}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ProjectileEndReason {HitLimit,Range,OwnerGone,Cancelled}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ProjectileEnd {pub id:ProjectileId,pub cast:ProjectileCastId,pub reason:ProjectileEndReason}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ProjectileImpact {pub projectile:ProjectileId,pub cast:ProjectileCastId,pub owner:EntityId,pub target:EntityId,pub ordinal:u16,pub base_damage:NonZeroU32}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct ProjectileReport {pub visited:usize,pub advanced:usize,pub hits:usize,pub ended:usize,pub pending:usize}
struct Cast {owner:EntityId,cap:u16,target_capacity:usize,reserved:usize,refs:usize,sealed:bool,hits:Vec<(EntityId,u16)>}
struct CastSlot {generation:u32,value:Option<Cast>}
struct Flight {spec:ProjectileLaunch,position:GridPosition,steps:u16,seen:Vec<EntityId>}
struct Slot {generation:u32,value:Option<Flight>}
/// One instance per authoritative map. A cast is committed at create_cast;
/// owner death does not retract it. Despawn invalidates the owner generation;
/// logout/map change should explicitly cancel its cast(s) before transfer.
pub struct Projectiles {
 slots:Vec<Slot>,free:Vec<u32>,order:Vec<ProjectileId>,len:usize,
 casts:Vec<CastSlot>,free_casts:Vec<u32>,cast_count:usize,
 frame:SpatialFrame,hits:Vec<CombatSegmentHit>,planned:Vec<(EntityId,NonZeroU32)>,
 processing:Option<(Tick,usize)>,last_finished:Option<Tick>,
}
impl Projectiles {
 pub fn new(cell_size:NonZeroU32)->Self{Self{slots:Vec::new(),free:Vec::new(),order:Vec::new(),len:0,casts:Vec::new(),free_casts:Vec::new(),cast_count:0,frame:SpatialFrame::new(cell_size),hits:Vec::new(),planned:Vec::with_capacity(MAX_PROJECTILE_HITS),processing:None,last_finished:None}}
 pub fn len(&self)->usize{self.len}pub fn is_empty(&self)->bool{self.len==0}pub fn casts(&self)->usize{self.cast_count}
 fn cast(&self,id:ProjectileCastId)->Result<&Cast,ProjectileError>{self.casts.get(id.index as usize).filter(|s|s.generation==id.generation).and_then(|s|s.value.as_ref()).ok_or(ProjectileError::InvalidCast)}
 pub fn create_cast(&mut self,world:&World,owner:EntityId,max_hits_per_target:NonZeroU16,target_capacity:usize)->Result<ProjectileCastId,ProjectileError>{
  if target_capacity==0||target_capacity>MAX_CAST_PROJECTILE_HITS{return Err(ProjectileError::InvalidConfig);}if !world.health(owner).is_ok_and(|h|!h.is_depleted()){return Err(ProjectileError::InvalidOwner);}if self.free_casts.is_empty()&&self.casts.len()==MAX_PROJECTILE_CASTS{return Err(ProjectileError::Full);}
  let index=self.free_casts.pop().unwrap_or_else(||{let i=self.casts.len()as u32;self.casts.push(CastSlot{generation:0,value:None});i});let slot=&mut self.casts[index as usize];slot.value=Some(Cast{owner,cap:max_hits_per_target.get(),target_capacity,reserved:0,refs:0,sealed:false,hits:Vec::with_capacity(target_capacity)});self.cast_count+=1;Ok(ProjectileCastId{index,generation:slot.generation})
 }
 fn release_cast_if_done(&mut self,id:ProjectileCastId){let slot=&mut self.casts[id.index as usize];if slot.value.as_ref().is_some_and(|c|c.sealed&&c.refs==0){slot.value=None;self.cast_count-=1;if let Some(g)=slot.generation.checked_add(1){slot.generation=g;self.free_casts.push(id.index);}}}
 /// Seal after admitting all pellets. Reservations/counts persist until every
 /// pellet ends; a cancelled projectile does not refund the per-cast hit budget.
 pub fn seal_cast(&mut self,id:ProjectileCastId)->Result<(),ProjectileError>{self.cast(id)?;self.casts[id.index as usize].value.as_mut().unwrap().sealed=true;self.release_cast_if_done(id);Ok(())}
 pub fn launch(&mut self,world:&World,spec:ProjectileLaunch)->Result<ProjectileId,ProjectileError>{
  if self.processing.is_some(){return Err(ProjectileError::UnfinishedTick);}let cast=self.cast(spec.cast)?;if cast.sealed{return Err(ProjectileError::SealedCast);}if world.position(cast.owner).is_err(){return Err(ProjectileError::InvalidOwner);}if spec.max_hits.get()as usize>MAX_PROJECTILE_HITS{return Err(ProjectileError::InvalidConfig);}crate::segment::SegmentGeometry::new(SegmentSweep{from:spec.from,to:spec.to,radius:spec.radius,strict_ends:spec.strict_ends}).map_err(ProjectileError::Query)?;
  if spec.activate_at<world.tick()||self.last_finished.is_some_and(|t|spec.activate_at<=t){return Err(ProjectileError::StaleActivation);}spec.activate_at.0.checked_add(u64::from(spec.travel_ticks.get())-1).ok_or(ProjectileError::TimeExhausted)?;
  if cast.reserved+usize::from(spec.max_hits.get())>cast.target_capacity{return Err(ProjectileError::CastReservationFull);}if self.free.is_empty()&&self.slots.len()==MAX_PROJECTILES{return Err(ProjectileError::Full);}if self.order.len()>=MAX_PROJECTILES{self.compact_order();}
  let index=self.free.pop().unwrap_or_else(||{let i=self.slots.len()as u32;self.slots.push(Slot{generation:0,value:None});i});let slot=&mut self.slots[index as usize];let id=ProjectileId{index,generation:slot.generation};slot.value=Some(Flight{spec,position:spec.from,steps:0,seen:Vec::with_capacity(spec.max_hits.get()as usize)});self.order.push(id);self.len+=1;let cast=self.casts[spec.cast.index as usize].value.as_mut().unwrap();cast.reserved+=spec.max_hits.get()as usize;cast.refs+=1;Ok(id)
 }
 fn flight(&self,id:ProjectileId)->Option<&Flight>{self.slots.get(id.index as usize).filter(|s|s.generation==id.generation).and_then(|s|s.value.as_ref())}
 pub fn state(&self,id:ProjectileId)->Result<ProjectileState,ProjectileError>{let p=self.flight(id).ok_or(ProjectileError::InvalidProjectile)?;Ok(ProjectileState{id,cast:p.spec.cast,position:p.position,steps:p.steps,hits:p.seen.len()as u16,activate_at:p.spec.activate_at})}
 fn finish(&mut self,id:ProjectileId,reason:ProjectileEndReason)->ProjectileEnd{let slot=&mut self.slots[id.index as usize];let p=slot.value.take().unwrap();self.len-=1;if let Some(g)=slot.generation.checked_add(1){slot.generation=g;self.free.push(id.index);}let cast=self.casts[p.spec.cast.index as usize].value.as_mut().unwrap();cast.refs-=1;self.release_cast_if_done(p.spec.cast);ProjectileEnd{id,cast:p.spec.cast,reason}}
 pub fn cancel(&mut self,id:ProjectileId)->Result<ProjectileEnd,ProjectileError>{self.flight(id).ok_or(ProjectileError::InvalidProjectile)?;Ok(self.finish(id,ProjectileEndReason::Cancelled))}
 /// Returned notices are the explicit ownership handoff; no combat hit occurs.
 pub fn cancel_cast(&mut self,id:ProjectileCastId)->Result<Vec<ProjectileEnd>,ProjectileError>{self.cast(id)?;let ids:Vec<_>=self.order.iter().copied().filter(|&p|self.flight(p).is_some_and(|p|p.spec.cast==id)).collect();let mut out=Vec::with_capacity(ids.len());for p in ids{out.push(self.finish(p,ProjectileEndReason::Cancelled));}if self.cast(id).is_ok(){self.seal_cast(id)?;}Ok(out)}
 fn compact_order(&mut self){let slots=&self.slots;self.order.retain(|id|slots[id.index as usize].generation==id.generation&&slots[id.index as usize].value.is_some());}
 /// Recapture only when entering a phase. No world movement can occur while
 /// the returned phase is borrowed. Failed current projectile is untouched;
 /// earlier committed projectiles must NOT be replayed. Resume before next tick.
 pub fn begin<'a>(&'a mut self,world:&'a mut World)->Result<ProjectilePhase<'a>,ProjectileError>{
  let tick=world.tick();if let Some((old,_))=self.processing{if old!=tick{return Err(ProjectileError::UnfinishedTick);}}else{if self.last_finished.is_some_and(|old|old>=tick){return Err(ProjectileError::AlreadyResolved);}if self.order.iter().any(|&id|self.flight(id).is_some_and(|p|p.spec.activate_at.0+u64::from(p.steps)<tick.0)){return Err(ProjectileError::SkippedTick);}}
  self.frame.capture(world).map_err(ProjectileError::Spatial)?;if self.processing.is_none(){self.processing=Some((tick,0));}Ok(ProjectilePhase{storage:self,world})
 }
}
pub struct ProjectilePhase<'a>{storage:&'a mut Projectiles,world:&'a mut World}
impl ProjectilePhase<'_>{
 /// Policy must be pure: None means ineligible, Some is final integer damage
 /// before shields. It can apply ordinal-dependent scaling and M1 attributes.
 /// The full sweep/policy/event admission completes before this projectile's
 /// first mutation. Query failures consume work but preserve its state/events.
 pub fn step(&mut self,work_limit:usize,budget:&mut CombatQueryBudget,events:&mut Vec<CombatEvent>,event_limit:usize,ends:&mut Vec<ProjectileEnd>,mut resolve:impl FnMut(&World,ProjectileImpact)->Option<NonZeroU32>)->Result<ProjectileReport,ProjectileError>{
  if work_limit>MAX_PROJECTILES{return Err(ProjectileError::InvalidConfig);}if event_limit>MAX_ACTIVE_COMBATANTS||events.len()>event_limit{return Err(ProjectileError::EventLimit);}let s=&mut*self.storage;let Some((tick,mut cursor))=s.processing else{return Err(ProjectileError::AlreadyResolved);};let mut report=ProjectileReport::default();
  while cursor<s.order.len()&&report.visited<work_limit{
   let id=s.order[cursor];report.visited+=1;let Some(p)=s.flight(id)else{cursor+=1;s.processing=Some((tick,cursor));continue;};if ends.len()>=MAX_PROJECTILES{return Err(ProjectileError::EndLimit);}let spec=p.spec;let cast=s.cast(spec.cast).expect("active cast retained");let owner=cast.owner;
   if self.world.position(owner).is_err(){ends.push(s.finish(id,ProjectileEndReason::OwnerGone));report.ended+=1;cursor+=1;s.processing=Some((tick,cursor));continue;}
   if tick<spec.activate_at{cursor+=1;s.processing=Some((tick,cursor));continue;}if spec.activate_at.0.checked_add(u64::from(p.steps))!=Some(tick.0){return Err(ProjectileError::SkippedTick);}
   let next_step=p.steps+1;let point=|a:i32,b:i32|{let delta=i64::from(b)-i64::from(a);(i64::from(a)+delta*i64::from(next_step)/i64::from(spec.travel_ticks.get()))as i32};let to=GridPosition{x:point(spec.from.x,spec.to.x),y:point(spec.from.y,spec.to.y)};let from=p.position;
   let frame=&s.frame;let slots=&s.slots;let cast=s.casts[spec.cast.index as usize].value.as_ref().unwrap();let p=slots[id.index as usize].value.as_ref().unwrap();
   let context=CombatQueryContext::new(frame,self.world.collision_map());context.collect_segment_unsorted(CombatSegmentQuery{sweep:SegmentSweep{from,to,radius:spec.radius,strict_ends:spec.strict_ends},exclude:Some(owner),sight_origin:spec.require_sight.then_some(from)},budget,&mut s.hits,|hit|self.world.health(hit.entity).is_ok_and(|h|!h.is_depleted())&&!p.seen.contains(&hit.entity)&&cast.hits.iter().find(|(e,_)|*e==hit.entity).is_none_or(|(_,count)|*count<cast.cap)).map_err(ProjectileError::Query)?;
   // Partition only the prefix likely to fill the quota, then sort it. Grow
   // rejected prefixes geometrically so an all-rejected policy stays bounded.
   // The complete query/budget checks still run before this selection.
   s.planned.clear();let mut remaining=s.hits.as_mut_slice();let quota=usize::from(spec.max_hits.get())-p.seen.len();let mut batch=quota.max(16);
   while !remaining.is_empty()&&s.planned.len()<quota{
    let take=if remaining.len()>64&&batch<remaining.len()/4{batch}else{remaining.len()};if take<remaining.len(){remaining.select_nth_unstable_by_key(take,hit_key);}let(prefix,tail)=remaining.split_at_mut(take);prefix.sort_unstable_by_key(hit_key);
    for hit in prefix{if s.planned.len()==quota{break;}let impact=ProjectileImpact{projectile:id,cast:spec.cast,owner,target:hit.hit.entity,ordinal:(p.seen.len()+s.planned.len())as u16,base_damage:spec.damage};if let Some(damage)=resolve(self.world,impact){s.planned.push((hit.hit.entity,damage));}}
    remaining=tail;batch*=2;
   }
   if s.planned.len()>event_limit-events.len(){return Err(ProjectileError::EventLimit);}events.reserve(s.planned.len());ends.reserve(1);let next_ready=self.world.attack_state(owner).expect("committed owner exists").next_ready;
   for &(target,damage)in &s.planned{let row=self.world.dense_index(target).expect("phase retains membership");let health=self.world.damage_at(row,damage.get()).expect("eligible health configured").health;events.push(CombatEvent{attacker:owner,target,kind:CombatEventKind::Hit{health,next_ready}});let cast=s.casts[spec.cast.index as usize].value.as_mut().unwrap();if let Some((_,count))=cast.hits.iter_mut().find(|(e,_)|*e==target){*count+=1;}else{assert!(cast.hits.len()<cast.target_capacity);cast.hits.push((target,1));}s.slots[id.index as usize].value.as_mut().unwrap().seen.push(target);}
   report.hits+=s.planned.len();report.advanced+=1;let p=s.slots[id.index as usize].value.as_mut().unwrap();p.position=to;p.steps=next_step;let reason=if p.seen.len()==spec.max_hits.get()as usize{Some(ProjectileEndReason::HitLimit)}else if next_step==spec.travel_ticks.get(){Some(ProjectileEndReason::Range)}else{None};if let Some(reason)=reason{ends.push(s.finish(id,reason));report.ended+=1;}cursor+=1;s.processing=Some((tick,cursor));
  }
  report.pending=s.order.len()-cursor;if report.pending==0{s.processing=None;s.last_finished=Some(tick);s.compact_order();}Ok(report)
 }
}

fn hit_key(hit:&CombatSegmentHit)->(u64,u128,u32,u32){(hit.along,hit.distance_scaled,hit.hit.entity.index,hit.hit.entity.generation)}
