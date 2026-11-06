//! IP v1 component snapshots/deltas and IQ v1 application ACKs. One immutable
//! pending packet per stream; replaceable snapshots remain separate from IC events.
use std::{sync::Arc,collections::{BTreeMap,BTreeSet}};
use crate::*;
use crate::component_state::ComponentFrame;
const HEADER: usize = 40;
pub const MAX_COMPONENT_PACKET_BYTES: usize = HEADER + MAX_COMPONENT_VIEW_RECORDS * 2 * 106 + MAX_STATIC_DEFINITIONS * (MAX_STATIC_DEFINITION_BYTES+15);
fn error(e: SyncError) -> ComponentError { ComponentError::State(e) }
fn var(out: &mut Vec<u8>, mut n: u32) {
    while n >= 128 { out.push((n as u8 & 127) | 128); n >>= 7; } out.push(n as u8);
}
fn write_key(out: &mut Vec<u8>, value: &VisibleComponent) {
    var(out, value.entity.index); var(out, value.entity.generation);
    out.push(value.record.key.kind as u8); var(out, value.record.key.instance);
}
fn write_fields(out: &mut Vec<u8>, scope: ComponentScope, fields: ComponentFields, set: u16, unset: u16, version:u8) {
    out.push(scope as u8); out.extend_from_slice(&set.to_le_bytes()); out.extend_from_slice(&unset.to_le_bytes());
    if version==2{out.extend_from_slice(&fields.references.to_le_bytes());}
    for i in 0..COMPONENT_FIELDS { if set & (1 << i) != 0 { var(out, fields.words[i]); } }
}
pub struct ComponentSender {
    stream: u64, sequence: u64, tick: Tick, observed_tick: Tick, source: Option<Arc<()>>,
    selector: ComponentSelector, baseline: Vec<VisibleComponent>, staged: Vec<VisibleComponent>,
    packet: Vec<u8>, pending: Option<SyncHeader>, full: bool,
    version:u8,baseline_definitions:BTreeMap<u32,u32>,staged_definitions:BTreeMap<u32,u32>,
    selection_valid: bool, catalog_revision: u64,
}
impl ComponentSender {
    pub fn new(stream: u64, owner: Option<EntityId>) -> Self {
        Self { stream, sequence: 0, tick: Tick(0), observed_tick: Tick(0), source: None, selector: ComponentSelector::new(owner),
            baseline: Vec::new(), staged: Vec::new(), packet: Vec::new(), pending: None, full: true,
            version:1,baseline_definitions:BTreeMap::new(),staged_definitions:BTreeMap::new(), selection_valid: false, catalog_revision: 0 }
    }
    pub fn new_with_definitions(stream:u64,owner:Option<EntityId>)->Self{let mut sender=Self::new(stream,owner);sender.version=2;sender}
    pub fn wire_version(&self)->u8{self.version}
    pub fn pending_packet(&self) -> Option<&[u8]> { self.pending.map(|_| self.packet.as_slice()) }
    pub fn pending_header(&self) -> Option<SyncHeader> { self.pending }
    pub fn request_full(&mut self) { self.full = true; }
    pub fn prepare(&mut self, frame: &ComponentFrame, view: ViewRect) -> Result<Option<&[u8]>, ComponentError> {
        if self.pending.is_some() { return Err(error(SyncError::WaitingForAck)); }
        let tick = frame.tick().ok_or(error(SyncError::NoFrame))?;
        if tick < self.observed_tick { return Err(error(SyncError::StaleTick)); }
        let source = frame.source().ok_or(error(SyncError::NoFrame))?;
        if self.source.as_ref().is_some_and(|s| !Arc::ptr_eq(s, source)) { return Err(ComponentError::WrongWorld); }
        let valid = self.selection_valid; self.selection_valid = false;
        let (selected, reused) = self.selector.select_cached(frame, view)?;
        if valid && reused && !self.full && self.catalog_revision == frame.catalog_revision() {
            self.selection_valid = true; self.observed_tick = tick; return Ok(None);
        }
        self.staged_definitions.clear();
        for value in selected{let mut mask=value.record.fields.references;while mask!=0{let i=mask.trailing_zeros()as usize;mask&=mask-1;
            if self.version!=2{return Err(ComponentError::UnsupportedReferences);}
            let id=value.record.fields.words[i];
            if let std::collections::btree_map::Entry::Vacant(entry) = self.staged_definitions.entry(id) {
                let definition=frame.definition(id).ok_or(ComponentError::MissingDefinition)?;
                entry.insert(definition.revision);
            }
        }}
        let sequence = self.sequence.checked_add(1).ok_or(error(SyncError::LimitExceeded))?;
        self.staged.clear(); self.staged.extend_from_slice(selected);
        let old = if self.full { &[][..] } else { self.baseline.as_slice() };
        self.packet.clear(); self.packet.resize(HEADER, 0);
        let mut definition_count=0u16;
        for (&id,&revision) in &self.staged_definitions{
            if !self.full&&self.baseline_definitions.get(&id)==Some(&revision){continue;}
            let definition=frame.definition(id).expect("validated definition");
            var(&mut self.packet,id);var(&mut self.packet,revision);var(&mut self.packet,definition.bytes.len()as u32);self.packet.extend_from_slice(&definition.bytes);definition_count+=1;
        }
        let (mut a, mut b, mut count) = (0, 0, 0u16);
        while a < old.len() || b < self.staged.len() {
            let before = old.get(a); let after = self.staged.get(b);
            if before.is_some_and(|v| after.is_none_or(|n| v.order() < n.order())) {
                let v = before.unwrap(); write_key(&mut self.packet, v); self.packet.push(0); a += 1; count += 1;
            } else if after.is_some_and(|v| before.is_none_or(|o| v.order() < o.order())) {
                let v = after.unwrap(); write_key(&mut self.packet, v); self.packet.push(1);
                write_fields(&mut self.packet, v.record.scope, v.record.fields, v.record.fields.mask(), 0,self.version); b += 1; count += 1;
            } else {
                let (o, n) = (before.unwrap(), after.unwrap()); a += 1; b += 1;
                if o.record == n.record { continue; }
                let mut set = 0u16; let unset = o.record.fields.present & !n.record.fields.present;
                for i in 0..COMPONENT_FIELDS {
                    if n.record.fields.get(i).is_some() && (o.record.fields.get(i) != n.record.fields.get(i)||((o.record.fields.references^n.record.fields.references)&(1<<i)!=0)) { set |= 1 << i; }
                }
                write_key(&mut self.packet, n); self.packet.push(2);
                write_fields(&mut self.packet, n.record.scope, n.record.fields, set, unset,self.version); count += 1;
            }
        }
        self.source = Some(source.clone());
        self.observed_tick = tick;
        self.selection_valid = true; self.catalog_revision = frame.catalog_revision();
        if count == 0 && definition_count==0 && !self.full { self.packet.clear(); return Ok(None); }
        let h = SyncHeader { stream: self.stream, sequence, base: if self.full { 0 } else { self.sequence }, tick, reset: self.full };
        self.packet[..4].copy_from_slice(&[b'I', b'P', self.version, u8::from(h.reset)]);
        for (offset, n) in [(4,h.stream),(12,h.sequence),(20,h.base),(28,h.tick.0)] { self.packet[offset..offset+8].copy_from_slice(&n.to_le_bytes()); }
        self.packet[36..38].copy_from_slice(&count.to_le_bytes());
        self.packet[38..40].copy_from_slice(&definition_count.to_le_bytes());
        self.pending = Some(h); self.full = false; Ok(Some(&self.packet))
    }
    pub fn acknowledge(&mut self, stream: u64, sequence: u64) -> Result<(), ComponentError> {
        if stream != self.stream { return Err(error(SyncError::WrongStream)); }
        if sequence != 0 && sequence == self.sequence { return Ok(()); }
        let h = self.pending.ok_or(error(SyncError::OutOfOrder))?;
        if sequence != h.sequence { return Err(error(SyncError::OutOfOrder)); }
        std::mem::swap(&mut self.baseline, &mut self.staged); self.staged.clear();
        std::mem::swap(&mut self.baseline_definitions,&mut self.staged_definitions);self.staged_definitions.clear();
        self.sequence = h.sequence; self.tick = h.tick; self.pending = None; self.packet.clear(); Ok(())
    }
    pub fn acknowledge_packet(&mut self, bytes: &[u8]) -> Result<(), ComponentError> {
        let (version,stream, sequence) = decode_component_ack(bytes)?;if version!=self.version{return Err(error(SyncError::Malformed));}self.acknowledge(stream, sequence)
    }
}
pub(crate) fn decode_component_ack(bytes: &[u8]) -> Result<(u8,u64,u64),ComponentError> {
    if bytes.len() != 20 || &bytes[..2] != b"IQ" || ![1,2].contains(&bytes[2]) || bytes[3]!=0 { return Err(error(SyncError::Malformed)); }
    Ok((bytes[2],u64::from_le_bytes(bytes[4..12].try_into().unwrap()),u64::from_le_bytes(bytes[12..20].try_into().unwrap())))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentApplyReport { pub stream: u64, pub sequence: u64, pub tick: Tick, pub duplicate: bool, pub version:u8 }
impl ComponentApplyReport {
    pub fn acknowledgement(self) -> [u8;20] {
        let mut bytes = [0;20]; bytes[..4].copy_from_slice(&[b'I',b'Q',self.version,0]);
        bytes[4..12].copy_from_slice(&self.stream.to_le_bytes()); bytes[12..].copy_from_slice(&self.sequence.to_le_bytes()); bytes
    }
}
struct Reader<'a> { bytes: &'a [u8], at: usize }
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8;N],ComponentError> {
        let bytes = self.bytes.get(self.at..self.at+N).ok_or(error(SyncError::Malformed))?;
        self.at += N; Ok(bytes.try_into().unwrap())
    }
    fn byte(&mut self) -> Result<u8,ComponentError> { Ok(self.take::<1>()?[0]) }
    fn u16(&mut self) -> Result<u16,ComponentError> { Ok(u16::from_le_bytes(self.take()?)) }
    fn u64(&mut self) -> Result<u64,ComponentError> { Ok(u64::from_le_bytes(self.take()?)) }
    fn var(&mut self) -> Result<u32,ComponentError> {
        let mut value = 0;
        for i in 0..5 {
            let byte = self.byte()?;
            if i == 4 && byte > 15 { return Err(error(SyncError::Malformed)); }
            value |= u32::from(byte & 127) << (i * 7);
            if byte & 128 == 0 {
                if i > 0 && byte == 0 { return Err(error(SyncError::Malformed)); }
                return Ok(value);
            }
        }
        Err(error(SyncError::Malformed))
    }
}
pub struct ComponentReplica { stream: u64, sequence: u64, tick: Tick,
    states: Vec<VisibleComponent>, scratch: Vec<VisibleComponent>, last_packet: Vec<u8>,version:u8,definitions:BTreeMap<u32,StaticDefinition> }
