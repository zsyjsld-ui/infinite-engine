//! Autonomous target selection and threat locks for one map/world.
use std::num::{NonZeroU16, NonZeroU32};
mod activation;
use activation::{Activation, SleepSlot};
use crate::{AttackError, AttackIntent, AttackProfile, AttackWindup, ChaseConfig, ChaseStatus, CombatQueryBudget,
    CombatQueryError, CombatQueryWork, EntityId, GridPosition, HealthError, SpatialFrame,
    SyncError, Tick, ViewRect, World, within_attack_range};

pub const MAX_AI_ACTORS: usize = 65_536;
pub const MAX_AI_TARGETS: usize = 4096;
pub const MAX_AI_THREATS: usize = 4096;
pub const MAX_AI_TOTAL_THREATS: usize = 262_144;
pub const MAX_AI_WORK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiProfile {
    pub aggro_radius: u32,
    /// Inclusive leash boundary; only armed after entering the original circle.
    pub leash_radius: u32,
    /// Extra acquisition range only for a target with positive unexpired bonus.
    pub bonus_radius: u32,
    pub attack: AttackProfile,
    pub attack_break_range: u32,
    pub chase: ChaseConfig,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiMode { Idle, Chasing, Attacking, Dead }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiState {
    pub mode: AiMode, pub target: Option<EntityId>,
    pub entered_aggro: bool, pub controlled: bool,
}
impl Default for AiState {
    fn default() -> Self { Self { mode: AiMode::Idle, target: None, entered_aggro: false, controlled: false } }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiError {
    InvalidEntity, UnregisteredActor, UnregisteredTarget, AlreadyRegistered,
    InvalidProfile, ActorCapacity, TargetCapacity, ThreatCapacity, SequenceExhausted,
    SelfTarget, Health(HealthError), InvalidWorkLimit, Spatial(SyncError),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiThreat {
    pub target: EntityId, pub damage: u64, pub bonus: u64, pub bonus_until: Tick,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiReason { Acquired, Switched, TargetLost, Leash, Mode, Control, Dead, AttackRejected(AttackError) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiEvent {
    pub actor: EntityId, pub before: AiState, pub after: AiState, pub reason: AiReason,
    /// Forward as CombatEvent::WindupCancelled if an intent change canceled it.
    pub cancelled_windup: Option<AttackWindup>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiReport {
    pub tick: Tick, pub visited: usize, pub processed: usize, pub remaining: usize,
    pub rejected: usize, pub deferred: Option<CombatQueryError>, pub work: CombatQueryWork,
    /// Dirty target jobs share `limit` with actor visits. Remaining includes both.
    pub wake_jobs: usize, pub woken: usize, pub pending_wakes: usize, pub sleeping: usize,
}
struct Actor {
    entity: EntityId, row: usize, profile: AiProfile, state: AiState,
    threats: Vec<AiThreat>, damage_wake: bool, control_until: Tick, last_tick: Option<Tick>, last_targets: u64,
    position: GridPosition, active: Option<NonZeroU32>, sleeper: Option<SleepSlot>,
}
#[derive(Clone, Copy)]
struct Target { entity: EntityId, row: usize, order: u64, eligible: bool,
    position: GridPosition, dirty: Option<NonZeroU16>, revision: u64 }
#[derive(Default)]
pub(crate) struct AiColumns {
    actors: Vec<Actor>, actor_rows: Vec<Option<NonZeroU32>>,
    targets: Vec<Target>, target_rows: Vec<Option<NonZeroU16>>, sequence: u64,
    cursor: usize, tick: Option<Tick>, remaining: usize, target_revision: u64, resolved_targets: u64, threat_count: usize,
    activation: Activation,
}
impl AiColumns {
    pub(crate) fn activity(&self)->(bool,bool){(!self.activation.active.is_empty(),!self.activation.dirty.is_empty())}
    pub(crate) fn preflight_registration(&self,count:usize,profile:AiProfile)->Result<(),AiError>{
        if profile.leash_radius<profile.aggro_radius||profile.attack_break_range<profile.attack.range{return Err(AiError::InvalidProfile);}
        if self.actors.len()+count>MAX_AI_ACTORS{return Err(AiError::ActorCapacity);}Ok(())
    }
    pub(crate) fn spawn(&mut self) { self.actor_rows.push(None); self.target_rows.push(None); }
    pub(crate) fn reset_life(&mut self,row:usize){
        self.died(row);if let Some(index)=self.actor_rows[row]{let actor=&mut self.actors[(index.get()-1)as usize];actor.control_until=Tick(0);actor.state=AiState::default();}self.revived(row);
    }
    pub(crate) fn died(&mut self, row: usize) {
        if let Some(index)=self.target_rows[row]{self.mark_target_dirty(usize::from(index.get()-1));}
        if let Some(index)=self.actor_rows[row]{let index=(index.get()-1)as usize;let actor=&mut self.actors[index];
            self.threat_count-=actor.threats.len();actor.threats.clear();actor.damage_wake=false;actor.state=AiState{mode:AiMode::Dead,..AiState::default()};self.wake_actor(index);}
    }
    pub(crate) fn revived(&mut self,row:usize){
        if let Some(index)=self.target_rows[row]{self.mark_target_dirty(usize::from(index.get()-1));}
        if let Some(index)=self.actor_rows[row]{self.wake_actor((index.get()-1)as usize);}
    }
    fn remove_actor(&mut self, row: usize) {
        if let Some(index) = self.actor_rows[row].take() {
            let index = (index.get()-1) as usize; self.remove_sleep(index);self.remove_active(index);
            let removed=self.actors.swap_remove(index);self.threat_count-=removed.threats.len();
            if let Some(actor) = self.actors.get(index) { self.actor_rows[actor.row] = NonZeroU32::new((index+1) as u32); }
            self.remap_actor(index);self.remaining = self.activation.active.len();
        }
    }
    fn remove_target(&mut self, row: usize) {
        if let Some(index) = self.target_rows[row].take() {
            let index = usize::from(index.get()-1); self.remove_dirty(index);self.targets.swap_remove(index);self.remap_target(index);
            if let Some(target) = self.targets.get(index) { self.target_rows[target.row] = NonZeroU16::new((index+1) as u16); }
            self.target_revision=self.target_revision.wrapping_add(1);
        }
    }
    pub(crate) fn despawn(&mut self, row: usize) {
        self.remove_actor(row); self.remove_target(row);
        self.actor_rows.swap_remove(row); self.target_rows.swap_remove(row);
        if let Some(Some(index)) = self.actor_rows.get(row) { self.actors[(index.get()-1) as usize].row = row; }
        if let Some(Some(index)) = self.target_rows.get(row) { self.targets[usize::from(index.get()-1)].row = row; }
    }
    fn actor_index(&self, row: usize) -> Result<usize, AiError> {
        self.actor_rows[row].map(|i| (i.get()-1) as usize).ok_or(AiError::UnregisteredActor)
    }
}
fn key(entity: EntityId) -> (u32,u32) { (entity.index,entity.generation) }
fn distance(a: GridPosition, b: GridPosition) -> u128 {
    let dx = i128::from(a.x)-i128::from(b.x); let dy = i128::from(a.y)-i128::from(b.y);
    (dx*dx+dy*dy) as u128
}
fn bounds(center: GridPosition, radius: u32) -> ViewRect {
    let clip = |n:i64| n.clamp(i64::from(i32::MIN),i64::from(i32::MAX)) as i32;
    ViewRect { min:GridPosition { x:clip(i64::from(center.x)-i64::from(radius)), y:clip(i64::from(center.y)-i64::from(radius)) },
        max:GridPosition { x:clip(i64::from(center.x)+i64::from(radius)), y:clip(i64::from(center.y)+i64::from(radius)) } }
}
impl World {
    pub fn register_ai_actor(&mut self, entity: EntityId, profile: AiProfile) -> Result<(),AiError> {
        let row = self.dense_index(entity).map_err(|_|AiError::InvalidEntity)?;
        self.health.read(row).map_err(AiError::Health)?;
        if profile.leash_radius < profile.aggro_radius || profile.attack_break_range < profile.attack.range { return Err(AiError::InvalidProfile); }
        if self.ai.actor_rows[row].is_some() { return Err(AiError::AlreadyRegistered); }
        if self.ai.actors.len() == MAX_AI_ACTORS { return Err(AiError::ActorCapacity); }
        self.ai.actors.push(Actor { entity,row,profile,state:AiState::default(),threats:Vec::new(),damage_wake:false,control_until:Tick(0),last_tick:None,last_targets:0,
            position:self.position_at(row),active:None,sleeper:None });
        self.ai.actor_rows[row] = NonZeroU32::new(self.ai.actors.len() as u32); self.ai.wake_actor(self.ai.actors.len()-1); Ok(())
    }
    /// AI owns this actor's combat intent while registered. Unregister before
    /// applying manual combat control. Returned windup belongs to cancellation feedback.
    pub fn unregister_ai_actor(&mut self, entity: EntityId) -> Result<Option<AttackWindup>,AiError> {
        let row = self.dense_index(entity).map_err(|_|AiError::InvalidEntity)?; self.ai.actor_index(row)?;
        let windup = self.attack_state(entity).unwrap().windup; self.stop_attack(entity).unwrap(); self.ai.remove_actor(row); Ok(windup)
    }
    /// One World is one map. App eligibility covers ghost/loading/faction/etc.;
    /// current HP and generation are always revalidated by the core.
    pub fn register_ai_target(&mut self, entity: EntityId, eligible: bool) -> Result<(),AiError> {
        let row = self.dense_index(entity).map_err(|_|AiError::InvalidEntity)?; self.health.read(row).map_err(AiError::Health)?;
        if self.ai.target_rows[row].is_some() { return Err(AiError::AlreadyRegistered); }
        if self.ai.targets.len() == MAX_AI_TARGETS { return Err(AiError::TargetCapacity); }
        let order = self.ai.sequence.checked_add(1).ok_or(AiError::SequenceExhausted)?;
        self.ai.targets.push(Target { entity,row,order,eligible,position:self.position_at(row),dirty:None,revision:0 }); self.ai.sequence = order;
        self.ai.target_rows[row] = NonZeroU16::new(self.ai.targets.len() as u16); self.ai.mark_target_dirty(self.ai.targets.len()-1); Ok(())
    }
    pub fn set_ai_target_eligible(&mut self, entity: EntityId, eligible: bool) -> Result<(),AiError> {
        let row = self.dense_index(entity).map_err(|_|AiError::InvalidEntity)?;
        let index = self.ai.target_rows[row].ok_or(AiError::UnregisteredTarget)?;
        let index=usize::from(index.get()-1);let target=&mut self.ai.targets[index];if target.eligible!=eligible{target.eligible=eligible;self.ai.mark_target_dirty(index);}Ok(())
    }
    pub fn unregister_ai_target(&mut self, entity: EntityId) -> Result<(),AiError> {
        let row = self.dense_index(entity).map_err(|_|AiError::InvalidEntity)?;
        self.ai.target_rows[row].ok_or(AiError::UnregisteredTarget)?; self.ai.remove_target(row); Ok(())
    }
    pub fn ai_state(&self, entity: EntityId) -> Result<AiState,AiError> {
        let row = self.dense_index(entity).map_err(|_|AiError::InvalidEntity)?; Ok(self.ai.actors[self.ai.actor_index(row)?].state)
    }
    pub fn set_ai_control_until(&mut self, entity: EntityId, until: Tick) -> Result<(),AiError> {
        let row = self.dense_index(entity).map_err(|_|AiError::InvalidEntity)?; let index = self.ai.actor_index(row)?;
        self.ai.actors[index].control_until = until; self.ai.wake_actor(index); Ok(())
    }
    /// Pass actual HP loss, not attempted or shield-absorbed damage. The caller
    /// retains authoritative hit feedback if this bounded admission is rejected.
    pub fn add_ai_damage_threat(&mut self, actor: EntityId, target: EntityId, damage: u32) -> Result<(),AiError> {
        self.edit_ai_threat(actor,target,damage,None)
    }
    /// Replace an application's already accumulated aura contribution. Querying
    /// does not increase it. An expired/zero contribution never extends acquisition.
    pub fn set_ai_bonus_threat(&mut self, actor: EntityId, target: EntityId, amount: u64, until: Tick) -> Result<(),AiError> {
        self.edit_ai_threat(actor,target,0,Some((amount,until)))
    }
    fn edit_ai_threat(&mut self, actor: EntityId, target: EntityId, damage: u32, bonus: Option<(u64,Tick)>) -> Result<(),AiError> {
        if actor == target { return Err(AiError::SelfTarget); }
        let row = self.dense_index(actor).map_err(|_|AiError::InvalidEntity)?; let index = self.ai.actor_index(row)?;
        let target_row = self.dense_index(target).map_err(|_|AiError::InvalidEntity)?;
        self.ai.target_rows[target_row].ok_or(AiError::UnregisteredTarget)?;
        if damage == 0 && bonus.is_none() { return Ok(()); }
        // Existing-source updates need only logarithmic lookup. Reclaim stale
        // generations when admitting a new entry at a capacity boundary.
        let mut search=self.ai.actors[index].threats.binary_search_by_key(&key(target),|e|key(e.target));
        if search.is_err()&&(self.ai.actors[index].threats.len()==MAX_AI_THREATS||self.ai.threat_count==MAX_AI_TOTAL_THREATS){self.prune_ai_threats(actor)?;search=self.ai.actors[index].threats.binary_search_by_key(&key(target),|e|key(e.target));}
        let entries=&mut self.ai.actors[index].threats;
        let at = match search {
            Ok(at) => at,
            Err(at) => { if entries.len() == MAX_AI_THREATS||self.ai.threat_count==MAX_AI_TOTAL_THREATS {return Err(AiError::ThreatCapacity);}
                entries.insert(at,AiThreat{target,damage:0,bonus:0,bonus_until:Tick(0)});self.ai.threat_count+=1;at },
        };
        entries[at].damage = entries[at].damage.saturating_add(u64::from(damage));
        if let Some((amount,until)) = bonus { entries[at].bonus=amount; entries[at].bonus_until=until; }
        let agent = &mut self.ai.actors[index]; agent.damage_wake |= damage!=0; agent.last_tick=None;
        self.ai.wake_actor(index); Ok(())
    }
    pub fn ai_threats(&self, actor: EntityId) -> Result<&[AiThreat],AiError> {
        let row=self.dense_index(actor).map_err(|_|AiError::InvalidEntity)?;Ok(&self.ai.actors[self.ai.actor_index(row)?].threats)
    }
    pub fn ai_total_threats(&self)->usize{self.ai.threat_count}
    /// Explicit bounded housekeeping. Dead/ineligible registered targets retain
    /// their contribution until this monster dies; departed generations do not.
    pub fn prune_ai_threats(&mut self,actor:EntityId)->Result<usize,AiError>{
        let row=self.dense_index(actor).map_err(|_|AiError::InvalidEntity)?;let index=self.ai.actor_index(row)?;
        let mut entries=std::mem::take(&mut self.ai.actors[index].threats);let before=entries.len();
        entries.retain(|e|self.dense_index(e.target).ok().is_some_and(|r|self.ai.target_rows[r].is_some()));
        let removed=before-entries.len();self.ai.threat_count-=removed;self.ai.actors[index].threats=entries;Ok(removed)
    }
    fn ai_target(&self, entity: EntityId) -> Option<Target> {
        let row=self.dense_index(entity).ok()?;let index=self.ai.target_rows[row]?;let target=self.ai.targets[usize::from(index.get()-1)];
        (target.eligible&&!self.health.read(row).ok()?.is_depleted()).then_some(target)
    }
}

/// Own one reusable spatial capture. Decisions use live health/eligibility and
/// immutable positions for this call; no app callbacks run during a decision.
#[derive(Clone,Copy)]
struct PreparedTarget { entity: EntityId, position: GridPosition, order: u64 }
pub struct AiSystem { frame: SpatialFrame, cell_size: NonZeroU32, target_bounds: Option<ViewRect>, targets:Vec<PreparedTarget>, target_rows:Vec<Option<NonZeroU16>> }
impl AiSystem {
    pub fn new(cell_size:NonZeroU32)->Self { Self { frame:SpatialFrame::new(cell_size), cell_size, target_bounds:None,targets:Vec::new(),target_rows:Vec::new() } }
    fn target(&self,world:&World,entity:EntityId)->Option<PreparedTarget>{
        let row=world.dense_index(entity).ok()?;let registered=world.ai.target_rows[row]?;
        let prepared=self.target_rows[usize::from(registered.get()-1)]?;Some(self.targets[usize::from(prepared.get()-1)])
    }
    fn nearest(&self, world:&World, actor:EntityId, origin:GridPosition, radius:Option<u32>, budget:&mut CombatQueryBudget)->Result<Option<EntityId>,CombatQueryError> {
        let Some(target_bounds)=self.target_bounds else{return Ok(None);};
        if let Some(radius)=radius{let area=bounds(origin,radius);if area.max.x<target_bounds.min.x||area.min.x>target_bounds.max.x||area.max.y<target_bounds.min.y||area.min.y>target_bounds.max.y{return Ok(None);}}
        let mut best:Option<(u128,u64,EntityId)>=None;
        let mut consider=|target:PreparedTarget| {
            if target.entity==actor{return;}
            let d=distance(origin,target.position);if radius.is_some_and(|r|d>u128::from(r)*u128::from(r)){return;}
            if best.is_none_or(|(old,order,_)|(d,target.order)<(old,order)){best=Some((d,target.order,target.entity));}
        };
        let wide=radius.is_some_and(|r|{let width=u128::from(r)*2/u128::from(self.cell_size.get())+2;width*width>self.targets.len()as u128});
        if self.targets.len()<8||radius.is_none()||wide{
            for &target in &self.targets {budget.candidate()?;consider(target);}
        }else{
            self.frame.visit_combat_candidates(bounds(origin,radius.unwrap()),budget,|motion,_|{
                if let Some(target)=self.target(world,motion.entity){consider(target);}Ok(())
            })?;
        }
        Ok(best.map(|(_,_,entity)|entity))
    }
    fn threat_target(&self,world:&World,index:usize,origin:GridPosition,budget:&mut CombatQueryBudget)->Result<Option<EntityId>,CombatQueryError>{
        let actor=&world.ai.actors[index];let mut best:Option<(u64,u64,EntityId)>=None;
        for entry in &actor.threats {
            budget.candidate()?;let Some(target)=self.target(world,entry.target)else{continue;};
            let bonus=if entry.bonus_until>world.tick(){entry.bonus}else{0};let total=entry.damage.saturating_add(bonus);if total==0{continue;}
            let position=target.position;
            if !actor.damage_wake&&!within_attack_range(origin,position,actor.profile.aggro_radius)&&!(bonus>0&&within_attack_range(origin,position,actor.profile.bonus_radius)){continue;}
            let replace=best.is_none_or(|(amount,order,entity)|total>amount||(total==amount&&
                (Some(target.entity)==actor.state.target||(Some(entity)!=actor.state.target&&target.order<order))));
            if replace{best=Some((total,target.order,target.entity));}
        }Ok(best.map(|(_,_,entity)|entity))
    }
    pub fn resolve(&mut self,world:&mut World,limit:usize,budget:&mut CombatQueryBudget,events:&mut Vec<AiEvent>)->Result<AiReport,AiError>{
        if limit>MAX_AI_WORK{return Err(AiError::InvalidWorkLimit);}events.clear();events.reserve(limit);
        let tick=world.tick();if world.ai.tick!=Some(tick)||world.ai.resolved_targets!=world.ai.target_revision{world.ai.tick=Some(tick);world.ai.resolved_targets=world.ai.target_revision;world.ai.remaining=world.ai.activation.active.len();}
        let old_work=budget.work();let mut report=AiReport{tick,visited:0,processed:0,remaining:world.ai.remaining,rejected:0,deferred:None,work:CombatQueryWork::default(),wake_jobs:0,woken:0,pending_wakes:0,sleeping:0};
        let mut prepared=false;
        loop {
        // Validate already-running intents before spending the slice on optional
        // wake work. A slow wake queue must not postpone a stale windup cancel.
        if world.ai.remaining==0{world.ai.wake_targets(&world.health,limit-report.visited,budget,&mut report);}
        if report.deferred.is_some()||report.visited+report.wake_jobs==limit||world.ai.remaining==0{break;}
        if !prepared {
        self.target_bounds=None;self.targets.clear();self.target_rows.clear();self.target_rows.resize(world.ai.targets.len(),None);
        for (index,target) in world.ai.targets.iter().enumerate(){if target.eligible&&world.health.read(target.row).is_ok_and(|h|!h.is_depleted()){
            let point=world.position_at(target.row);
            self.targets.push(PreparedTarget{entity:target.entity,position:point,order:target.order});self.target_rows[index]=NonZeroU16::new(self.targets.len()as u16);
            self.target_bounds=Some(match self.target_bounds {
                None=>ViewRect{min:point,max:point},
                Some(rect)=>ViewRect {
                    min:GridPosition{x:rect.min.x.min(point.x),y:rect.min.y.min(point.y)},
                    max:GridPosition{x:rect.max.x.max(point.x),y:rect.max.y.max(point.y)},
                },
            });
        }}
        self.frame.capture(world).map_err(AiError::Spatial)?;
        prepared=true;
        }
        while report.visited+report.wake_jobs<limit&&world.ai.remaining>0&&!world.ai.activation.active.is_empty(){
            world.ai.cursor%=world.ai.activation.active.len();let index=world.ai.activation.active[world.ai.cursor];
            if world.ai.actors[index].last_tick==Some(tick)&&world.ai.actors[index].last_targets==world.ai.target_revision{world.ai.cursor+=1;world.ai.remaining-=1;report.visited+=1;continue;}
            let previous=world.ai.actors[index].state;let mut can_sleep=previous.mode==AiMode::Idle&&previous.target.is_none();
            match self.resolve_actor(world,index,budget){
                Ok(event)=>{if let Some(event)=event{if matches!(event.reason,AiReason::AttackRejected(_)){report.rejected+=1;can_sleep=false;}events.push(event);}},
                Err(error)=>{
                    // Stop unsafe old intent even if replacement acquisition
                    // cannot finish within this query budget. Keep its cursor.
                    let actor=&world.ai.actors[index];let entity=actor.entity;let before=actor.state;let mut after=before;
                    let controlled=actor.control_until>tick;let lost=before.target.is_some_and(|target|world.ai_target(target).is_none());
                    if controlled||lost{
                        let windup=world.attack_state(entity).unwrap().windup;world.stop_attack(entity).unwrap();after.controlled=controlled;
                        if lost{after.target=None;after.entered_aggro=false;}
                        world.ai.actors[index].state=after;
                        if before!=after||windup.is_some(){events.push(AiEvent{actor:entity,before,after,reason:if controlled{AiReason::Control}else{AiReason::TargetLost},cancelled_windup:windup});}
                    }
                    report.deferred=Some(error);break;
                },
            }
            world.ai.actors[index].last_tick=Some(tick);world.ai.actors[index].last_targets=world.ai.target_revision;
            if can_sleep {let (navigation,movement)=world.navigation_motion_at(world.ai.actors[index].row);
                can_sleep=movement!=crate::MovementStatus::Moving&&!matches!(navigation,crate::NavigationStatus::Pending{..});}
            if !world.ai.park_actor(index,tick,can_sleep){world.ai.cursor+=1;}
            world.ai.remaining-=1;report.visited+=1;report.processed+=1;
        }
        if report.deferred.is_some(){break;}
        }
        finish_report(&world.ai,budget,old_work,&mut report);Ok(report)
    }
    fn resolve_actor(&self,world:&mut World,index:usize,budget:&mut CombatQueryBudget)->Result<Option<AiEvent>,CombatQueryError>{
        let actor=&world.ai.actors[index];let (entity,row,profile,before,control_until,damage_wake)=(actor.entity,actor.row,actor.profile,actor.state,actor.control_until,actor.damage_wake);
        let origin=world.position_at(row);let mut after=before;let mut reason=AiReason::Mode;let mut finish=false;
        if !world.health.read(row).is_ok_and(|h|!h.is_depleted()) {after=AiState{mode:AiMode::Dead,..AiState::default()};reason=AiReason::Dead;finish=true;}
        else {
            if after.mode==AiMode::Dead {after=AiState::default();}
            // A damage wake selects before the ordinary tick leash, matching
            // the legacy synchronous injury hook, including while controlled.
            if damage_wake {if let Some(target)=self.threat_target(world,index,origin,budget)?{
                if Some(target)!=after.target{after.target=Some(target);after.mode=AiMode::Chasing;after.entered_aggro=false;reason=if before.target.is_none(){AiReason::Acquired}else{AiReason::Switched};}
            }}
            if let Some(target)=after.target.and_then(|id|self.target(world,id)) {
                let position=target.position;after.entered_aggro|=within_attack_range(origin,position,profile.aggro_radius);
                if after.entered_aggro&&!within_attack_range(origin,position,profile.leash_radius){after=AiState::default();reason=AiReason::Leash;finish=true;}
            }
            if !finish&&control_until>world.tick(){after.controlled=true;reason=AiReason::Control;finish=true;}else{after.controlled=false;}
        }
        if !finish {
            if let Some(target)=if damage_wake{None}else{self.threat_target(world,index,origin,budget)?} {
                if Some(target)!=after.target{after.target=Some(target);after.mode=AiMode::Chasing;after.entered_aggro=false;reason=if before.target.is_none(){AiReason::Acquired}else{AiReason::Switched};}
            }
            let target_valid=after.target.and_then(|id|self.target(world,id)).is_some();
            if !target_valid {
                let fallback=if after.mode==AiMode::Chasing{self.nearest(world,entity,origin,None,budget)?}
                    else if after.mode==AiMode::Idle{self.nearest(world,entity,origin,Some(profile.aggro_radius),budget)?}else{None};
                after.target=fallback;after.entered_aggro=false;
                if fallback.is_some(){after.mode=AiMode::Chasing;reason=if before.target.is_none(){AiReason::Acquired}else{AiReason::Switched};}
                else{after.mode=AiMode::Idle;if before.target.is_some(){reason=AiReason::TargetLost;}}
            }
            if let Some(target)=after.target.and_then(|id|self.target(world,id)){
                let position=target.position;after.entered_aggro|=within_attack_range(origin,position,profile.aggro_radius);
                if after.mode==AiMode::Chasing&&within_attack_range(origin,position,profile.attack.range){after.mode=AiMode::Attacking;}
                else if after.mode==AiMode::Attacking&&!within_attack_range(origin,position,profile.attack_break_range){after.mode=AiMode::Chasing;}
            }
        }
        let old_attack=world.attack_state(entity).expect("registered live handle");
        let result=if after.controlled||after.target.is_none()||after.mode==AiMode::Dead{if old_attack.intent.is_none(){Ok(())}else{world.stop_attack(entity).map(|_|())}}
            else if after.mode==AiMode::Attacking{
                // Finishing the accepted windup preserves its range/timing; a
                // mode change must not restart a valid in-flight attack.
                if old_attack.windup.is_some_and(|w|Some(w.target)==after.target){Ok(())}
                else{let desired=AttackIntent{target:after.target.unwrap(),profile:AttackProfile{range:profile.attack_break_range,..profile.attack}};
                    if old_attack.intent==Some(desired)&&old_attack.chase.is_none(){Ok(())}else{world.start_attack(entity,desired.target,desired.profile)}}
            }else{let desired=AttackIntent{target:after.target.unwrap(),profile:profile.attack};
                if old_attack.intent==Some(desired)&&old_attack.chase.is_some_and(|c|c.config==profile.chase&&c.status!=ChaseStatus::Interrupted){Ok(())}
                else{world.start_chase_attack(entity,desired.target,desired.profile,profile.chase)}};
        if let Err(error)=result{return Ok(Some(AiEvent{actor:entity,before,after:before,reason:AiReason::AttackRejected(error),cancelled_windup:None}));}
        let cancelled_windup=old_attack.windup.filter(|old|world.attack_state(entity).unwrap().windup!=Some(*old));
        let actor=&mut world.ai.actors[index];actor.state=after;actor.damage_wake=false;
        if after.mode==AiMode::Dead{world.ai.threat_count-=actor.threats.len();actor.threats.clear();}
        Ok((before!=after||cancelled_windup.is_some()).then_some(AiEvent{actor:entity,before,after,reason,cancelled_windup}))
    }
}
fn finish_report(ai:&AiColumns,budget:&CombatQueryBudget,old:CombatQueryWork,report:&mut AiReport){
    let work=budget.work();report.work=CombatQueryWork{cells:work.cells-old.cells,candidates:work.candidates-old.candidates,sight_steps:work.sight_steps-old.sight_steps};
    report.pending_wakes=ai.activation.dirty.len();report.sleeping=ai.activation.sleeping;report.remaining=ai.remaining+report.pending_wakes;
}
