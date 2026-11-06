//! Integer absorption and resource-backed shields on the shared damage path.
use std::num::{NonZeroU16, NonZeroU32};
use crate::{EntityId, HealthError, HealthOutcome, ResourceCommand,
    ResourceError, ResourceOutcome, Tick, TickQueue, TickQueueError, TickTaskId, World};

pub const MAX_ACTIVE_SHIELDS: usize = 65_536;
pub const MAX_SHIELD_WORK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShieldProfile {
    Absorb { capacity: NonZeroU32 },
    /// Resource cost per damage = numerator / denominator. Credit survives
    /// between hits, so many small integer hits cost the same as one large hit.
    Mana { numerator: NonZeroU16, denominator: NonZeroU16, minimum_resource: NonZeroU32 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShieldSpec {
    pub source: EntityId, pub target: EntityId,
    pub duration: Option<NonZeroU16>, pub profile: ShieldProfile,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShieldState {
    pub source: EntityId, pub profile: ShieldProfile,
    pub expires_at: Option<Tick>, pub prepaid_units: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShieldError {
    InvalidEntity, Health(HealthError), Resource(ResourceError), AlreadyShielded,
    InsufficientResource, CapacityExhausted, TimeOverflow, InvalidWorkLimit,
    Queue(TickQueueError),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShieldExpired { pub target: EntityId, pub state: ShieldState, pub scheduled_at: Tick }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShieldReport { pub expired: usize, pub overdue: Option<Tick> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageOutcome {
    pub health: HealthOutcome, pub absorbed: u32, pub resource: Option<ResourceOutcome>,
    pub shield_before: Option<ShieldState>, pub shield_after: Option<ShieldState>,
}
#[derive(Clone, Copy)]
struct Record { state: ShieldState, task: Option<TickTaskId> }
pub(crate) struct ShieldColumns {
    states: Vec<Option<Record>>, deadlines: TickQueue<EntityId>, active: usize,
}
impl Default for ShieldColumns {
    fn default() -> Self { Self { states: Vec::new(), deadlines: TickQueue::new(MAX_ACTIVE_SHIELDS).unwrap(), active: 0 } }
}
impl ShieldColumns {
    pub(crate) fn next_due(&self)->Option<Tick>{self.deadlines.peek().map(|t|t.due)}
    pub(crate) fn spawn(&mut self) { self.states.push(None); }
    pub(crate) fn despawn(&mut self, row: usize) { self.remove(row); self.states.swap_remove(row); }
    fn remove(&mut self, row: usize) -> Option<ShieldState> {
        let record = self.states[row].take()?;
        if let Some(id) = record.task { self.deadlines.cancel(id); }
        self.active -= 1; Some(record.state)
    }
    /// Latch resource invalidation immediately, before a later restore can
    /// reactivate a shield that already fell below its threshold.
    pub(crate) fn resource_changed(&mut self, row: usize, current: u32) {
        if self.states[row].is_some_and(|r| matches!(r.state.profile,
            ShieldProfile::Mana { minimum_resource, .. } if current < minimum_resource.get())) {
            self.remove(row);
        }
    }
    pub(crate) fn died(&mut self, row: usize) { self.remove(row); }
}
impl World {
    fn live_shield(&self, row: usize) -> Option<ShieldState> {
        let state = self.shields.states[row]?.state;
        // Grant validates health/resource; every mutating path latches their
        // invalidation immediately. Reads need only test the logical deadline.
        if state.expires_at.is_some_and(|at| at <= self.tick()) { return None; }
        Some(state)
    }
    pub fn shield(&self, entity: EntityId) -> Result<Option<ShieldState>, ShieldError> {
        let row = self.dense_index(entity).map_err(|_| ShieldError::InvalidEntity)?;
        Ok(self.live_shield(row))
    }
    /// Eligibility (profession/map/PvP) and converting overheal to capacity are
    /// application rules. An effective existing shield is not silently refreshed.
    pub fn grant_shield(&mut self, spec: ShieldSpec) -> Result<ShieldState, ShieldError> {
        self.dense_index(spec.source).map_err(|_| ShieldError::InvalidEntity)?;
        let row = self.dense_index(spec.target).map_err(|_| ShieldError::InvalidEntity)?;
        if self.health.read(row).map_err(ShieldError::Health)?.is_depleted() { return Err(ShieldError::Health(HealthError::Depleted)); }
        if let ShieldProfile::Mana { minimum_resource, .. } = spec.profile {
            if self.resource.read(row).map_err(ShieldError::Resource)?.current < minimum_resource.get() { return Err(ShieldError::InsufficientResource); }
        }
        if self.live_shield(row).is_some() { return Err(ShieldError::AlreadyShielded); }
        let expires_at = spec.duration.map(|d| self.tick().0.checked_add(u64::from(d.get())).map(Tick).ok_or(ShieldError::TimeOverflow)).transpose()?;
        let old = self.shields.states[row];
        if old.is_none() && self.shields.active == MAX_ACTIVE_SHIELDS { return Err(ShieldError::CapacityExhausted); }
        let old_task = old.and_then(|r| r.task);
        if expires_at.is_some() && old_task.is_none() { self.shields.deadlines.check_schedule().map_err(ShieldError::Queue)?; }
        let task = match (expires_at, old_task) {
            (Some(due), Some(id)) => { self.shields.deadlines.reschedule(id, due).unwrap(); Some(id) },
            (Some(due), None) => Some(self.shields.deadlines.schedule(due, spec.target).expect("preflighted shield deadline")),
            (None, Some(id)) => { self.shields.deadlines.cancel(id); None },
            (None, None) => None,
        };
        let state = ShieldState { source: spec.source, profile: spec.profile, expires_at, prepaid_units: 0 };
        self.shields.states[row] = Some(Record { state, task });
        if old.is_none() { self.shields.active += 1; } Ok(state)
    }
    pub fn clear_shield(&mut self, entity: EntityId) -> Result<Option<ShieldState>, ShieldError> {
        let row = self.dense_index(entity).map_err(|_| ShieldError::InvalidEntity)?; Ok(self.shields.remove(row))
    }
    /// Expired shields never absorb even if this notification phase is delayed.
    pub fn resolve_shields(&mut self, limit: usize, output: &mut Vec<ShieldExpired>) -> Result<ShieldReport, ShieldError> {
        if limit > MAX_SHIELD_WORK { return Err(ShieldError::InvalidWorkLimit); }
        output.clear(); output.reserve(limit);
        while output.len() < limit {
            let Some(task) = self.shields.deadlines.pop_due(self.tick()) else { break; };
            let row = self.dense_index(task.value).expect("despawn removes shield deadline");
            let state = self.shields.remove(row).expect("deadline owns shield");
            output.push(ShieldExpired { target: task.value, state, scheduled_at: task.due });
        }
        Ok(ShieldReport { expired: output.len(), overdue: self.shields.deadlines.peek().map(|t| t.due).filter(|t| *t <= self.tick()) })
    }
    /// Final mitigated integer damage, BEFORE clamping to current health. All
    /// basic/area/periodic hits use this path. Raw HealthCommand stays an explicit
    /// already-settled health edit and therefore intentionally bypasses shields.
    pub fn apply_damage(&mut self, target: EntityId, amount: u32) -> Result<DamageOutcome, HealthError> {
        let row = self.dense_index(target).map_err(|_| HealthError::InvalidEntity)?;
        self.damage_at(row, amount)
    }
    pub(crate) fn damage_at(&mut self, row: usize, amount: u32) -> Result<DamageOutcome, HealthError> {
        let before = self.health.read(row)?;
        if before.is_depleted() { return Err(HealthError::Depleted); }
        let shield_before = self.shields.states[row].map(|r| r.state);
        let mut absorbed = 0; let mut resource = None;
        if let Some(mut state) = self.live_shield(row) {
            match state.profile {
                ShieldProfile::Absorb { capacity } => {
                    absorbed = amount.min(capacity.get());
                    if let Some(capacity) = NonZeroU32::new(capacity.get() - absorbed) {
                        state.profile = ShieldProfile::Absorb { capacity }; self.shields.states[row].as_mut().unwrap().state = state;
                    } else { self.shields.remove(row); }
                },
                ShieldProfile::Mana { numerator, denominator, .. } => {
                    let available = self.resource.read(row).expect("mana shield retains resource").current;
                    let n = u64::from(numerator.get()); let d = u64::from(denominator.get()); let credit = u64::from(state.prepaid_units);
                    absorbed = u64::from(amount).min((u64::from(available)*d+credit)/n) as u32;
                    let units = u64::from(absorbed)*n;
                    let cost = units.saturating_sub(credit).div_ceil(d) as u32;
                    state.prepaid_units = (u64::from(cost)*d+credit-units) as u16;
                    self.shields.states[row].as_mut().unwrap().state = state;
                    if cost != 0 { resource = Some(self.resource_at(row, ResourceCommand::Spend(cost)).expect("bounded mana charge")); }
                },
            }
        } else { self.shields.remove(row); }
        let health = self.health.damage_validated(row, before, amount - absorbed);
        if health.depleted() { self.shields.died(row); self.ai.died(row); self.lifecycle.died(row,self.tick()); self.population.release(row,self.tick()); }
        Ok(DamageOutcome { health, absorbed, resource, shield_before, shield_after: self.shields.states[row].map(|r| r.state) })
    }
    pub(crate) fn resource_at(&mut self, row: usize, command: ResourceCommand) -> Result<ResourceOutcome, ResourceError> {
        let outcome = self.resource.apply(row, command)?;
        self.shields.resource_changed(row, outcome.after.current); Ok(outcome)
    }
}
