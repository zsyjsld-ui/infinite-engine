//! Scheduled periodic effects and source-owned additive attribute buffs.
use std::num::{NonZeroU16,NonZeroU32};
use crate::tick_queue::TickEdit;
use crate::{AttributeError,CombatEvent,CombatEventKind,EntityId,HealthCommand,HealthError,
    HealthOutcome,ResourceCommand,ResourceError,ResourceOutcome,Tick,TickQueue,TickQueueError,
    TickTaskId,World,ATTRIBUTE_COUNT,MAX_ACTIVE_COMBATANTS};
pub const MAX_ACTIVE_EFFECTS:usize=65_536;
pub const MAX_EFFECTS_PER_ENTITY:usize=64;
pub const MAX_EFFECT_WORK:usize=4096;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
pub struct EffectId(pub TickTaskId);
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum PeriodicKind { Damage, Heal, RestoreResource, DrainResource }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum PeriodicRefresh { Replace, StrongerOrExtend }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct PeriodicEffect {
    pub source:EntityId,pub target:EntityId,pub key:u16,pub kind:PeriodicKind,
    pub amount:NonZeroU32,pub first_delay:u16,pub interval:NonZeroU16,
    pub pulses:NonZeroU16,pub refresh:PeriodicRefresh,
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct BuffModifiers { pub flat:[i32;ATTRIBUTE_COUNT],pub rate:[i32;ATTRIBUTE_COUNT] }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct AttributeBuff { pub source:EntityId,pub target:EntityId,pub key:u16,pub duration:NonZeroU16,pub modifiers:BuffModifiers }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum EffectDisposition { Added,Replaced,Extended }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct EffectApplication { pub id:EffectId,pub expires_at:Tick,pub disposition:EffectDisposition }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum EffectStateKind { Periodic{kind:PeriodicKind,amount:NonZeroU32,interval:NonZeroU16,remaining:u16},Buff(BuffModifiers) }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct EffectState { pub id:EffectId,pub source:EntityId,pub target:EntityId,pub key:u16,pub next_due:Tick,pub expires_at:Tick,pub kind:EffectStateKind }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum EffectEnd { Completed,Expired,Manual,SourceGone,TargetDepleted,ResourceDepleted,StateUnavailable }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum EffectEventKind { Health{kind:PeriodicKind,outcome:HealthOutcome},Resource(ResourceOutcome),Ended(EffectEnd) }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct EffectEvent { pub id:EffectId,pub source:EntityId,pub target:EntityId,pub key:u16,pub scheduled_at:Tick,pub kind:EffectEventKind,pub ended:Option<EffectEnd> }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum EffectError { InvalidEntity,InvalidEffect,KindConflict,PerEntityLimit,TimeOverflow,
    InvalidWorkLimit,CombatEventCapacity,TargetDepleted,Attribute(AttributeError),Health(HealthError),Resource(ResourceError),Queue(TickQueueError) }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct EffectReport { pub tick:Tick,pub processed:usize,pub pulses:usize,pub ended:usize,pub overdue:Option<Tick>,pub blocked:Option<EffectError> }
#[derive(Clone,Copy)]
enum Payload { Periodic{kind:PeriodicKind,amount:NonZeroU32,interval:NonZeroU16,remaining:u16},Buff(BuffModifiers) }
#[derive(Clone,Copy)]
struct Effect { source:EntityId,target:EntityId,key:u16,expires:Tick,payload:Payload }
pub(crate) struct EffectColumns { queue:TickQueue<Effect>,targets:Vec<Vec<TickTaskId>>,batch:Vec<TickTaskId>,edits:Vec<TickEdit<Effect>>,batching:bool }
impl Default for EffectColumns{fn default()->Self{Self{queue:TickQueue::new(MAX_ACTIVE_EFFECTS).unwrap(),targets:Vec::new(),batch:Vec::new(),edits:Vec::new(),batching:false}}}
impl EffectColumns {
    pub(crate) fn next_due(&self)->Option<Tick>{self.queue.peek().map(|t|t.due)}
    pub(crate) fn spawn(&mut self){self.targets.push(Vec::new());}
    pub(crate) fn has_target_effects(&self,row:usize)->bool{!self.targets[row].is_empty()}
    pub(crate) fn despawn(&mut self,row:usize){for id in self.targets.swap_remove(row){self.queue.cancel(id);}}
    fn find(&self,row:usize,source:EntityId,key:u16)->Option<TickTaskId>{self.targets[row].iter().copied().find(|id|self.queue.get(*id).is_some_and(|t|t.value.source==source&&t.value.key==key))}
    fn preflight_new(&self,row:usize)->Result<(),EffectError>{if self.targets[row].len()==MAX_EFFECTS_PER_ENTITY{return Err(EffectError::PerEntityLimit);}self.queue.check_schedule().map_err(EffectError::Queue)}
    fn remove(&mut self,row:usize,id:TickTaskId){if self.batching{self.edits.push(TickEdit::Cancel(id));}else{self.queue.cancel(id);}let index=self.targets[row].iter().position(|v|*v==id).expect("owned effect");self.targets[row].swap_remove(index);}
    fn repeat(&mut self,id:TickTaskId,due:Tick,value:Effect){if self.batching{self.edits.push(TickEdit::Replace(id,due,value));}else{self.queue.replace_value(id,value).unwrap();self.queue.reschedule(id,due).unwrap();}}
}
fn last_due(first:Tick,interval:NonZeroU16,pulses:u16)->Result<Tick,EffectError>{first.0.checked_add(u64::from(interval.get())*u64::from(pulses-1)).map(Tick).ok_or(EffectError::TimeOverflow)}
fn event(id:TickTaskId,e:Effect,due:Tick,kind:EffectEventKind,ended:Option<EffectEnd>)->EffectEvent{EffectEvent{id:EffectId(id),source:e.source,target:e.target,key:e.key,scheduled_at:due,kind,ended}}
impl World {
    fn effect_target(&self,source:EntityId,target:EntityId)->Result<usize,EffectError>{
        self.dense_index(source).map_err(|_|EffectError::InvalidEntity)?;let row=self.dense_index(target).map_err(|_|EffectError::InvalidEntity)?;
        if self.health.read(row).map_err(EffectError::Health)?.is_depleted(){return Err(EffectError::TargetDepleted);}Ok(row)
    }
    /// Key namespace is (source generation, target generation, key). Distinct
    /// keys/sources coexist. A dead but existing source may retain periodic work,
    /// matching the legacy source-presence rule; target death stops it.
    pub fn apply_periodic(&mut self,spec:PeriodicEffect)->Result<EffectApplication,EffectError>{
        let row=self.effect_target(spec.source,spec.target)?;
        if matches!(spec.kind,PeriodicKind::RestoreResource|PeriodicKind::DrainResource){self.resource.read(row).map_err(EffectError::Resource)?;}
        let first=Tick(self.tick().0.checked_add(u64::from(spec.first_delay)).ok_or(EffectError::TimeOverflow)?);
        let expires=last_due(first,spec.interval,spec.pulses.get())?;
        let replacement=Effect{source:spec.source,target:spec.target,key:spec.key,expires,payload:Payload::Periodic{kind:spec.kind,amount:spec.amount,interval:spec.interval,remaining:spec.pulses.get()}};
        if let Some(id)=self.effects.find(row,spec.source,spec.key){
            let old=*self.effects.queue.get(id).unwrap();let Payload::Periodic{kind,amount,interval,remaining}=old.value.payload else{return Err(EffectError::KindConflict);};
            if kind!=spec.kind{return Err(EffectError::KindConflict);}
            if spec.refresh==PeriodicRefresh::StrongerOrExtend&&amount>spec.amount&&old.value.expires>self.tick(){
                let remaining=remaining.max(spec.pulses.get());last_due(old.due,interval,remaining)?;
                let extended=Effect{expires:old.value.expires.max(expires),payload:Payload::Periodic{kind,amount,interval,remaining},..old.value};
                self.effects.queue.replace_value(id,extended).unwrap();return Ok(EffectApplication{id:EffectId(id),expires_at:extended.expires,disposition:EffectDisposition::Extended});
            }
            self.effects.queue.replace_value(id,replacement).unwrap();self.effects.queue.reschedule(id,first).unwrap();
            Ok(EffectApplication{id:EffectId(id),expires_at:expires,disposition:EffectDisposition::Replaced})
        }else{
            self.effects.preflight_new(row)?;let id=self.effects.queue.schedule(first,replacement).map_err(EffectError::Queue)?;self.effects.targets[row].push(id);
            Ok(EffectApplication{id:EffectId(id),expires_at:expires,disposition:EffectDisposition::Added})
        }
    }
    /// Buff modifiers own a separate aggregate from equipment/application values.
    /// Refreshing the same source/key replaces its contribution, never multiplies it.
    pub fn apply_buff(&mut self,spec:AttributeBuff)->Result<EffectApplication,EffectError>{
        let row=self.effect_target(spec.source,spec.target)?;let expires=Tick(self.tick().0.checked_add(u64::from(spec.duration.get())).ok_or(EffectError::TimeOverflow)?);
        let replacement=Effect{source:spec.source,target:spec.target,key:spec.key,expires,payload:Payload::Buff(spec.modifiers)};
        if let Some(id)=self.effects.find(row,spec.source,spec.key){
            let Payload::Buff(old)=self.effects.queue.get(id).unwrap().value.payload else{return Err(EffectError::KindConflict);};
            if old!=spec.modifiers{self.attributes.prepare_effect(row,spec.target).map_err(EffectError::Attribute)?;self.attributes.adjust_effect(row,old.flat,old.rate,-1);self.attributes.adjust_effect(row,spec.modifiers.flat,spec.modifiers.rate,1);}
            self.effects.queue.replace_value(id,replacement).unwrap();self.effects.queue.reschedule(id,expires).unwrap();
            Ok(EffectApplication{id:EffectId(id),expires_at:expires,disposition:EffectDisposition::Replaced})
        }else{
            self.effects.preflight_new(row)?;self.attributes.prepare_effect(row,spec.target).map_err(EffectError::Attribute)?;
            let id=self.effects.queue.schedule(expires,replacement).expect("preflighted queue");self.effects.targets[row].push(id);self.attributes.adjust_effect(row,spec.modifiers.flat,spec.modifiers.rate,1);
            Ok(EffectApplication{id:EffectId(id),expires_at:expires,disposition:EffectDisposition::Added})
        }
    }
    pub fn cancel_effect(&mut self,id:EffectId)->Result<EffectEvent,EffectError>{
        let task=*self.effects.queue.get(id.0).ok_or(EffectError::InvalidEffect)?;let e=task.value;let row=self.dense_index(e.target).map_err(|_|EffectError::InvalidEntity)?;
        if let Payload::Buff(modifiers)=e.payload{self.attributes.prepare_effect(row,e.target).map_err(EffectError::Attribute)?;self.attributes.adjust_effect(row,modifiers.flat,modifiers.rate,-1);}
        self.effects.remove(row,id.0);Ok(event(id.0,e,self.tick(),EffectEventKind::Ended(EffectEnd::Manual),Some(EffectEnd::Manual)))
    }
    pub fn active_effects(&self)->usize{self.effects.queue.len()}
    pub fn effect_state(&self,id:EffectId)->Result<EffectState,EffectError>{
        let t=self.effects.queue.get(id.0).ok_or(EffectError::InvalidEffect)?;let e=t.value;
        let kind=match e.payload{Payload::Buff(m)=>EffectStateKind::Buff(m),Payload::Periodic{kind,amount,interval,remaining}=>EffectStateKind::Periodic{kind,amount,interval,remaining}};
        Ok(EffectState{id,source:e.source,target:e.target,key:e.key,next_due:t.due,expires_at:e.expires,kind})
    }
    /// Bounded target-local enumeration, suitable for component snapshots.
    pub fn entity_effects(&self,entity:EntityId,output:&mut Vec<EffectId>)->Result<(),EffectError>{
        let row=self.dense_index(entity).map_err(|_|EffectError::InvalidEntity)?;output.clear();output.extend(self.effects.targets[row].iter().copied().map(EffectId));Ok(())
    }
    /// Same-tick draining is explicit. Output is cleared; combat feedback is
    /// appended. A capacity/dirty-attribute block leaves the next task intact.
    /// Catch-up keeps each original scheduled tick and never skips a pulse.
    pub fn resolve_effects(&mut self,limit:usize,output:&mut Vec<EffectEvent>,combat:&mut Vec<CombatEvent>)->Result<EffectReport,EffectError>{
        if limit>MAX_EFFECT_WORK{return Err(EffectError::InvalidWorkLimit);}if combat.len()>MAX_ACTIVE_COMBATANTS{return Err(EffectError::CombatEventCapacity);}
        output.clear();output.reserve(limit);let now=self.tick();let mut report=EffectReport{tick:now,processed:0,pulses:0,ended:0,overdue:None,blocked:None};
        self.effects.batching=self.effects.queue.current_batch(now,limit,&mut self.effects.batch);
        while report.processed<limit{
            let next=if self.effects.batching{self.effects.batch.get(report.processed).and_then(|id|self.effects.queue.get(*id))}else{self.effects.queue.peek()};
            let Some(task)=next.copied().filter(|t|t.due<=now)else{break;};let e=task.value;let row=self.dense_index(e.target).expect("despawn removes target effects");
            if let Payload::Buff(modifiers)=e.payload{
                if let Err(error)=self.attributes.prepare_effect(row,e.target){report.blocked=Some(EffectError::Attribute(error));break;}
                self.attributes.adjust_effect(row,modifiers.flat,modifiers.rate,-1);self.effects.remove(row,task.id);
                output.push(event(task.id,e,task.due,EffectEventKind::Ended(EffectEnd::Expired),Some(EffectEnd::Expired)));report.processed+=1;report.ended+=1;continue;
            }
            let Payload::Periodic{kind,amount,interval,remaining}=e.payload else{unreachable!()};
            let end=if self.dense_index(e.source).is_err(){Some(EffectEnd::SourceGone)}else{match self.health.read(row){Err(_)=>Some(EffectEnd::StateUnavailable),Ok(h)if h.is_depleted()=>Some(EffectEnd::TargetDepleted),_=>None}};
            if let Some(reason)=end{self.effects.remove(row,task.id);output.push(event(task.id,e,task.due,EffectEventKind::Ended(reason),Some(reason)));report.processed+=1;report.ended+=1;continue;}
            if kind==PeriodicKind::Damage&&combat.len()==MAX_ACTIVE_COMBATANTS{report.blocked=Some(EffectError::CombatEventCapacity);break;}
            let (outcome,depleted)=match kind{
                PeriodicKind::Damage|PeriodicKind::Heal=>{
                    let health=if kind==PeriodicKind::Damage{self.damage_at(row,amount.get()).expect("live configured target").health}else{self.health.apply(row,HealthCommand::Heal{entity:e.target,amount:amount.get()}).expect("live configured target")};
                    if kind==PeriodicKind::Damage{let next_ready=self.attack_state(e.source).expect("live source handle").next_ready;combat.push(CombatEvent{attacker:e.source,target:e.target,kind:CombatEventKind::Hit{health,next_ready}});}
                    (EffectEventKind::Health{kind,outcome:health},health.after.is_depleted())
                },
                PeriodicKind::RestoreResource=>(EffectEventKind::Resource(self.resource_at(row,ResourceCommand::Restore(amount.get())).expect("configured resource retained")),false),
                PeriodicKind::DrainResource=>{let available=self.resource.read(row).expect("configured resource retained").current;(EffectEventKind::Resource(self.resource_at(row,ResourceCommand::Spend(available.min(amount.get()))).expect("saturating resource drain")),false)},
            };
            let ended=if depleted{Some(EffectEnd::TargetDepleted)}else if kind==PeriodicKind::DrainResource&&self.resource.read(row).unwrap().current==0{Some(EffectEnd::ResourceDepleted)}else if remaining==1{Some(EffectEnd::Completed)}else{None};
            if ended.is_some(){self.effects.remove(row,task.id);report.ended+=1;}else{
                self.effects.repeat(task.id,Tick(task.due.0+u64::from(interval.get())),Effect{payload:Payload::Periodic{kind,amount,interval,remaining:remaining-1},..e});
            }
            output.push(event(task.id,e,task.due,outcome,ended));report.processed+=1;report.pulses+=1;
        }
        if self.effects.batching{self.effects.queue.apply_batch(&self.effects.edits);self.effects.edits.clear();self.effects.batching=false;}
        report.overdue=self.effects.queue.peek().map(|t|t.due).filter(|t|*t<=now);Ok(report)
    }
}
