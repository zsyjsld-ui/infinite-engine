//! Trusted skill definitions, atomic cast admission and tick-driven readiness.
use std::num::NonZeroU32;
use crate::{EntityId,GridPosition,HealthError,ResourceCommand,ResourceError,ResourceOutcome,
    Tick,TickQueue,TickQueueError,TickTaskId,World};
pub const MAX_SKILLS_PER_ENTITY:usize=32;
pub const MAX_RECOVERY_GROUPS:usize=16;
pub const MAX_ACTIVE_CASTS:usize=4096;
#[derive(Clone,Copy,Debug,PartialEq,Eq,PartialOrd,Ord,Hash)]
pub struct SkillId(pub u16);
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SkillRecovery { pub group:u8, pub ticks:u16, pub ingress_grace_ticks:u16 }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SkillSpec {
    pub id:SkillId, pub resource_cost:u32, pub cooldown_ticks:u16, pub windup_ticks:u16,
    pub recovery:Option<SkillRecovery>,
    /// Explicit ingress tolerance, <= cooldown. A grace acceptance advances from
    /// the old deadline, so repeated early requests cannot accelerate the rate.
    pub ingress_grace_ticks:u16,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum CastTarget { Entity(EntityId), Point(GridPosition) }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CastRequest { pub caster:EntityId, pub request_id:u64, pub skill:SkillId, pub target:CastTarget }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CastTicket { pub request:CastRequest, pub spec:SkillSpec, pub started_at:Tick, pub ready_at:Tick }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CastReceipt { pub cast:CastTicket, pub resource:Option<ResourceOutcome>, pub cooldown_ready:Tick, pub recovery_ready:Tick }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CastAdmission { pub receipt:CastReceipt, pub duplicate:bool }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum CastCancelReason { Manual, CasterUnavailable, CasterDepleted, TargetGone }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum SkillEvent { Ready(CastTicket), Cancelled { cast:CastTicket, reason:CastCancelReason } }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum SkillError {
    InvalidEntity,InvalidTarget,InvalidSpec,UnknownSkill,SkillCapacity,Busy,CoolingDown,
    Recovering,Depleted,StaleRequest,RequestConflict,TimeOverflow,InvalidWorkLimit,
    Health(HealthError),Resource(ResourceError),Queue(TickQueueError),
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SkillState { pub cooldown_ready:Tick, pub recovery_ready:Tick, pub pending:Option<CastTicket> }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SkillReport { pub tick:Tick, pub processed:usize, pub ready:usize, pub cancelled:usize, pub overdue:Option<Tick> }
#[derive(Clone,Copy)]
struct LearnedSkill { spec:SkillSpec, ready:Tick, grace:u16 }
struct SkillActor { row:usize, learned:Vec<LearnedSkill>, recovery:[Tick;MAX_RECOVERY_GROUPS],
    recovery_grace:[u16;MAX_RECOVERY_GROUPS], pending:Option<TickTaskId>, last:Option<CastReceipt> }
pub(crate) struct SkillColumns { rows:Vec<Option<NonZeroU32>>, actors:Vec<SkillActor>, queue:TickQueue<CastTicket> }
impl Default for SkillColumns {
    fn default()->Self{Self{rows:Vec::new(),actors:Vec::new(),queue:TickQueue::new(MAX_ACTIVE_CASTS).unwrap()}}
}
impl SkillColumns {
    pub(crate) fn next_due(&self)->Option<Tick>{self.queue.peek().map(|t|t.due)}
    pub(crate) fn spawn(&mut self){self.rows.push(None);}
    pub(crate) fn has_pending(&self,row:usize)->bool{self.rows[row].is_some_and(|i|self.actors[(i.get()-1)as usize].pending.is_some())}
    pub(crate) fn reset_recovery(&mut self,row:usize){
        if let Some(index)=self.rows[row]{let actor=&mut self.actors[(index.get()-1)as usize];debug_assert!(actor.pending.is_none());
            for skill in &mut actor.learned{skill.ready=Tick(0);skill.grace=0;}actor.recovery=[Tick(0);MAX_RECOVERY_GROUPS];actor.recovery_grace=[0;MAX_RECOVERY_GROUPS];}
    }
    pub(crate) fn despawn(&mut self,row:usize){
        if let Some(index)=self.rows[row].take(){
            let index=(index.get()-1)as usize;let actor=self.actors.swap_remove(index);
            if let Some(pending)=actor.pending{self.queue.cancel(pending);}
            if let Some(moved)=self.actors.get(index){self.rows[moved.row]=NonZeroU32::new((index+1)as u32);}
        }
        self.rows.swap_remove(row);
        if let Some(Some(index))=self.rows.get(row){self.actors[(index.get()-1)as usize].row=row;}
    }
    fn actor(&self,row:usize)->Result<usize,SkillError>{self.rows[row].map(|i|(i.get()-1)as usize).ok_or(SkillError::UnknownSkill)}
}
fn ready(now:Tick,due:Tick,grace:u16)->bool{now>=due||due.0-now.0<=u64::from(grace)}
fn next(now:Tick,previous:Tick,duration:u16)->Result<Tick,SkillError>{
    // Called only after readiness: a future deadline can only be inside grace.
    now.max(previous).0.checked_add(u64::from(duration)).map(Tick).ok_or(SkillError::TimeOverflow)
}
impl World {
    /// Trusted registration, never supplied by a client cast packet. Updating a
    /// definition preserves its existing cooldown and already accepted tickets.
    pub fn learn_skill(&mut self,entity:EntityId,spec:SkillSpec)->Result<(),SkillError>{
        let row=self.dense_index(entity).map_err(|_|SkillError::InvalidEntity)?;
        if spec.ingress_grace_ticks>spec.cooldown_ticks||spec.recovery.is_some_and(|r|usize::from(r.group)>=MAX_RECOVERY_GROUPS||r.ingress_grace_ticks>r.ticks){return Err(SkillError::InvalidSpec);}
        let index=match self.skills.rows[row]{Some(i)=>(i.get()-1)as usize,None=>{
            let index=self.skills.actors.len();self.skills.actors.push(SkillActor{row,learned:Vec::new(),recovery:[Tick(0);MAX_RECOVERY_GROUPS],recovery_grace:[0;MAX_RECOVERY_GROUPS],pending:None,last:None});self.skills.rows[row]=NonZeroU32::new((index+1)as u32);index
        }};
        let learned=&mut self.skills.actors[index].learned;
        match learned.binary_search_by_key(&spec.id,|s|s.spec.id){
            Ok(i)=>learned[i].spec=spec,
            Err(i)=>{if learned.len()==MAX_SKILLS_PER_ENTITY{return Err(SkillError::SkillCapacity);}learned.insert(i,LearnedSkill{spec,ready:Tick(0),grace:spec.ingress_grace_ticks});},
        }
        Ok(())
    }
    pub fn skill_state(&self,entity:EntityId,skill:SkillId)->Result<SkillState,SkillError>{
        let row=self.dense_index(entity).map_err(|_|SkillError::InvalidEntity)?;let actor=&self.skills.actors[self.skills.actor(row)?];
        let index=actor.learned.binary_search_by_key(&skill,|s|s.spec.id).map_err(|_|SkillError::UnknownSkill)?;let slot=actor.learned[index];
        Ok(SkillState{cooldown_ready:slot.ready,recovery_ready:slot.spec.recovery.map_or(Tick(0),|r|actor.recovery[r.group as usize]),pending:actor.pending.and_then(|id|self.skills.queue.get(id).map(|t|t.value))})
    }
    /// Successful IDs increase per caster. The latest accepted request can be
    /// retried identically without spending or emitting Ready twice. Older IDs
    /// and reuse with different contents fail; rejected IDs are not consumed.
    pub fn start_cast(&mut self,request:CastRequest)->Result<CastAdmission,SkillError>{
        let row=self.dense_index(request.caster).map_err(|_|SkillError::InvalidEntity)?;
        let index=self.skills.actor(row)?;let actor=&self.skills.actors[index];
        if let Some(last)=actor.last{
            if request.request_id<last.cast.request.request_id{return Err(SkillError::StaleRequest);}
            if request.request_id==last.cast.request.request_id{
                if request!=last.cast.request{return Err(SkillError::RequestConflict);}
                return Ok(CastAdmission{receipt:last,duplicate:true});
            }
        }
        let slot_index=actor.learned.binary_search_by_key(&request.skill,|s|s.spec.id).map_err(|_|SkillError::UnknownSkill)?;
        let slot=actor.learned[slot_index];let spec=slot.spec;let now=self.tick();
        if actor.pending.is_some(){return Err(SkillError::Busy);}
        if self.health.read(row).map_err(SkillError::Health)?.is_depleted(){return Err(SkillError::Depleted);}
        if let CastTarget::Entity(target)=request.target{self.dense_index(target).map_err(|_|SkillError::InvalidTarget)?;}
        if !ready(now,slot.ready,slot.grace){return Err(SkillError::CoolingDown);}
        let recovery_ready=if let Some(r)=spec.recovery{
            let previous=actor.recovery[r.group as usize];if !ready(now,previous,actor.recovery_grace[r.group as usize]){return Err(SkillError::Recovering);}next(now,previous,r.ticks)?
        }else{Tick(0)};
        let cooldown_ready=next(now,slot.ready,spec.cooldown_ticks)?;
        let ready_at=Tick(now.0.checked_add(u64::from(spec.windup_ticks)).ok_or(SkillError::TimeOverflow)?);
        if spec.resource_cost>0&&self.resource.read(row).map_err(SkillError::Resource)?.current<spec.resource_cost{return Err(SkillError::Resource(ResourceError::Insufficient));}
        let cast=CastTicket{request,spec,started_at:now,ready_at};
        let task=self.skills.queue.schedule(ready_at,cast).map_err(SkillError::Queue)?;
        // All fallible admission checks precede resource/cooldown writes.
        let resource=if spec.resource_cost==0{None}else{Some(self.resource_at(row,ResourceCommand::Spend(spec.resource_cost)).expect("preflighted resource"))};
        let actor=&mut self.skills.actors[index];actor.pending=Some(task);
        actor.learned[slot_index].ready=cooldown_ready;actor.learned[slot_index].grace=spec.ingress_grace_ticks;
        if let Some(r)=spec.recovery{actor.recovery[r.group as usize]=recovery_ready;actor.recovery_grace[r.group as usize]=r.ingress_grace_ticks;}
        let receipt=CastReceipt{cast,resource,cooldown_ready,recovery_ready};actor.last=Some(receipt);
        Ok(CastAdmission{receipt,duplicate:false})
    }
    /// Explicit cancellation. Accepted resource/cooldown remain spent. Despawn
    /// removes pending work directly; call this first if a cancel event is needed.
    pub fn cancel_cast(&mut self,entity:EntityId)->Result<Option<SkillEvent>,SkillError>{
        let row=self.dense_index(entity).map_err(|_|SkillError::InvalidEntity)?;
        let Some(index)=self.skills.rows[row]else{return Ok(None);};let actor=&mut self.skills.actors[(index.get()-1)as usize];
        let Some(task)=actor.pending.take()else{return Ok(None);};
        Ok(self.skills.queue.cancel(task).map(|t|SkillEvent::Cancelled{cast:t.value,reason:CastCancelReason::Manual}))
    }
    /// Due tasks only, in (ready tick, acceptance order). Ready transfers exactly
    /// once to the application for range damage/projectile/effect dispatch. It is
    /// not proof of a hit: target eligibility and hit geometry still apply there.
    /// A budget shortfall leaves deadlines intact and explicitly reports overdue;
    /// drain before advancing authoritative work, don't silently drop callbacks.
    pub fn resolve_casts(&mut self,limit:usize,output:&mut Vec<SkillEvent>)->Result<SkillReport,SkillError>{
        if limit>MAX_ACTIVE_CASTS{return Err(SkillError::InvalidWorkLimit);}
        output.clear();output.reserve(limit.min(self.skills.queue.len()));let now=self.tick();let mut report=SkillReport{tick:now,processed:0,ready:0,cancelled:0,overdue:None};
        while report.processed<limit{
            let Some(task)=self.skills.queue.pop_due(now)else{break;};report.processed+=1;let cast=task.value;
            let reason=match self.dense_index(cast.request.caster){
                Err(_)=>Some(CastCancelReason::CasterUnavailable),
                Ok(row)=>{
                    if let Some(index)=self.skills.rows[row]{self.skills.actors[(index.get()-1)as usize].pending=None;}
                    match self.health.read(row){
                        Err(_)=>Some(CastCancelReason::CasterUnavailable),
                        Ok(h)if h.is_depleted()=>Some(CastCancelReason::CasterDepleted),
                        Ok(_)=>if let CastTarget::Entity(target)=cast.request.target{self.dense_index(target).err().map(|_|CastCancelReason::TargetGone)}else{None},
                    }
                },
            };
            if let Some(reason)=reason{report.cancelled+=1;output.push(SkillEvent::Cancelled{cast,reason});}else{report.ready+=1;output.push(SkillEvent::Ready(cast));}
        }
        report.overdue=self.skills.queue.peek().map(|t|t.due).filter(|t|*t<=now);Ok(report)
    }
    pub fn active_casts(&self)->usize{self.skills.queue.len()}
}
