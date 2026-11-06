//! Dense integer attributes with an explicit dirty-only derivation phase.
use std::num::{NonZeroU16, NonZeroU32};
use crate::{EntityId, HealthCommand, HealthError, HealthOutcome, Tick, World};

pub const ATTRIBUTE_COUNT: usize = 5;
pub const MAX_DIRTY_ATTRIBUTES: usize = 4096;
pub const ATTRIBUTE_RATE_SCALE: i64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum AttributeKind { Attack, BreakPower, Defense, MaximumHealth, MaximumResource }

/// App-resolved base values. Profession, equipment tables and primary-stat
/// growth remain outside the core; level is available to the armor curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatAttributes {
    pub attack: u32,
    pub break_power: u32,
    pub defense: u32,
    pub maximum_health: NonZeroU32,
    pub maximum_resource: u32,
    pub level: NonZeroU16,
}

/// Aggregated additive modifiers. A rate of 200 means +2%, -10000 means -100%.
/// Application/effect code owns source attribution; replace the aggregate, do
/// not reverse a previously clamped derived value when removing a modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttributeModifiers {
    pub flat: [i64; ATTRIBUTE_COUNT],
    pub rate: [i64; ATTRIBUTE_COUNT],
}
impl AttributeModifiers {
    pub fn set(&mut self, kind: AttributeKind, flat: i64, rate: i64) {
        self.flat[kind as usize] = flat; self.rate[kind as usize] = rate;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeError {
    InvalidEntity, Unconfigured, AlreadyConfigured, Dirty, CapacityExhausted,
    Health(HealthError), InvalidIgnoreRate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributeChange {
    pub entity: EntityId,
    pub before: Option<CombatAttributes>,
    pub after: CombatAttributes,
    pub health: HealthOutcome,
    pub resource: Option<crate::ResourceOutcome>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributeReport { pub tick: Tick, pub recomputed: usize, pub changed: usize }

#[derive(Clone, Copy)]
struct DirtyAttribute { entity: EntityId, row: usize }
#[derive(Default)]
pub(crate) struct AttributeColumns {
    base: Vec<Option<CombatAttributes>>,
    modifiers: Vec<AttributeModifiers>,
    effects: Vec<AttributeModifiers>,
    derived: Vec<Option<CombatAttributes>>,
    dirty_rows: Vec<Option<NonZeroU16>>,
    dirty: Vec<DirtyAttribute>,
}
impl AttributeColumns {
    pub(crate) fn spawn(&mut self) {
        self.base.push(None); self.modifiers.push(AttributeModifiers::default());
        self.effects.push(AttributeModifiers::default());
        self.derived.push(None); self.dirty_rows.push(None);
    }
    pub(crate) fn despawn(&mut self, row: usize) {
        if let Some(index) = self.dirty_rows[row].take() {
            let index = usize::from(index.get()-1);
            self.dirty.swap_remove(index);
            if let Some(moved) = self.dirty.get(index) {
                self.dirty_rows[moved.row] = NonZeroU16::new((index+1) as u16);
            }
        }
        self.base.swap_remove(row); self.modifiers.swap_remove(row);
        self.effects.swap_remove(row);
        self.derived.swap_remove(row); self.dirty_rows.swap_remove(row);
        if let Some(Some(index)) = self.dirty_rows.get(row) {
            self.dirty[usize::from(index.get()-1)].row = row;
        }
    }
    fn mark(&mut self, row: usize, entity: EntityId) -> Result<(), AttributeError> {
        if self.dirty_rows[row].is_some() { return Ok(()); }
        if self.dirty.len() == MAX_DIRTY_ATTRIBUTES { return Err(AttributeError::CapacityExhausted); }
        self.dirty.push(DirtyAttribute { entity, row });
        self.dirty_rows[row] = NonZeroU16::new(self.dirty.len() as u16);
        Ok(())
    }
    pub(crate) fn prepare_effect(&mut self,row:usize,entity:EntityId)->Result<(),AttributeError>{
        self.base[row].ok_or(AttributeError::Unconfigured)?;self.mark(row,entity)
    }
    pub(crate) fn adjust_effect(&mut self,row:usize,flat:[i32;ATTRIBUTE_COUNT],rate:[i32;ATTRIBUTE_COUNT],sign:i64){
        // Effect admission caps each entity at 64 sources with i32 contributions.
        for i in 0..ATTRIBUTE_COUNT{self.effects[row].flat[i]+=i64::from(flat[i])*sign;self.effects[row].rate[i]+=i64::from(rate[i])*sign;}
    }
}

fn derive(base: CombatAttributes, modifier: AttributeModifiers, effect: AttributeModifiers) -> CombatAttributes {
    let field = |value: u32, kind: AttributeKind, minimum: u32| {
        let index = kind as usize;
        if modifier.flat[index] == 0 && modifier.rate[index] == 0 && effect.flat[index] == 0 && effect.rate[index] == 0 { return value.max(minimum); }
        // Clamp only at the documented stages, using wide intermediates even
        // for i64-extreme trusted aggregates. Never multiply two signed i64s.
        let flat = (i128::from(value)+i128::from(modifier.flat[index])+i128::from(effect.flat[index])).max(0) as u64;
        let rate = (i128::from(ATTRIBUTE_RATE_SCALE)+i128::from(modifier.rate[index])+i128::from(effect.rate[index])).max(0) as u64;
        if let Some(product) = flat.checked_mul(rate) {
            (product/10_000).clamp(u64::from(minimum),u64::from(u32::MAX)) as u32
        } else {
            (u128::from(flat)*u128::from(rate)/10_000).clamp(u128::from(minimum),u128::from(u32::MAX)) as u32
        }
    };
    CombatAttributes {
        attack: field(base.attack,AttributeKind::Attack,0),
        break_power: field(base.break_power,AttributeKind::BreakPower,0),
        defense: field(base.defense,AttributeKind::Defense,0),
        maximum_health: NonZeroU32::new(field(base.maximum_health.get(),AttributeKind::MaximumHealth,1)).unwrap(),
        maximum_resource: field(base.maximum_resource,AttributeKind::MaximumResource,0),
        level: base.level,
    }
}

impl World {
    /// Health must be configured first. Attachment and subsequent edits mark
    /// one dirty row; readers reject pending values until resolve_attributes.
    pub fn configure_attributes(&mut self, entity: EntityId, base: CombatAttributes) -> Result<(),AttributeError> {
        let row = self.dense_index(entity).map_err(|_|AttributeError::InvalidEntity)?;
        if self.attributes.base[row].is_some() { return Err(AttributeError::AlreadyConfigured); }
        self.health.read(row).map_err(AttributeError::Health)?;
        self.attributes.mark(row,entity)?;
        self.attributes.base[row] = Some(base);
        Ok(())
    }
    pub fn set_base_attributes(&mut self, entity: EntityId, base: CombatAttributes) -> Result<bool,AttributeError> {
        let row = self.dense_index(entity).map_err(|_|AttributeError::InvalidEntity)?;
        let old = self.attributes.base[row].ok_or(AttributeError::Unconfigured)?;
        if old == base { return Ok(false); }
        self.attributes.mark(row,entity)?; self.attributes.base[row] = Some(base); Ok(true)
    }
    pub fn set_attribute_modifiers(&mut self, entity: EntityId, modifiers: AttributeModifiers) -> Result<bool,AttributeError> {
        let row = self.dense_index(entity).map_err(|_|AttributeError::InvalidEntity)?;
        self.attributes.base[row].ok_or(AttributeError::Unconfigured)?;
        if self.attributes.modifiers[row] == modifiers { return Ok(false); }
        self.attributes.mark(row,entity)?; self.attributes.modifiers[row] = modifiers; Ok(true)
    }
    pub fn combat_attributes(&self, entity: EntityId) -> Result<CombatAttributes,AttributeError> {
        let row = self.dense_index(entity).map_err(|_|AttributeError::InvalidEntity)?;
        if self.attributes.dirty_rows[row].is_some() { return Err(AttributeError::Dirty); }
        self.attributes.derived[row].ok_or(AttributeError::Unconfigured)
    }
    pub fn pending_attribute_updates(&self) -> usize { self.attributes.dirty.len() }

    /// Recompute only dirty entities, in first-dirty order (despawn may swap
    /// pending entries). Clears output; returns one record per actual change.
    /// Max-health changes clamp current HP down, never heal or revive. Capture
    /// outgoing state after this phase. A later direct SetMaximum command is an
    /// app override until the next derivation; don't use competing max-HP owners.
    pub fn resolve_attributes(&mut self, output: &mut Vec<AttributeChange>) -> AttributeReport {
        output.clear(); output.reserve(self.attributes.dirty.len());
        let recomputed = self.attributes.dirty.len();
        for dirty in &self.attributes.dirty {
            let row = dirty.row;
            let before = self.attributes.derived[row];
            let after = derive(self.attributes.base[row].unwrap(),self.attributes.modifiers[row],self.attributes.effects[row]);
            let health = self.health.apply(row,HealthCommand::SetMaximum {
                entity: dirty.entity, maximum: after.maximum_health,
            }).expect("configured attributes retain health");
            let resource = self.resource.derive_maximum(row,after.maximum_resource);
            if let Some(resource) = resource { self.shields.resource_changed(row,resource.after.current); }
            self.attributes.derived[row] = Some(after); self.attributes.dirty_rows[row] = None;
            if before != Some(after) || health.changed() || resource.is_some_and(|r|r.changed()) {
                output.push(AttributeChange { entity: dirty.entity, before, after, health, resource });
            }
        }
        self.attributes.dirty.clear();
        AttributeReport { tick: self.tick(), recomputed, changed: output.len() }
    }
}

/// Data-driven rational armor curve. For the legacy physical rule, the app
/// supplies base=50, per_level=2, armor_rate_bps=600 (0.06). No game constant is
/// hardcoded. Poison/logarithmic resistance is a different rule, not this curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmorCurve {
    pub base: NonZeroU32,
    pub per_level: u16,
    pub armor_rate_bps: u16,
}

/// Remaining defense = max(0, defense*(1-ignore) - break power). Round the final
/// positive damage to nearest integer, ties up, minimum one. Zero stays zero.
/// Exact rational arithmetic: checked u64 common path, u128 product fallback.
pub fn physical_damage(raw: u32, defense: u32, break_power: u32, level: NonZeroU16,
    ignore_defense_bps: u16, curve: ArmorCurve) -> Result<u32,AttributeError>
{
    if ignore_defense_bps > 10_000 { return Err(AttributeError::InvalidIgnoreRate); }
    if raw == 0 { return Ok(0); }
    let remaining = (u64::from(defense)*u64::from(10_000-ignore_defense_bps))
        .saturating_sub(u64::from(break_power)*10_000);
    if remaining == 0 || curve.armor_rate_bps == 0 { return Ok(raw); }
    // Given the u32/u16 inputs, scale and denominator fit u64 even at maxima.
    // Only raw*scale (and its rounding addition) may need the wider fallback.
    let scale = (u64::from(curve.base.get())+u64::from(curve.per_level)*u64::from(level.get()))*100_000_000;
    let denominator = scale+remaining*u64::from(curve.armor_rate_bps);
    if let Some(rounded) = u64::from(raw).checked_mul(scale).and_then(|v|v.checked_add(denominator/2)) {
        Ok((rounded/denominator).max(1) as u32)
    } else {
        let numerator = u128::from(raw)*u128::from(scale);
        Ok(((numerator+u128::from(denominator/2))/u128::from(denominator)).max(1) as u32)
    }
}
