//! Bounded, integer component projections. These are replaceable presentation
//! states, never a replacement for ordered combat or durable economy events.
use std::{collections::{BTreeMap, VecDeque}, num::NonZeroU32, sync::{Arc, Mutex}};
use crate::{EntityId, SpatialFrame, SyncError, Tick, ViewRect, World};
use crate::sync::{key, VisibleRows};

pub const COMPONENT_FIELDS: usize = 16;
pub const MAX_COMPONENTS_PER_ENTITY: usize = 128;
pub const MAX_COMPONENT_RECORDS: usize = 262_144;
pub const MAX_COMPONENT_VIEW_RECORDS: usize = 16_384;
pub const COMPONENT_JOURNAL_CAPACITY: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ComponentKind { Appearance = 1, Effect = 2, Skill = 3, GroundItem = 4, Attributes = 5, GroundMembers = 6 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ComponentScope { Owner = 1, Nearby = 2, Map = 3 }
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ComponentKey { pub kind: ComponentKind, pub instance: u32 }

/// Missing fields and fields whose value is zero are distinct. u64 deadlines
/// use two adjacent u32 words; field names/types belong to the component schema.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComponentFields { pub(crate) present: u16, pub(crate) references: u16, pub(crate) words: [u32; COMPONENT_FIELDS] }
impl ComponentFields {
    pub fn from_words(words: &[u32]) -> Result<Self, ComponentError> {
        if words.len() > COMPONENT_FIELDS { return Err(ComponentError::InvalidField); }
        let mut value = Self::default();
        for (i, &word) in words.iter().enumerate() { value.set(i, word)?; }
        Ok(value)
    }
    pub fn mask(&self) -> u16 { self.present }
    pub fn reference_mask(&self) -> u16 { self.references }
    pub fn get(&self, field: usize) -> Option<u32> {
        (field < COMPONENT_FIELDS && self.present & (1 << field) != 0).then(|| self.words[field])
    }
    pub fn set(&mut self, field: usize, value: u32) -> Result<(), ComponentError> {
        if field >= COMPONENT_FIELDS { return Err(ComponentError::InvalidField); }
        self.words[field] = value; self.present |= 1 << field; self.references &= !(1 << field); Ok(())
    }
    pub fn set_reference(&mut self, field:usize, definition:u32)->Result<(),ComponentError>{self.set(field,definition)?;self.references|=1<<field;Ok(())}
    pub fn remove(&mut self, field: usize) -> Result<(), ComponentError> {
        if field >= COMPONENT_FIELDS { return Err(ComponentError::InvalidField); }
        self.present &= !(1 << field); self.references &= !(1 << field); self.words[field] = 0; Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentRecord { pub key: ComponentKey, pub scope: ComponentScope, pub fields: ComponentFields }
impl ComponentRecord {
    pub fn validate(self) -> Result<(), ComponentError> {
        let valid = match self.key.kind {
            ComponentKind::Skill | ComponentKind::Attributes => self.scope == ComponentScope::Owner,
            ComponentKind::Effect => self.scope != ComponentScope::Map,
            ComponentKind::Appearance | ComponentKind::GroundItem | ComponentKind::GroundMembers => self.scope != ComponentScope::Owner,
        };
        if valid { Ok(()) } else { Err(ComponentError::InvalidScope) }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentError { InvalidEntity, InvalidField, InvalidScope, EntityLimit, RecordLimit,
    ViewLimit, WrongWorld, DuplicateKey, InvalidKind, DefinitionLimit, MissingDefinition, DefinitionInUse, UnsupportedReferences, State(SyncError),
    Skill(crate::SkillError), Effect(crate::EffectError), Attribute(crate::AttributeError) }
impl From<SyncError> for ComponentError { fn from(e: SyncError) -> Self { Self::State(e) } }

pub(crate) struct ComponentColumns {
    rows: Vec<Vec<ComponentRecord>>, count: usize, revision: u64,
    source: Arc<()>, journal: VecDeque<(u64, EntityId)>,
    pub(crate) definitions:BTreeMap<u32,crate::component_catalog::DefinitionEntry>,
    pub(crate) definition_versions:BTreeMap<u32,u32>, pub(crate) catalog_revision:u64,
}
impl Default for ComponentColumns {
    fn default() -> Self { Self { rows: Vec::new(), count: 0, revision: 0,
        source: Arc::new(()), journal: VecDeque::new(),definitions:BTreeMap::new(),definition_versions:BTreeMap::new(),catalog_revision:0 } }
}
impl ComponentColumns {
    fn check_references(&self,record:ComponentRecord)->Result<(),ComponentError>{
        for i in 0..COMPONENT_FIELDS{if record.fields.references&(1<<i)!=0&&!self.definitions.contains_key(&record.fields.words[i]){return Err(ComponentError::MissingDefinition);}}Ok(())
    }
    fn references(&mut self,record:ComponentRecord,add:bool){
        for i in 0..COMPONENT_FIELDS{if record.fields.references&(1<<i)!=0{let entry=self.definitions.get_mut(&record.fields.words[i]).expect("validated definition retained");if add{entry.users+=1;}else{entry.users-=1;}}}
    }
    pub(crate) fn spawn(&mut self) { self.rows.push(Vec::new()); }
    fn changed(&mut self, entity: EntityId) {
        // Never wrap into a formerly valid observation baseline.
        if self.revision == u64::MAX { self.source = Arc::new(()); self.revision = 0; self.journal.clear(); }
        self.revision += 1;
        if self.journal.len() == COMPONENT_JOURNAL_CAPACITY { self.journal.pop_front(); }
        self.journal.push_back((self.revision, entity));
    }
    pub(crate) fn despawn(&mut self, row: usize, entity: EntityId) {
        let records = self.rows.swap_remove(row); self.count -= records.len();
        for &record in &records{self.references(record,false);}
        if !records.is_empty() { self.changed(entity); }
    }
}
impl World {
    /// Application-owned projection write. Identical values are a true no-op.
    /// It does not apply an item, grant a buff, spend mana, or alter cooldowns.
    pub fn set_component(&mut self, entity: EntityId, record: ComponentRecord) -> Result<bool, ComponentError> {
        record.validate()?;
        let row = self.dense_index(entity).map_err(|_| ComponentError::InvalidEntity)?;
        let store = &mut self.components;
        store.check_references(record)?;
        let records = &mut store.rows[row];
        let mut previous=None;
        match records.binary_search_by_key(&record.key, |r| r.key) {
            Ok(at) => { if records[at] == record { return Ok(false); } previous=Some(records[at]); records[at] = record; }
            Err(at) => {
                if records.len() == MAX_COMPONENTS_PER_ENTITY { return Err(ComponentError::EntityLimit); }
                if store.count == MAX_COMPONENT_RECORDS { return Err(ComponentError::RecordLimit); }
                records.insert(at, record); store.count += 1;
            }
        }
        if let Some(old)=previous{store.references(old,false);}store.references(record,true);
        store.changed(entity); Ok(true)
    }
    pub fn remove_component(&mut self, entity: EntityId, component: ComponentKey) -> Result<bool, ComponentError> {
        let row = self.dense_index(entity).map_err(|_| ComponentError::InvalidEntity)?;
        let store = &mut self.components;
        let Ok(at) = store.rows[row].binary_search_by_key(&component, |r| r.key) else { return Ok(false); };
        let old=store.rows[row].remove(at); store.references(old,false);store.count -= 1; store.changed(entity); Ok(true)
    }
    pub fn components(&self, entity: EntityId) -> Result<&[ComponentRecord], ComponentError> {
        let row = self.dense_index(entity).map_err(|_| ComponentError::InvalidEntity)?;
        Ok(&self.components.rows[row])
    }
    pub fn component_count(&self) -> usize { self.components.count }

    /// Atomically replace one component family, including removals. Used by
    /// core projections so an over-capacity refresh cannot publish half a buff list.
    pub fn replace_component_kind(&mut self, entity: EntityId, kind: ComponentKind,
        next: &[ComponentRecord]) -> Result<bool, ComponentError> {
        let row = self.dense_index(entity).map_err(|_| ComponentError::InvalidEntity)?;
        if next.len() > MAX_COMPONENTS_PER_ENTITY { return Err(ComponentError::EntityLimit); }
        for &record in next { if record.key.kind != kind { return Err(ComponentError::InvalidKind); } record.validate()?; }
        let mut replacement = next.to_vec(); replacement.sort_unstable_by_key(|r| r.key);
        if replacement.windows(2).any(|r| r[0].key == r[1].key) { return Err(ComponentError::DuplicateKey); }
        let store = &mut self.components; let old = &store.rows[row];
        for &record in next{store.check_references(record)?;}
        let removed = old.iter().filter(|r| r.key.kind == kind).count();
        if old.len() - removed + replacement.len() > MAX_COMPONENTS_PER_ENTITY { return Err(ComponentError::EntityLimit); }
        if store.count - removed + replacement.len() > MAX_COMPONENT_RECORDS { return Err(ComponentError::RecordLimit); }
        if old.iter().filter(|r| r.key.kind == kind).eq(replacement.iter()) { return Ok(false); }
        let new_count = store.count - removed + replacement.len();
        replacement.extend(old.iter().filter(|r| r.key.kind != kind).copied());
        replacement.sort_unstable_by_key(|r| r.key);
        let previous=std::mem::replace(&mut store.rows[row],replacement);
        for record in previous.into_iter().filter(|r|r.key.kind==kind){store.references(record,false);}
        for &record in next{store.references(record,true);}
        store.count = new_count; store.changed(entity); Ok(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleComponent { pub entity: EntityId, pub record: ComponentRecord }
impl VisibleComponent {
    pub(crate) fn order(&self) -> ((u32,u32), ComponentKey) { (key(self.entity), self.record.key) }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComponentCaptureReport { pub rebuilt: bool, pub changed_entities: usize, pub records: usize }

/// One immutable capture shared across connections. Only changed component rows
/// are cloned; the existing spatial capture still discovers position changes.
pub struct ComponentFrame {
    spatial: SpatialFrame, source: Option<Arc<()>>, revision: u64,
    entries: Vec<Option<(EntityId, Arc<[ComponentRecord]>)>>, incremental_base: Option<u64>,
    map_rows: Vec<VisibleComponent>, dirty: Vec<EntityId>, count: usize,
    definitions:BTreeMap<u32,crate::StaticDefinition>,catalog_revision:u64,
    // Exact-view public projections belong to this capture, never to an owner
    // or an ACK baseline. Bound retention when every connection has its own view.
    public_views: Mutex<Vec<(ViewRect, Arc<[VisibleComponent]>)>>,
}
impl ComponentFrame {
    pub fn new(cell_size: NonZeroU32) -> Self {
        Self { spatial: SpatialFrame::new(cell_size), source: None, revision: 0,
            entries: Vec::new(), incremental_base: None, map_rows: Vec::new(), dirty: Vec::new(), count: 0,definitions:BTreeMap::new(),catalog_revision:0,
            public_views: Mutex::new(Vec::new()) }
    }
    pub fn tick(&self) -> Option<Tick> { self.spatial.tick() }
    pub(crate) fn source(&self) -> Option<&Arc<()>> { self.source.as_ref() }
    pub fn motion_frame(&self) -> &SpatialFrame { &self.spatial }
    pub fn record_count(&self) -> usize { self.count }
    /// Retained public projection storage, excluding allocator and Arc headers.
    pub fn public_view_cache_bytes(&self) -> usize {
        let cache = self.public_views.lock().unwrap();
        cache.capacity() * std::mem::size_of::<(ViewRect, Arc<[VisibleComponent]>)>()
            + cache.iter().map(|(_, rows)| std::mem::size_of_val(rows.as_ref())).sum::<usize>()
    }
    pub fn definition(&self,id:u32)->Option<&crate::StaticDefinition>{self.definitions.get(&id)}
    pub(crate) fn catalog_revision(&self) -> u64 { self.catalog_revision }
    pub fn entity_records(&self, entity: EntityId) -> &[ComponentRecord] {
        self.entries.get(entity.index as usize).and_then(Option::as_ref)
            .filter(|(id,_)| *id == entity).map_or(&[], |(_,r)| r.as_ref())
    }
    pub fn capture(&mut self, world: &World) -> Result<ComponentCaptureReport, ComponentError> {
        let store = &world.components;
        if self.source.as_ref().is_some_and(|s| !Arc::ptr_eq(s, &store.source)) { return Err(ComponentError::WrongWorld); }
        if self.tick().is_some_and(|tick| world.tick() <= tick) { return Err(ComponentError::State(SyncError::StaleTick)); }
        self.spatial.capture(world)?;
        self.public_views.get_mut().unwrap().clear();
        if self.source.is_none()||self.catalog_revision!=store.catalog_revision{
            self.definitions=store.definitions.iter().map(|(&id,e)|(id,e.state.clone())).collect();self.catalog_revision=store.catalog_revision;
        }
        let rebuilt = self.source.is_none() || store.journal.front().is_some_and(|&(revision,_)| self.revision < revision - 1);
        self.incremental_base = (!rebuilt).then_some(self.revision);
        let mut map_changed = rebuilt;
        self.dirty.clear();
        if rebuilt {
            self.entries.clear();
            for (row, motion) in world.motion_states().enumerate() {
                if !store.rows[row].is_empty() {
                    self.put_entry(motion.entity, &store.rows[row]);
                    self.dirty.push(motion.entity);
                }
            }
        } else {
            self.dirty.extend(store.journal.iter().filter(|(revision,_)| *revision > self.revision).map(|&(_,e)|e));
            self.dirty.sort_unstable_by_key(|&id| key(id)); self.dirty.dedup();
            for at in 0..self.dirty.len() {
                let entity = self.dirty[at];
                map_changed |= self.entity_records(entity).iter().any(|r| r.scope == ComponentScope::Map);
                if let Ok(row) = world.dense_index(entity) {
                    if !store.rows[row].is_empty() {
                        map_changed |= store.rows[row].iter().any(|r| r.scope == ComponentScope::Map);
                        self.put_entry(entity, &store.rows[row]); continue;
                    }
                }
                if let Some(entry) = self.entries.get_mut(entity.index as usize) {
                    // The journal can contain both generations of a reused slot.
                    if entry.as_ref().is_some_and(|(id,_)| *id == entity) { *entry = None; }
                }
            }
        }
        // Map scope is indexed once per changed capture, never by each peer.
        if map_changed {
            self.map_rows.clear();
            for (entity, records) in self.entries.iter().flatten() {
                for &record in records.iter().filter(|r| r.scope == ComponentScope::Map) {
                    self.map_rows.push(VisibleComponent { entity: *entity, record });
                }
            }
        }
        self.count = store.count; self.revision = store.revision; self.source = Some(store.source.clone());
        Ok(ComponentCaptureReport { rebuilt, changed_entities: self.dirty.len(), records: self.count })
    }
    fn put_entry(&mut self, entity: EntityId, records: &[ComponentRecord]) {
        let at = entity.index as usize;
        if self.entries.len() <= at { self.entries.resize_with(at + 1, || None); }
        self.entries[at] = Some((entity, Arc::from(records)));
    }
    fn public_projection(&self, view: ViewRect, visible: &VisibleRows) -> Result<Arc<[VisibleComponent]>, ComponentError> {
        if let Some((_, rows)) = self.public_views.lock().unwrap().iter().find(|(v,_)| *v == view) {
            return Ok(rows.clone());
        }
        let mut rows = Vec::new();
        let mut map = self.map_rows.iter().copied().peekable();
        let nearby = visible.rows.iter().flat_map(|&(entity,_)| self.entity_records(entity).iter()
            .filter(|r| r.scope == ComponentScope::Nearby).map(move |&record| VisibleComponent { entity, record }));
        for value in nearby {
            while map.peek().is_some_and(|m| m.order() < value.order()) { rows.push(map.next().unwrap()); }
            rows.push(value);
            if rows.len() > MAX_COMPONENT_VIEW_RECORDS { return Err(ComponentError::ViewLimit); }
        }
        rows.extend(map);
        if rows.len() > MAX_COMPONENT_VIEW_RECORDS { return Err(ComponentError::ViewLimit); }
        let rows: Arc<[VisibleComponent]> = rows.into();
        let mut cache = self.public_views.lock().unwrap();
        if cache.len() < 8 && !cache.iter().any(|(v,_)| *v == view) { cache.push((view, rows.clone())); }
        Ok(rows)
    }
}

/// Reusable per-connection spatial membership. Owner identity is fixed for the
/// subscription; changing characters requires a fresh selector/sender/stream.
pub struct ComponentSelector { owner: Option<EntityId>, visible: VisibleRows, scratch: Vec<VisibleComponent>,
    source: Option<Arc<()>>, revision: Option<u64>, merged: Vec<VisibleComponent> }
impl ComponentSelector {
    pub fn new(owner: Option<EntityId>) -> Self { Self { owner, visible: VisibleRows::default(), scratch: Vec::new(), source: None, revision: None, merged: Vec::new() } }
    pub fn owner(&self) -> Option<EntityId> { self.owner }
    pub fn select<'a>(&'a mut self, frame: &ComponentFrame, view: ViewRect) -> Result<&'a [VisibleComponent], ComponentError> {
        self.select_cached(frame, view).map(|(rows, _)| rows)
    }
    pub(crate) fn select_cached<'a>(&'a mut self, frame: &ComponentFrame, view: ViewRect) -> Result<(&'a [VisibleComponent], bool), ComponentError> {
        // A successful spatial reuse proves exact membership, including full
        // entity generations. Component revisions cover owner and map scopes.
        let reused = match frame.spatial.collect_rows_cached(view, &mut self.visible) {
            Ok(reused) => reused,
            Err(error) => { self.revision = None; return Err(error.into()); }
        };
        let same_source = self.source.as_ref().zip(frame.source()).is_some_and(|(a,b)| Arc::ptr_eq(a,b));
        if reused && self.revision == Some(frame.revision) && same_source {
            return Ok((&self.scratch, true));
        }
        if reused && same_source && self.revision.is_some() && self.revision == frame.incremental_base {
            self.revision = None;
            self.merged.clear();
            let mut old = 0;
            for &entity in &frame.dirty {
                // Dirty identities are sorted and deduplicated at capture.
                // Remote changes still remove an old Map projection if needed.
                while old < self.scratch.len() && key(self.scratch[old].entity) < key(entity) {
                    self.merged.push(self.scratch[old]); old += 1;
                }
                while old < self.scratch.len() && self.scratch[old].entity == entity { old += 1; }
                let nearby = self.visible.rows.binary_search_by_key(&key(entity), |&(id,_)| key(id)).is_ok();
                for &record in frame.entity_records(entity) {
                    if record.scope == ComponentScope::Map || (record.scope == ComponentScope::Owner && self.owner == Some(entity))
                        || (record.scope == ComponentScope::Nearby && nearby) {
                        self.merged.push(VisibleComponent { entity, record });
                    }
                }
                if self.merged.len() > MAX_COMPONENT_VIEW_RECORDS { return Err(ComponentError::ViewLimit); }
            }
            if self.merged.len() + self.scratch.len() - old > MAX_COMPONENT_VIEW_RECORDS { return Err(ComponentError::ViewLimit); }
            self.merged.extend_from_slice(&self.scratch[old..]);
            let unchanged = self.merged == self.scratch;
            std::mem::swap(&mut self.scratch, &mut self.merged);
            self.revision = Some(frame.revision);
            return Ok((&self.scratch, unchanged));
        }
        self.revision = None;
        self.scratch.clear();
        let public = frame.public_projection(view, &self.visible)?;
        let own = self.owner.into_iter().flat_map(|entity| frame.entity_records(entity).iter()
            .filter(|r| r.scope == ComponentScope::Owner).map(move |&record| VisibleComponent { entity, record }));
        let mut at = 0;
        for value in own {
            let end = at + public[at..].partition_point(|row| row.order() < value.order());
            if self.scratch.len() + end - at + 1 > MAX_COMPONENT_VIEW_RECORDS { self.scratch.clear(); return Err(ComponentError::ViewLimit); }
            self.scratch.extend_from_slice(&public[at..end]); self.scratch.push(value); at = end;
        }
        if self.scratch.len() + public.len() - at > MAX_COMPONENT_VIEW_RECORDS { self.scratch.clear(); return Err(ComponentError::ViewLimit); }
        self.scratch.extend_from_slice(&public[at..]);
        self.source = frame.source.clone(); self.revision = Some(frame.revision);
        Ok((&self.scratch, false))
    }
}
