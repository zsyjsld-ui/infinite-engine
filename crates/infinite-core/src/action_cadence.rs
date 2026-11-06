//! Integer action deadlines for applications with externally authorized casts.
//! The clock unit is chosen by the application (e.g. 1 ms ingress ticks); it is
//! independent of the movement step. No timer, automatic attack, or MP debit.
use crate::Tick;
use std::num::NonZeroU32;
pub const MAX_CADENCE_SKILLS:usize=32;
pub const MAX_CADENCE_GROUPS:usize=16;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CadenceGrace {pub maximum:u32,pub divisor:NonZeroU32}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CadenceTiming {pub cooldown:u32,pub recovery:Option<(u16,u32)>}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CadenceDeadline {pub key:u16,pub ready:Tick,pub duration:u32}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CadenceReadiness {pub cooldown:bool,pub recovery:bool}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum CadenceError {SkillCapacity,GroupCapacity,TimeOverflow}
pub struct ActionCadence {
    grace:CadenceGrace,last_skill:Option<u16>,last_accepted:Tick,
    cooldowns:Vec<CadenceDeadline>,recoveries:Vec<CadenceDeadline>,
}
impl ActionCadence {
    pub fn new(grace:CadenceGrace)->Self {Self{grace,last_skill:None,last_accepted:Tick(0),cooldowns:Vec::new(),recoveries:Vec::new()}}
    pub fn grace(&self)->CadenceGrace{self.grace}
    pub fn last_skill(&self)->Option<u16>{self.last_skill}
    pub fn last_accepted(&self)->Tick{self.last_accepted}
    pub fn cooldowns(&self)->&[CadenceDeadline]{&self.cooldowns}
    pub fn recoveries(&self)->&[CadenceDeadline]{&self.recoveries}
    fn allowance(&self,duration:u32)->u64{u64::from(self.grace.maximum.min(duration/self.grace.divisor.get()))}
    fn ready(&self,slot:Option<&CadenceDeadline>,now:Tick)->bool {
        slot.is_none_or(|s|now>=s.ready||s.ready.0-now.0<=self.allowance(s.duration))
    }
    pub fn readiness(&self,skill:u16,timing:CadenceTiming,now:Tick)->CadenceReadiness {
        CadenceReadiness{cooldown:self.ready(self.cooldowns.iter().find(|s|s.key==skill),now),
            recovery:timing.recovery.is_none_or(|(key,_)|self.ready(self.recoveries.iter().find(|s|s.key==key),now))}
    }
    fn next(&self,old:Option<&CadenceDeadline>,duration:u32,now:Tick)->Result<Tick,CadenceError> {
        let anchor=old.filter(|s|s.ready>now&&s.ready.0-now.0<=self.allowance(s.duration)).map_or(now,|s|s.ready);
        anchor.0.checked_add(u64::from(duration)).map(Tick).ok_or(CadenceError::TimeOverflow)
    }
    /// The application has already authorized the action, including any explicit
    /// recovery-cancel policy. This method does not itself grant permission.
    /// All capacities/deadlines preflight before changing the ledger.
    pub fn accept_authorized(&mut self,skill:u16,timing:CadenceTiming,now:Tick)->Result<(),CadenceError> {
        let cd=self.cooldowns.iter().position(|s|s.key==skill);
        if cd.is_none()&&self.cooldowns.len()>=MAX_CADENCE_SKILLS{return Err(CadenceError::SkillCapacity)}
        let next=self.next(cd.map(|i|&self.cooldowns[i]),timing.cooldown,now)?;
        let recovery=if let Some((key,duration))=timing.recovery.filter(|(_,d)|*d>0){
            let index=self.recoveries.iter().position(|s|s.key==key);
            if index.is_none()&&self.recoveries.len()>=MAX_CADENCE_GROUPS{return Err(CadenceError::GroupCapacity)}
            let ready=self.next(index.map(|i|&self.recoveries[i]),duration,now)?;
            Some((index,CadenceDeadline{key,ready,duration}))
        }else{None};
        let value=CadenceDeadline{key:skill,ready:next,duration:timing.cooldown};
        if let Some(i)=cd{self.cooldowns[i]=value}else{self.cooldowns.push(value)}
        if let Some((index,value))=recovery{if let Some(i)=index{self.recoveries[i]=value}else{self.recoveries.push(value)}}
        self.last_skill=Some(skill);self.last_accepted=now;Ok(())
    }
}
