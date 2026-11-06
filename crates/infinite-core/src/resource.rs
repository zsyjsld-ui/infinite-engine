use crate::{EntityId,World};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ResourceState { pub current:u32, pub maximum:u32 }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ResourceOutcome { pub before:ResourceState, pub after:ResourceState }
impl ResourceOutcome{pub fn changed(self)->bool{self.before!=self.after}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ResourceError { InvalidEntity,Unconfigured,AlreadyConfigured,AboveMaximum,Insufficient,AttributesPending,MaximumMismatch }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ResourceCommand { Spend(u32), Restore(u32), SetMaximum(u32) }
#[derive(Default)]
pub(crate) struct ResourceColumns { states:Vec<Option<ResourceState>> }
impl ResourceColumns {
    pub(crate) fn spawn(&mut self){self.states.push(None);}
    pub(crate) fn despawn(&mut self,row:usize){self.states.swap_remove(row);}
    pub(crate) fn read(&self,row:usize)->Result<ResourceState,ResourceError>{self.states[row].ok_or(ResourceError::Unconfigured)}
    pub(crate) fn apply(&mut self,row:usize,command:ResourceCommand)->Result<ResourceOutcome,ResourceError>{
        let before=self.read(row)?;let mut after=before;
        match command{
            ResourceCommand::Spend(amount)=>{after.current=before.current.checked_sub(amount).ok_or(ResourceError::Insufficient)?;},
            ResourceCommand::Restore(amount)=>{after.current+=amount.min(before.maximum-before.current);},
            ResourceCommand::SetMaximum(maximum)=>{after.maximum=maximum;after.current=before.current.min(maximum);},
        }
        self.states[row]=Some(after);Ok(ResourceOutcome{before,after})
    }
    pub(crate) fn derive_maximum(&mut self,row:usize,maximum:u32)->Option<ResourceOutcome>{
        self.states[row]?;Some(self.apply(row,ResourceCommand::SetMaximum(maximum)).unwrap())
    }
}
impl World {
    /// Zero maximum is valid. Attribute derivation updates this maximum when
    /// both components exist; increasing maximum never restores spent resource.
    pub fn configure_resource(&mut self,entity:EntityId,current:u32,maximum:u32)->Result<(),ResourceError>{
        let row=self.dense_index(entity).map_err(|_|ResourceError::InvalidEntity)?;
        if self.resource.states[row].is_some(){return Err(ResourceError::AlreadyConfigured);}
        if current>maximum{return Err(ResourceError::AboveMaximum);}
        match self.combat_attributes(entity){
            Ok(attributes)if attributes.maximum_resource!=maximum=>return Err(ResourceError::MaximumMismatch),
            Err(crate::AttributeError::Dirty)=>return Err(ResourceError::AttributesPending),
            _=>{},
        }
        self.resource.states[row]=Some(ResourceState{current,maximum});Ok(())
    }
    pub fn resource(&self,entity:EntityId)->Result<ResourceState,ResourceError>{let row=self.dense_index(entity).map_err(|_|ResourceError::InvalidEntity)?;self.resource.read(row)}
    pub fn apply_resource(&mut self,entity:EntityId,command:ResourceCommand)->Result<ResourceOutcome,ResourceError>{let row=self.dense_index(entity).map_err(|_|ResourceError::InvalidEntity)?;self.resource_at(row,command)}
}