impl ComponentReplica {
    pub fn new(stream: u64) -> Self { Self { stream, sequence: 0, tick: Tick(0), states: Vec::new(), scratch: Vec::new(), last_packet: Vec::new(),version:1,definitions:BTreeMap::new() } }
    pub fn new_with_definitions(stream:u64)->Self{let mut replica=Self::new(stream);replica.version=2;replica}
    pub fn definition(&self,id:u32)->Option<&StaticDefinition>{self.definitions.get(&id)}
    pub fn definition_count(&self)->usize{self.definitions.len()}
    pub fn states(&self) -> &[VisibleComponent] { &self.states }
    pub fn sequence(&self) -> u64 { self.sequence }
    pub fn apply(&mut self, bytes: &[u8]) -> Result<ComponentApplyReport,ComponentError> {
        if bytes.len() < HEADER || bytes.len() > MAX_COMPONENT_PACKET_BYTES { return Err(error(SyncError::Malformed)); }
        let mut r = Reader { bytes, at: 0 };
        if r.take::<2>()? != *b"IP" || r.byte()?!=self.version { return Err(error(SyncError::Malformed)); }
        let flags = r.byte()?; if flags > 1 { return Err(error(SyncError::Malformed)); }
        let reset = flags == 1;
        let (stream, sequence, base, tick) = (r.u64()?, r.u64()?, r.u64()?, Tick(r.u64()?));
        let count = r.u16()? as usize;
        let definition_count=r.u16()?as usize;
        if (self.version==1&&definition_count!=0)||definition_count>MAX_STATIC_DEFINITIONS||count > MAX_COMPONENT_VIEW_RECORDS * 2 { return Err(error(SyncError::Malformed)); }
        if stream != self.stream { return Err(error(SyncError::WrongStream)); }
        if sequence != 0 && sequence == self.sequence && bytes == self.last_packet {
            return Ok(ComponentApplyReport { stream, sequence, tick, duplicate: true,version:self.version });
        }
        if sequence <= self.sequence || (reset && base != 0) || (!reset && (base != self.sequence || base == 0 || base.checked_add(1) != Some(sequence))) {
            return Err(error(SyncError::OutOfOrder));
        }
        if tick < self.tick { return Err(error(SyncError::StaleTick)); }
        let mut definitions=if reset{BTreeMap::new()}else{self.definitions.clone()};let mut updated=BTreeSet::new();let mut previous_definition=None;
        for _ in 0..definition_count{
            let id=r.var()?;let revision=r.var()?;let length=r.var()?as usize;
            if previous_definition.is_some_and(|old|old>=id)||revision==0||length>MAX_STATIC_DEFINITION_BYTES||definitions.get(&id).is_some_and(|old:&StaticDefinition|old.revision>=revision){return Err(error(SyncError::Malformed));}
            previous_definition=Some(id);let data=r.bytes.get(r.at..r.at+length).ok_or(error(SyncError::Malformed))?;r.at+=length;
            definitions.insert(id,StaticDefinition{id,revision,bytes:Arc::from(data)});updated.insert(id);
            // Previous references may leave in this packet. Both bounded sets
            // can coexist until final-state validation, then stale ones are freed.
            if definitions.len()>MAX_STATIC_DEFINITIONS*2{return Err(ComponentError::DefinitionLimit);}
        }
        self.scratch.clear();
        let old = if reset { &[][..] } else { self.states.as_slice() };
        let mut cursor = 0; let mut previous = None;
        for _ in 0..count {
            let entity = EntityId { index: r.var()?, generation: r.var()? };
            let kind = match r.byte()? { 1=>ComponentKind::Appearance,2=>ComponentKind::Effect,3=>ComponentKind::Skill,
                4=>ComponentKind::GroundItem,5=>ComponentKind::Attributes,6=>ComponentKind::GroundMembers,_=>return Err(error(SyncError::Malformed)) };
            let key = ComponentKey { kind, instance: r.var()? }; let order = (crate::sync::key(entity),key);
            if previous.is_some_and(|p| p >= order) { return Err(error(SyncError::DuplicateEntity)); } previous = Some(order);
            while cursor < old.len() && old[cursor].order() < order { self.scratch.push(old[cursor]); cursor += 1; }
            if self.scratch.len() > MAX_COMPONENT_VIEW_RECORDS { return Err(ComponentError::ViewLimit); }
            let before = old.get(cursor).filter(|v| v.order() == order).copied();
            if before.is_some() { cursor += 1; }
            let op = r.byte()?;
            if op == 0 { if before.is_none() { return Err(error(SyncError::MissingEntity)); } continue; }
            if op > 2 || (op == 1 && before.is_some()) || (op == 2 && before.is_none()) { return Err(error(SyncError::Malformed)); }
            let scope = match r.byte()? { 1=>ComponentScope::Owner,2=>ComponentScope::Nearby,3=>ComponentScope::Map,_=>return Err(error(SyncError::Malformed)) };
            let (set, unset) = (r.u16()?,r.u16()?);
            let references=if self.version==2{r.u16()?}else{0};
            let mut fields = before.map_or(ComponentFields::default(), |v| v.record.fields);
            if set & unset != 0 || (op == 1 && unset != 0) || unset & !fields.present != 0 { return Err(error(SyncError::Malformed)); }
            for i in 0..COMPONENT_FIELDS {
                if unset & (1 << i) != 0 { fields.remove(i)?; }
                if set & (1 << i) != 0 { fields.set(i,r.var()?)?; }
            }
            if references&!fields.present!=0{return Err(error(SyncError::Malformed));}fields.references=references;
            let record = ComponentRecord { key, scope, fields }; record.validate()?;
            self.scratch.push(VisibleComponent { entity, record });
            if self.scratch.len() > MAX_COMPONENT_VIEW_RECORDS { return Err(ComponentError::ViewLimit); }
        }
        if self.scratch.len() + old.len() - cursor > MAX_COMPONENT_VIEW_RECORDS { return Err(ComponentError::ViewLimit); }
        self.scratch.extend_from_slice(&old[cursor..]);
        if r.at != bytes.len() { return Err(error(SyncError::Malformed)); }
        let mut required=BTreeSet::new();
        for value in &self.scratch{let mut mask=value.record.fields.references;while mask!=0{let i=mask.trailing_zeros()as usize;mask&=mask-1;
            let id=value.record.fields.words[i];if required.insert(id)&&!definitions.contains_key(&id){return Err(ComponentError::MissingDefinition);}
        }}
        if required.len()>MAX_STATIC_DEFINITIONS{return Err(ComponentError::DefinitionLimit);}
        if updated.iter().any(|id|!required.contains(id)){return Err(error(SyncError::Malformed));}
        definitions.retain(|id,_|required.contains(id));
        std::mem::swap(&mut self.states,&mut self.scratch); self.scratch.clear();
        self.definitions=definitions;
        self.sequence = sequence; self.tick = tick; self.last_packet.clear(); self.last_packet.extend_from_slice(bytes);
        Ok(ComponentApplyReport { stream, sequence, tick, duplicate: false,version:self.version })
    }
}
