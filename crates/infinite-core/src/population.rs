//! One map's natural birth-group slots. Species choice/placement remain app policy.
use std::{collections::HashMap,num::{NonZeroU16,NonZeroU32}};
use crate::{AiProfile,AiError,EntityId,GridPosition,HealthError,LifecycleProfile,Tick,World,WorldError};
pub const MAX_POPULATION_SLOTS:usize=256;
pub const MAX_SPAWN_GROUP_MEMBERS:usize=64;
pub const MAX_SPAWN_SPECIES_GATES:usize=1024;
pub const MAX_SPAWN_WORK:usize=4096;
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn population_sequence_and_deadline_overflow_do_not_reuse_or_make_ready(){
        let mut w=World::default();w.resize_population(1).unwrap();w.population.sequence=u64::MAX;
        assert_eq!(w.reserve_spawn(1),Err(PopulationError::SequenceExhausted));assert_eq!(w.population_state().reserved,0);
        let mut w=World::default();w.resize_population(1).unwrap();let e=w.spawn(GridPosition::default()).unwrap();w.configure_health(e,1,NonZeroU32::new(1).unwrap()).unwrap();w.allocate_spawn_group(1,None,&[e],1).unwrap();
        w.skip_idle_to(Tick(u64::MAX),crate::MapWork::default()).unwrap();w.apply_damage(e,1).unwrap();assert_eq!(w.population_state().blocked,1);assert_eq!(w.population_state().ready,0);assert!(w.map_activity(crate::MapWork::default()).pending);
        let before=w.population_state();assert_eq!(w.resize_population(257),Err(PopulationError::InvalidCapacity));assert_eq!(w.population_state(),before);
    }
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
pub struct SpawnGroupId(pub u64);
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SpawnReservation {pub id:u64,pub species:u32,pub slot:u16}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SpawnTemplate {pub maximum_health:NonZeroU32,pub corpse_ticks:u32,pub respawn_ticks:u32,pub ai:Option<AiProfile>}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum PopulationError {
    InvalidCapacity,InvalidSpecies,InvalidGroupSize,InvalidBudget,BudgetExhausted,NoVacancy,
    SpeciesCoolingDown,StaleReservation,SequenceExhausted,TimeOverflow,SpeciesGateCapacity,
    InvalidEntity,DuplicateMember,AlreadyMember,GroupMismatch,UnknownGroup,Depleted,
    WorldCapacity,LifecycleCapacity,Health(HealthError),Ai(AiError),World(WorldError),
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SpawnBudget {members:usize,groups:usize}
impl SpawnBudget {
    pub fn new(members:usize,groups:usize)->Result<Self,PopulationError>{
        if members>MAX_SPAWN_WORK||groups>MAX_POPULATION_SLOTS{return Err(PopulationError::InvalidBudget);}Ok(Self{members,groups})
    }
    pub fn remaining_members(&self)->usize{self.members}
    pub fn remaining_groups(&self)->usize{self.groups}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct PopulationState {
    pub capacity:usize,pub enabled:usize,pub alive:usize,pub cooling_down:usize,pub ready:usize,
    pub reserved:usize,pub eligible_unreserved:usize,pub blocked:usize,pub next_due:Option<Tick>,
}
struct Slot {members:u16,order:u16,group:Option<SpawnGroupId>,ready_at:Option<Tick>,reservation:Option<SpawnReservation>,cooldown:u32}
impl Default for Slot {fn default()->Self{Self{members:0,order:u16::MAX,group:None,ready_at:Some(Tick(0)),reservation:None,cooldown:0}}}
#[derive(Default)]
pub(crate) struct PopulationColumns {
    slots:Vec<Slot>,enabled:Vec<u16>,retired:Vec<u16>,rows:Vec<Option<NonZeroU16>>,
    capacity:usize,sequence:u64,gates:HashMap<u32,Tick>,
    // Bits use enabled order, not physical slot order: shrink/grow preserves the
    // original first-ready choice even when retired slots are restored in LIFO order.
    vacant:[u64;4],reserved_mask:[u64;4],selection:SelectionCache,
}
#[derive(Default)]
struct SelectionCache {valid:bool,next_due:Option<Tick>,ready:[u64;4]}
impl PopulationColumns {
    fn indices(&self,mask:[u64;4])->impl Iterator<Item=usize>+'_ {
        mask.into_iter().enumerate().flat_map(move |(word,mut bits)|std::iter::from_fn(move||{
            if bits==0{return None;}let bit=bits.trailing_zeros()as usize;bits&=bits-1;Some(usize::from(self.enabled[word*64+bit]))
        }))
    }
    fn refresh_slot(&mut self,index:usize){
        let slot=&self.slots[index];let order=usize::from(slot.order);if order==usize::from(u16::MAX){return;}
        let word=order/64;let bit=1u64<<(order%64);self.vacant[word]&=!bit;self.reserved_mask[word]&=!bit;
        if slot.members==0{self.vacant[word]|=bit;if slot.reservation.is_some(){self.reserved_mask[word]|=bit;}}
        if slot.members>0||slot.reservation.is_some(){self.selection.ready[word]&=!bit;}else{self.selection.valid=false;}
    }
    fn rebuild_masks(&mut self){
        self.vacant=[0;4];self.reserved_mask=[0;4];self.selection.valid=false;
        for order in 0..self.enabled.len(){let index=usize::from(self.enabled[order]);self.slots[index].order=order as u16;self.refresh_slot(index);}
    }
    pub(crate) fn spawn(&mut self){self.rows.push(None);}
    fn retire(&mut self,at:usize){let index=self.enabled.remove(at);self.slots[usize::from(index)].reservation=None;self.slots[usize::from(index)].order=u16::MAX;self.retired.push(index);self.rebuild_masks();}
    pub(crate) fn release(&mut self,row:usize,now:Tick)->bool{
        let Some(index)=self.rows[row].take()else{return false;};let index=usize::from(index.get()-1);let slot=&mut self.slots[index];
        slot.members-=1;if slot.members==0{
            slot.ready_at=now.0.checked_add(u64::from(slot.cooldown)).map(Tick);
            if self.enabled.len()>self.capacity{let at=self.enabled.iter().position(|&i|usize::from(i)==index).unwrap();self.retire(at);}
            else{self.refresh_slot(index);}
        }true
    }
    pub(crate) fn despawn(&mut self,row:usize,now:Tick){self.release(row,now);self.rows.swap_remove(row);}
    fn check_species(&self,species:u32,now:Tick)->Result<(),PopulationError>{
        if species==0{return Err(PopulationError::InvalidSpecies);}if self.gates.get(&species).is_some_and(|at|*at>now){return Err(PopulationError::SpeciesCoolingDown);}Ok(())
    }
    fn reserved(&self,r:SpawnReservation)->Result<usize,PopulationError>{
        let index=usize::from(r.slot);if self.slots.get(index).is_none_or(|s|s.reservation!=Some(r)){return Err(PopulationError::StaleReservation);}Ok(index)
    }
    fn select(&mut self,species:u32,reservation:Option<SpawnReservation>,now:Tick)->Result<usize,PopulationError>{
        self.check_species(species,now)?;
        if let Some(r)=reservation{
            if r.species!=species{return Err(PopulationError::StaleReservation);}let index=self.reserved(r)?;
            if self.slots[index].members!=0||!self.slots[index].ready_at.is_some_and(|at|at<=now){return Err(PopulationError::NoVacancy);}Ok(index)
        }else{
            if !self.selection.valid||self.selection.next_due.is_some_and(|due|due<=now){
                let mut ready=[0u64;4];let mut next_due:Option<Tick>=None;
                for index in self.indices(std::array::from_fn(|i|self.vacant[i]&!self.reserved_mask[i])){
                    let slot=&self.slots[index];if let Some(due)=slot.ready_at{
                        if due<=now{let order=usize::from(slot.order);ready[order/64]|=1u64<<(order%64);}
                        else{next_due=Some(next_due.map_or(due,|old|old.min(due)));}
                    }
                }
                self.selection=SelectionCache{valid:true,next_due,ready};
            }
            self.indices(self.selection.ready).next().ok_or(PopulationError::NoVacancy)
        }
    }
    fn group_id(&self,reservation:Option<SpawnReservation>)->Result<SpawnGroupId,PopulationError>{
        reservation.map(|r|SpawnGroupId(r.id)).map_or_else(||self.sequence.checked_add(1).map(SpawnGroupId).ok_or(PopulationError::SequenceExhausted),Ok)
    }
    fn claim(&mut self,index:usize,group:SpawnGroupId,members:&[usize],cooldown:u32){
        self.sequence=self.sequence.max(group.0);let slot=&mut self.slots[index];slot.members=members.len()as u16;slot.group=Some(group);slot.reservation=None;slot.cooldown=cooldown;
        for &row in members{self.rows[row]=NonZeroU16::new((index+1)as u16);}
        self.refresh_slot(index);
    }
}
impl World {
    /// Vacancies retire first; living excess groups retire on their final death.
    /// Retired slots retain their cooldown and are restored in LIFO order.
    pub fn resize_population(&mut self,capacity:usize)->Result<(),PopulationError>{
        if capacity>MAX_POPULATION_SLOTS{return Err(PopulationError::InvalidCapacity);}let p=&mut self.population;p.capacity=capacity;
        for at in (0..p.enabled.len()).rev(){if p.enabled.len()<=capacity{break;}if p.slots[usize::from(p.enabled[at])].members==0{p.retire(at);}}
        while p.enabled.len()<capacity{let index=p.retired.pop().unwrap_or_else(||{let index=p.slots.len()as u16;p.slots.push(Slot::default());index});p.enabled.push(index);}p.rebuild_masks();Ok(())
    }
    pub fn population_state(&self)->PopulationState{
        let p=&self.population;let now=self.tick();let mut r=PopulationState{capacity:p.capacity,enabled:p.enabled.len(),alive:0,cooling_down:0,ready:0,reserved:0,eligible_unreserved:0,blocked:0,next_due:None};
        r.alive=p.enabled.len()-p.vacant.iter().map(|bits|bits.count_ones()as usize).sum::<usize>();
        for index in p.indices(p.vacant){let s=&p.slots[index];let Some(at)=s.ready_at else{r.blocked+=1;continue;};
            if at>now{r.cooling_down+=1;}else{r.ready+=1;}
            let eligible=if let Some(reservation)=s.reservation{r.reserved+=1;at.max(*p.gates.get(&reservation.species).unwrap_or(&Tick(0)))}else{if at<=now{r.eligible_unreserved+=1;}at};
            let next=eligible.max(now);r.next_due=Some(r.next_due.map_or(next,|old|old.min(next)));
        }r
    }
    pub fn reserve_spawn(&mut self,species:u32)->Result<SpawnReservation,PopulationError>{
        let index=self.population.select(species,None,self.tick())?;let id=self.population.group_id(None)?.0;
        let r=SpawnReservation{id,species,slot:index as u16};self.population.sequence=id;self.population.slots[index].reservation=Some(r);self.population.refresh_slot(index);Ok(r)
    }
    pub fn cancel_spawn_reservation(&mut self,reservation:SpawnReservation)->Result<(),PopulationError>{
        let index=self.population.reserved(reservation)?;self.population.slots[index].reservation=None;self.population.refresh_slot(index);Ok(())
    }
    /// Species-wide backoff, matching shared blocked-placement retries. Keep a
    /// mini-boss reservation, or cancel a regular choice so other species can use it.
    pub fn defer_spawn_species(&mut self,species:u32,delay:NonZeroU32)->Result<Tick,PopulationError>{
        if species==0{return Err(PopulationError::InvalidSpecies);}let now=self.tick();let at=now.0.checked_add(u64::from(delay.get())).map(Tick).ok_or(PopulationError::TimeOverflow)?;
        let gates=&mut self.population.gates;if !gates.contains_key(&species)&&gates.len()==MAX_SPAWN_SPECIES_GATES{gates.retain(|_,at|*at>now);}
        if !gates.contains_key(&species)&&gates.len()==MAX_SPAWN_SPECIES_GATES{return Err(PopulationError::SpeciesGateCapacity);}
        let at=at.max(*gates.get(&species).unwrap_or(&Tick(0)));gates.insert(species,at);Ok(at)
    }
    pub fn ready_spawn_reservations(&self,output:&mut Vec<SpawnReservation>){
        output.clear();for index in self.population.indices(self.population.reserved_mask){let s=&self.population.slots[index];if s.ready_at.is_some_and(|at|at<=self.tick()){
            if let Some(r)=s.reservation{if self.population.check_species(r.species,self.tick()).is_ok(){output.push(r);}}
        }}
    }
    pub fn spawn_group_of(&self,entity:EntityId)->Result<Option<SpawnGroupId>,PopulationError>{
        let row=self.dense_index(entity).map_err(|_|PopulationError::InvalidEntity)?;Ok(self.population.rows[row].and_then(|i|self.population.slots[usize::from(i.get()-1)].group))
    }
    pub fn set_spawn_group_cooldown(&mut self,group:SpawnGroupId,ticks:u32)->Result<(),PopulationError>{
        let s=self.population.slots.iter_mut().find(|s|s.group==Some(group)&&s.members>0).ok_or(PopulationError::UnknownGroup)?;s.cooldown=ticks;Ok(())
    }
    pub fn release_spawn_member(&mut self,entity:EntityId,expected:Option<SpawnGroupId>)->Result<bool,PopulationError>{
        let group=self.spawn_group_of(entity)?;if expected.is_some()&&group!=expected{return Err(PopulationError::GroupMismatch);}
        let row=self.dense_index(entity).unwrap();Ok(self.population.release(row,self.tick()))
    }
    /// Attach existing living entities atomically, useful for application-loaded
    /// groups. Corpse removal policy is configured separately in this path.
    pub fn allocate_spawn_group(&mut self,species:u32,reservation:Option<SpawnReservation>,members:&[EntityId],cooldown:u32)->Result<SpawnGroupId,PopulationError>{
        if members.is_empty()||members.len()>MAX_SPAWN_GROUP_MEMBERS{return Err(PopulationError::InvalidGroupSize);}
        let index=self.population.select(species,reservation,self.tick())?;let group=self.population.group_id(reservation)?;let mut rows=[0usize;MAX_SPAWN_GROUP_MEMBERS];
        for (at,&entity)in members.iter().enumerate(){if members[..at].contains(&entity){return Err(PopulationError::DuplicateMember);}let row=self.dense_index(entity).map_err(|_|PopulationError::InvalidEntity)?;
            if self.population.rows[row].is_some(){return Err(PopulationError::AlreadyMember);}if self.health.read(row).map_err(PopulationError::Health)?.is_depleted(){return Err(PopulationError::Depleted);}rows[at]=row;}
        self.population.claim(index,group,&rows[..members.len()],cooldown);Ok(group)
    }
    /// Preflight the entire birth before mutating World, reservation or budget.
    /// Caller validates dynamic occupancy/player separation/species placement;
    /// the core checks static collision, capacities and optional AI profile.
    pub fn spawn_group(&mut self,species:u32,reservation:Option<SpawnReservation>,template:SpawnTemplate,positions:&[GridPosition],budget:&mut SpawnBudget,output:&mut Vec<EntityId>)->Result<SpawnGroupId,PopulationError>{
        let count=positions.len();if count==0||count>MAX_SPAWN_GROUP_MEMBERS{return Err(PopulationError::InvalidGroupSize);}
        if budget.groups==0||budget.members<count{return Err(PopulationError::BudgetExhausted);}
        let index=self.population.select(species,reservation,self.tick())?;let group=self.population.group_id(reservation)?;
        if self.len()+count>crate::MAX_SYNC_ENTITIES{return Err(PopulationError::WorldCapacity);}
        self.preflight_spawns(positions).map_err(PopulationError::World)?;
        if !self.lifecycle.can_register(count){return Err(PopulationError::LifecycleCapacity);}
        if let Some(ai)=template.ai{self.ai.preflight_registration(count,ai).map_err(PopulationError::Ai)?;}
        output.clear();output.reserve(count);let mut rows=[0usize;MAX_SPAWN_GROUP_MEMBERS];
        for (at,&position)in positions.iter().enumerate(){let entity=self.spawn(position).expect("birth preflight");self.configure_health(entity,template.maximum_health.get(),template.maximum_health).unwrap();
            self.register_lifecycle(entity,LifecycleProfile{corpse_ticks:template.corpse_ticks,respawn_after:None}).unwrap();if let Some(ai)=template.ai{self.register_ai_actor(entity,ai).unwrap();}
            rows[at]=self.dense_index(entity).unwrap();output.push(entity);}
        self.population.claim(index,group,&rows[..count],template.respawn_ticks);budget.groups-=1;budget.members-=count;Ok(group)
    }
}
