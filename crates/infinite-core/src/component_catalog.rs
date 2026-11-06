//! Explicitly public immutable definition revisions. Payloads are application
//! schema bytes, never an implicit serialization of player/private state.
use std::sync::Arc;
use crate::{ComponentError,World};
pub const MAX_STATIC_DEFINITIONS:usize=4096;
pub const MAX_STATIC_DEFINITION_BYTES:usize=1024;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct StaticDefinition { pub id:u32, pub revision:u32, pub bytes:Arc<[u8]> }
pub(crate) struct DefinitionEntry { pub state:StaticDefinition, pub users:usize }
impl World {
    /// IDs are stable for this World. Replacing bytes increments the revision;
    /// already captured/inflight definitions retain their old immutable bytes.
    pub fn define_public_component(&mut self,id:u32,bytes:&[u8])->Result<bool,ComponentError>{
        if bytes.len()>MAX_STATIC_DEFINITION_BYTES{return Err(ComponentError::DefinitionLimit);}
        let store=&mut self.components;
        let old=store.definitions.get(&id);
        if old.is_some_and(|e|e.state.bytes.as_ref()==bytes){return Ok(false);}
        if old.is_none()&&store.definitions.len()==MAX_STATIC_DEFINITIONS{return Err(ComponentError::DefinitionLimit);}
        // Reusing a removed ID also advances its lifetime revision. The map is
        // bounded by distinct admitted IDs, not just currently live definitions.
        if !store.definition_versions.contains_key(&id)&&store.definition_versions.len()==MAX_STATIC_DEFINITIONS{return Err(ComponentError::DefinitionLimit);}
        let revision=store.definition_versions.get(&id).copied().unwrap_or(0).checked_add(1).ok_or(ComponentError::DefinitionLimit)?;
        let catalog_revision=store.catalog_revision.checked_add(1).ok_or(ComponentError::DefinitionLimit)?;
        let users=old.map_or(0,|e|e.users);
        store.definitions.insert(id,DefinitionEntry{state:StaticDefinition{id,revision,bytes:Arc::from(bytes)},users});
        store.definition_versions.insert(id,revision);store.catalog_revision=catalog_revision;Ok(true)
    }
    pub fn remove_public_component_definition(&mut self,id:u32)->Result<bool,ComponentError>{
        let store=&mut self.components;let Some(entry)=store.definitions.get(&id)else{return Ok(false);};
        if entry.users!=0{return Err(ComponentError::DefinitionInUse);}
        let revision=store.catalog_revision.checked_add(1).ok_or(ComponentError::DefinitionLimit)?;
        store.definitions.remove(&id);store.catalog_revision=revision;Ok(true)
    }
    pub fn public_component_definition(&self,id:u32)->Option<&StaticDefinition>{self.components.definitions.get(&id).map(|e|&e.state)}
}
