//! Death/corpse/respawn deadlines. Placement and temporary-effect policy remain
//! application decisions; a checked ticket completes a clean NPC life reset.
use std::num::NonZeroU32;
use crate::tick_queue::TickEdit;
use crate::{AttackWindup,EntityId,GridPosition,HealthError,HealthOutcome,HealthCommand,
    ResourceCommand,ResourceOutcome,SkillEvent,Tick,TickQueue,TickQueueError,TickTaskId,World,WorldError};
pub const MAX_LIFECYCLE_ENTITIES:usize=65_536;
pub const MAX_LIFECYCLE_WORK:usize=4096;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct LifecycleProfile {
    pub corpse_ticks:u32,
    /// None removes the entity after corpse_ticks. Some waits for placement
    /// approval after this many ticks from death, not from corpse removal.
    pub respawn_after:Option<u32>,
}
#[cfg(test)]
mod tests {
    use super::*;
    fn actor(w:&mut World)->EntityId{
        let e=w.spawn(GridPosition::default()).unwrap();w.configure_health(e,1,NonZeroU32::new(1).unwrap()).unwrap();
        w.register_lifecycle(e,LifecycleProfile{corpse_ticks:0,respawn_after:None}).unwrap();e
    }
    #[test]
    fn lifecycle_full_queue_keeps_pending_and_one_item_slices_make_progress(){
        let mut w=World::default();w.lifecycle.queue=TickQueue::new(1).unwrap();let a=actor(&mut w);let b=actor(&mut w);
        w.apply_damage(a,1).unwrap();w.apply_damage(b,1).unwrap();assert_eq!(w.lifecycle_activity().unscheduled,1);
        let mut events=Vec::new();let mut removed=Vec::new();
        for _ in 0..5{let r=w.resolve_lifecycle(1,&mut events).unwrap();assert_eq!(r.visited,1);assert!(r.blocked.is_none());
            removed.extend(events.iter().filter(|e|e.kind==LifecycleEventKind::Removed).map(|e|e.entity));}
        assert_eq!(removed,vec![a,b]);assert_eq!(w.lifecycle_activity().registered,0);assert_eq!(w.lifecycle_activity().unscheduled,0);
    }
    #[test]
    fn lifecycle_tick_and_life_revision_overflow_never_wrap_or_drop(){
        assert_eq!(due(Tick(u64::MAX),0),Ok(Tick(u64::MAX)));assert_eq!(due(Tick(u64::MAX),1),Err(LifecycleError::TimeOverflow));
        let mut w=World::default();let e=actor(&mut w);w.lifecycle.sequence=Some(u64::MAX);w.apply_damage(e,1).unwrap();
        let mut events=Vec::new();let r=w.resolve_lifecycle(1,&mut events).unwrap();assert_eq!(r.blocked,Some(LifecycleError::RevisionExhausted));assert_eq!(r.unscheduled,1);assert!(events.is_empty());assert_eq!(w.health(e).unwrap().current,0);
        w.unregister_lifecycle(e).unwrap();assert_eq!(w.lifecycle_activity().unscheduled,0);
    }
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum LifecyclePhase { Alive, NewlyDead, Corpse, Hidden, Ready }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct LifecycleState {pub phase:LifecyclePhase,pub death_tick:Option<Tick>,pub visible:bool}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct RespawnTicket {pub entity:EntityId,pub life:u64}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum LifecycleEventKind {
    /// Cleanup of the current dead life, not a replacement for ordered damage
    /// outcomes. Death+external-revive before this phase coalesces to Alive.
    DeathSettled {cancelled_windup:Option<AttackWindup>,cancelled_cast:Option<SkillEvent>},
    CorpseHidden,Removed,RespawnReady(RespawnTicket),
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct LifecycleEvent {pub entity:EntityId,pub scheduled_at:Tick,pub kind:LifecycleEventKind}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum LifecycleError {
    InvalidEntity,Unregistered,AlreadyRegistered,Capacity,Health(HealthError),
    InvalidWorkLimit,TimeOverflow,RevisionExhausted,Queue(TickQueueError),
    StaleTicket,ResetPending,AttributesPending,World(WorldError),
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct LifecycleReport {
    pub tick:Tick,pub visited:usize,pub processed:usize,pub admitted:usize,pub removed:usize,pub ready:usize,
    pub unscheduled:usize,pub overdue:Option<Tick>,pub blocked:Option<LifecycleError>,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct LifecycleActivity {pub registered:usize,pub queued:usize,pub unscheduled:usize,pub awaiting_placement:usize,pub next_due:Option<Tick>}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct RespawnOutcome {pub entity:EntityId,pub health:HealthOutcome,pub resource:Option<ResourceOutcome>}
struct Entry {
    entity:EntityId,row:usize,profile:LifecycleProfile,state:LifecycleState,life:Option<u64>,
    task:Option<TickTaskId>,pending:Option<NonZeroU32>,
}
pub(crate) struct LifecycleColumns {
    entries:Vec<Entry>,rows:Vec<Option<NonZeroU32>>,queue:TickQueue<EntityId>,
    pending:Vec<usize>,ready:usize,sequence:Option<u64>,
    batch:Vec<TickTaskId>,edits:Vec<TickEdit<EntityId>>,batching:bool,
    batch_tick:Option<Tick>,batch_cursor:usize,revision:u64,batch_revision:u64,
}
impl Default for LifecycleColumns {
    fn default()->Self{Self{entries:Vec::new(),rows:Vec::new(),queue:TickQueue::new(MAX_LIFECYCLE_ENTITIES).unwrap(),pending:Vec::new(),ready:0,sequence:Some(0),batch:Vec::new(),edits:Vec::new(),batching:false,batch_tick:None,batch_cursor:0,revision:0,batch_revision:0}}
}
fn alive()->LifecycleState{LifecycleState{phase:LifecyclePhase::Alive,death_tick:None,visible:true}}
impl LifecycleColumns {
    pub(crate) fn can_register(&self,count:usize)->bool{self.entries.len()+count<=MAX_LIFECYCLE_ENTITIES}
    pub(crate) fn spawn(&mut self){self.rows.push(None);}
    fn index(&self,row:usize)->Result<usize,LifecycleError>{self.rows[row].map(|v|(v.get()-1)as usize).ok_or(LifecycleError::Unregistered)}
    fn clear_pending(&mut self,index:usize){
        if let Some(slot)=self.entries[index].pending.take(){let slot=(slot.get()-1)as usize;self.pending.swap_remove(slot);
            if let Some(&moved)=self.pending.get(slot){self.entries[moved].pending=NonZeroU32::new((slot+1)as u32);}}
    }
    fn cancel(&mut self,index:usize){
        if let Some(task)=self.entries[index].task.take(){self.cancel_task(task);}
        self.clear_pending(index);
        if self.entries[index].state.phase==LifecyclePhase::Ready{self.ready-=1;}
    }
    fn invalidate_due(&mut self,due:Tick){if self.batch_tick.is_some_and(|tick|due<=tick){self.revision=self.revision.wrapping_add(1);}}
    fn cancel_task(&mut self,task:TickTaskId){if self.batching{self.edits.push(TickEdit::Cancel(task));}else{
        if self.batch_tick.is_some(){if let Some(task)=self.queue.get(task){self.invalidate_due(task.due);}}self.queue.cancel(task);
    }}
    fn reschedule_task(&mut self,task:TickTaskId,at:Tick,entity:EntityId){
        if self.batching{self.edits.push(TickEdit::Replace(task,at,entity));}else{
            if self.batch_tick.is_some(){self.invalidate_due(self.queue.get(task).unwrap().due);self.invalidate_due(at);}self.queue.reschedule(task,at).unwrap();
        }
    }
    fn flush_batch(&mut self){
        if self.batching{self.queue.apply_batch(&self.edits);self.edits.clear();self.batching=false;}
    }
    fn abandon_batch(&mut self){self.flush_batch();self.batch_tick=None;}
    fn admit(&mut self,index:usize)->Result<(),LifecycleError>{
        let e=&self.entries[index];if e.life.is_none(){return Err(LifecycleError::RevisionExhausted);}
        let at=e.state.death_tick.unwrap();let task=self.queue.schedule(at,e.entity).map_err(LifecycleError::Queue)?;self.invalidate_due(at);
        self.entries[index].task=Some(task);self.clear_pending(index);Ok(())
    }
    pub(crate) fn died(&mut self,row:usize,tick:Tick){
        let Ok(index)=self.index(row)else{return;};self.cancel(index);let e=&mut self.entries[index];
        self.sequence=self.sequence.and_then(|n|n.checked_add(1));e.life=self.sequence;
        e.state=LifecycleState{phase:LifecyclePhase::NewlyDead,death_tick:Some(tick),visible:true};
        if self.admit(index).is_err(){self.pending.push(index);self.entries[index].pending=NonZeroU32::new(self.pending.len()as u32);}
    }
    pub(crate) fn revived(&mut self,row:usize){
        let Ok(index)=self.index(row)else{return;};self.cancel(index);self.entries[index].state=alive();
    }
    fn remove(&mut self,row:usize){
        let Some(index)=self.rows[row].take()else{return;};let index=(index.get()-1)as usize;self.cancel(index);self.entries.swap_remove(index);
        if let Some(moved)=self.entries.get(index){self.rows[moved.row]=NonZeroU32::new((index+1)as u32);
            if let Some(slot)=moved.pending{self.pending[(slot.get()-1)as usize]=index;}}
    }
    pub(crate) fn despawn(&mut self,row:usize){
        self.remove(row);self.rows.swap_remove(row);
        if let Some(Some(index))=self.rows.get(row){self.entries[(index.get()-1)as usize].row=row;}
    }
}
fn due(death:Tick,delay:u32)->Result<Tick,LifecycleError>{death.0.checked_add(u64::from(delay)).map(Tick).ok_or(LifecycleError::TimeOverflow)}
impl World {
    pub fn register_lifecycle(&mut self,entity:EntityId,profile:LifecycleProfile)->Result<(),LifecycleError>{
        let row=self.dense_index(entity).map_err(|_|LifecycleError::InvalidEntity)?;let health=self.health.read(row).map_err(LifecycleError::Health)?;
        if self.lifecycle.rows[row].is_some(){return Err(LifecycleError::AlreadyRegistered);}
        if self.lifecycle.entries.len()==MAX_LIFECYCLE_ENTITIES{return Err(LifecycleError::Capacity);}
        self.lifecycle.entries.push(Entry{entity,row,profile,state:alive(),life:Some(0),task:None,pending:None});
        self.lifecycle.rows[row]=NonZeroU32::new(self.lifecycle.entries.len()as u32);
        if health.is_depleted(){self.lifecycle.died(row,self.tick());}Ok(())
    }
    pub fn unregister_lifecycle(&mut self,entity:EntityId)->Result<(),LifecycleError>{
        let row=self.dense_index(entity).map_err(|_|LifecycleError::InvalidEntity)?;self.lifecycle.index(row)?;self.lifecycle.remove(row);Ok(())
    }
    pub fn lifecycle_state(&self,entity:EntityId)->Result<LifecycleState,LifecycleError>{
        let row=self.dense_index(entity).map_err(|_|LifecycleError::InvalidEntity)?;Ok(self.lifecycle.entries[self.lifecycle.index(row)?].state)
    }
    pub fn lifecycle_activity(&self)->LifecycleActivity{
        let a=&self.lifecycle;LifecycleActivity{registered:a.entries.len(),queued:a.queue.len(),unscheduled:a.pending.len(),awaiting_placement:a.ready,
            next_due:if a.pending.is_empty(){a.queue.peek().map(|t|t.due)}else{Some(self.tick())}}
    }
    /// Call after life-changing combat/effects and before capture/next movement.
    /// Work/output is bounded; outstanding or blocked deadlines stay queued.
    /// Forward cancellation feedback; this phase does not discard temporary buffs.
    pub fn resolve_lifecycle(&mut self,limit:usize,output:&mut Vec<LifecycleEvent>)->Result<LifecycleReport,LifecycleError>{
        if limit>MAX_LIFECYCLE_WORK{return Err(LifecycleError::InvalidWorkLimit);}output.clear();output.reserve(limit);
        let now=self.tick();let mut r=LifecycleReport{tick:now,visited:0,processed:0,admitted:0,removed:0,ready:0,unscheduled:0,overdue:None,blocked:None};
        // Continue the already ordered cohort across small host slices. Only
        // same/earlier-deadline external changes invalidate it; app placement
        // retries in the future do not alter the remaining current cohort.
        let life=&mut self.lifecycle;
        if limit>=128&&life.batch_tick==Some(now)&&life.batch_revision==life.revision&&life.batch_cursor<life.batch.len(){life.batching=true;}
        else{
            life.batching=limit>=128&&life.queue.current_batch(now,life.queue.len(),&mut life.batch);
            life.batch_cursor=0;life.batch_tick=life.batching.then_some(now);life.batch_revision=life.revision;
        }
        // Service accepted due tasks first: a failed admission must not consume
        // every one-item slice and prevent the queue from ever freeing space.
        while r.visited<limit{
            let next=if self.lifecycle.batching{self.lifecycle.batch.get(self.lifecycle.batch_cursor).and_then(|id|self.lifecycle.queue.get(*id))}else{self.lifecycle.queue.peek()};
            let Some(task)=next.copied().filter(|t|t.due<=now)else{
                if self.lifecycle.batching{break;}
                let Some(&index)=self.lifecycle.pending.last()else{break;};r.visited+=1;
                if let Err(error)=self.lifecycle.admit(index){r.blocked=Some(error);break;}r.admitted+=1;continue;
            };
            r.visited+=1;
            let row=self.dense_index(task.value).expect("despawn cancels lifecycle");let index=self.lifecycle.index(row).unwrap();
            let e=&self.lifecycle.entries[index];let (entity,profile,state,life)=(e.entity,e.profile,e.state,e.life);
            let death=state.death_tick.expect("scheduled dead life");
            let kind=match state.phase {
                LifecyclePhase::NewlyDead=>{
                    let first=profile.respawn_after.map_or(profile.corpse_ticks,|delay|delay.min(profile.corpse_ticks));
                    let next=match due(death,first){Ok(at)=>at,Err(error)=>{r.blocked=Some(error);break;}};
                    // Same-tick rescheduling may precede the cohort's next
                    // member. Flush the prefix and resume the ordinary heap.
                    if self.lifecycle.batching&&next<=now{self.lifecycle.abandon_batch();continue;}
                    self.lifecycle.reschedule_task(task.id,next,entity);
                    let cancelled_windup=self.attack_state(entity).unwrap().windup;self.stop_attack(entity).unwrap();
                    self.halt_movement_at(row);let cancelled_cast=self.cancel_cast(entity).unwrap().map(|event|match event{
                        SkillEvent::Cancelled{cast,..}=>SkillEvent::Cancelled{cast,reason:crate::CastCancelReason::CasterDepleted},other=>other,
                    });
                    self.lifecycle.entries[index].state.phase=LifecyclePhase::Corpse;
                    LifecycleEventKind::DeathSettled{cancelled_windup,cancelled_cast}
                },
                LifecyclePhase::Corpse|LifecyclePhase::Hidden=>{
                    let respawn=match profile.respawn_after.map(|delay|due(death,delay)).transpose(){Ok(at)=>at,Err(error)=>{r.blocked=Some(error);break;}};
                    // Equal deadlines prefer respawn; otherwise preserve each
                    // original transition in heap order even during catch-up.
                    if state.phase==LifecyclePhase::Hidden||profile.respawn_after.is_some_and(|delay|delay<=profile.corpse_ticks){
                        let Some(life)=life else{r.blocked=Some(LifecycleError::RevisionExhausted);break;};
                        self.lifecycle.cancel_task(task.id);self.lifecycle.entries[index].task=None;
                        self.lifecycle.entries[index].state.phase=LifecyclePhase::Ready;self.lifecycle.ready+=1;r.ready+=1;
                        LifecycleEventKind::RespawnReady(RespawnTicket{entity,life})
                    }else if let Some(at)=respawn{
                        if self.lifecycle.batching&&at<=now{self.lifecycle.abandon_batch();continue;}
                        self.lifecycle.reschedule_task(task.id,at,entity);let e=&mut self.lifecycle.entries[index];e.state.phase=LifecyclePhase::Hidden;e.state.visible=false;
                        LifecycleEventKind::CorpseHidden
                    }else{
                        self.despawn(entity).expect("live lifecycle handle");r.removed+=1;LifecycleEventKind::Removed
                    }
                },
                LifecyclePhase::Alive|LifecyclePhase::Ready=>unreachable!("inactive lifecycle has no deadline"),
            };
            output.push(LifecycleEvent{entity,scheduled_at:task.due,kind});r.processed+=1;if self.lifecycle.batching{self.lifecycle.batch_cursor+=1;}
        }
        self.lifecycle.flush_batch();
        r.unscheduled=self.lifecycle.pending.len();r.overdue=self.lifecycle.queue.peek().map(|t|t.due).filter(|t|*t<=now);Ok(r)
    }
    /// Requeue a failed placement for an explicit future tick. Old tickets stop
    /// being ready immediately. The death/life identity is retained.
    pub fn defer_respawn(&mut self,ticket:RespawnTicket,delay:NonZeroU32)->Result<(),LifecycleError>{
        let index=self.ready_life(ticket)?;let at=due(self.tick(),delay.get())?;
        let task=self.lifecycle.queue.schedule(at,ticket.entity).map_err(LifecycleError::Queue)?;
        let e=&mut self.lifecycle.entries[index];e.task=Some(task);
        e.state.phase=if e.state.visible{LifecyclePhase::Corpse}else{LifecyclePhase::Hidden};self.lifecycle.ready-=1;Ok(())
    }
    fn ready_life(&self,ticket:RespawnTicket)->Result<usize,LifecycleError>{
        let row=self.dense_index(ticket.entity).map_err(|_|LifecycleError::StaleTicket)?;let index=self.lifecycle.index(row).map_err(|_|LifecycleError::StaleTicket)?;
        let e=&self.lifecycle.entries[index];if e.state.phase!=LifecyclePhase::Ready||e.life!=Some(ticket.life){return Err(LifecycleError::StaleTicket);}Ok(index)
    }
    /// Full-health NPC respawn after the app selects a legal placement. The app
    /// first cancels target effects/casts/attack and flushes derived attributes,
    /// retaining their existing feedback. Failed preflight changes nothing.
    /// Permanent attributes/learned skills and cast request dedup survive; health,
    /// resource, attack/skill cooldowns, AI threat/control and motion reset.
    pub fn complete_respawn(&mut self,ticket:RespawnTicket,position:GridPosition)->Result<RespawnOutcome,LifecycleError>{
        let index=self.ready_life(ticket)?;let row=self.lifecycle.entries[index].row;let entity=ticket.entity;
        let health=self.health.read(row).map_err(LifecycleError::Health)?;
        if !health.is_depleted(){return Err(LifecycleError::StaleTicket);}
        if self.effects.has_target_effects(row)||self.skills.has_pending(row)||self.attack_state(entity).unwrap().intent.is_some(){return Err(LifecycleError::ResetPending);}
        if matches!(self.combat_attributes(entity),Err(crate::AttributeError::Dirty)){return Err(LifecycleError::AttributesPending);}
        self.set_position(entity,position).map_err(LifecycleError::World)?;
        let outcome=self.health.apply(row,HealthCommand::Revive{entity,current:health.maximum}).expect("checked dead maximum");
        let resource=self.resource.read(row).ok().map(|_|self.resource_at(row,ResourceCommand::Restore(u32::MAX)).unwrap());
        self.combat.reset_life(row);self.skills.reset_recovery(row);self.ai.reset_life(row);self.lifecycle.revived(row);
        Ok(RespawnOutcome{entity,health:outcome,resource})
    }
}
