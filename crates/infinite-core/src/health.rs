use std::num::{NonZeroU16, NonZeroU32};

use crate::{EntityId, Tick, World};

/// A per-call bound, not a per-tick or per-client rate limit.
pub const MAX_HEALTH_COMMANDS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthState {
    pub current: u32,
    pub maximum: NonZeroU32,
}
impl HealthState {
    pub fn is_depleted(self) -> bool { self.current == 0 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthError { InvalidEntity, Unconfigured, AlreadyConfigured, AboveMaximum, Depleted, NotDepleted }

/// Trusted, already-resolved amounts. Armor, shields, attribution and skill
/// formulas belong to the application before issuing these commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthCommand {
    Damage { entity: EntityId, amount: u32 },
    Heal { entity: EntityId, amount: u32 },
    Revive { entity: EntityId, current: NonZeroU32 },
    /// Preserve current health, clamping down if the maximum shrinks.
    SetMaximum { entity: EntityId, maximum: NonZeroU32 },
}
impl HealthCommand {
    pub fn entity(self) -> EntityId {
        match self {
            Self::Damage{entity,..} | Self::Heal{entity,..} |
            Self::Revive{entity,..} | Self::SetMaximum{entity,..} => entity,
        }
    }
}

/// One outcome per admitted command, including no-ops. Preserve these ordered
/// outcomes for gameplay events; final changes alone can hide intermediate death.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthOutcome { pub before: HealthState, pub after: HealthState }
impl HealthOutcome {
    pub fn depleted(self) -> bool { self.before.current != 0 && self.after.current == 0 }
    pub fn revived(self) -> bool { self.before.current == 0 && self.after.current != 0 }
    pub fn changed(self) -> bool { self.before != self.after }
}

/// Pure numeric settlement for applications that retain transactional storage.
/// The caller publishes this outcome once at its existing commit boundary.
/// No World, event, shield, or lifecycle is mutated. The command's entity is
/// intentionally unused; access control and storage identity remain the host's.
pub fn settle_health(before: HealthState, command: HealthCommand) -> Result<HealthOutcome, HealthError> {
    if before.current > before.maximum.get() { return Err(HealthError::AboveMaximum); }
    let mut after = before;
    match command {
        HealthCommand::Damage{amount,..} => after.current = before.current.saturating_sub(amount),
        HealthCommand::Heal{amount,..} => {
            if before.is_depleted() { return Err(HealthError::Depleted); }
            after.current += amount.min(before.maximum.get() - before.current);
        },
        HealthCommand::Revive{current,..} => {
            if !before.is_depleted() { return Err(HealthError::NotDepleted); }
            if current.get() > before.maximum.get() { return Err(HealthError::AboveMaximum); }
            after.current = current.get();
        },
        HealthCommand::SetMaximum{maximum,..} => {
            after.maximum = maximum; after.current = before.current.min(maximum.get());
        },
    }
    Ok(HealthOutcome { before, after })
}

/// First state and final state for one entity in this batch. Not an event log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthChange { pub entity: EntityId, pub before: HealthState, pub after: HealthState }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthBatchError { TooManyCommands, ResultsBufferTooSmall, ChangesBufferTooSmall }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthBatchReport {
    pub tick: Tick,
    pub processed: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub changed_entities: usize,
}

/// Separate dense columns: no attributes map, per-entity heap allocation or
/// background scan. A zero maximum denotes an entity without health.
#[derive(Default)]
pub(crate) struct HealthColumns {
    current: Vec<u32>,
    maximum: Vec<u32>,
    // One-based position in this call's output; cleared before returning.
    changed_rows: Vec<Option<NonZeroU16>>,
}
impl HealthColumns {
    pub(crate) fn spawn(&mut self) {
        self.current.push(0); self.maximum.push(0); self.changed_rows.push(None);
    }
    pub(crate) fn despawn(&mut self, row: usize) {
        self.current.swap_remove(row); self.maximum.swap_remove(row); self.changed_rows.swap_remove(row);
    }
    pub(crate) fn read(&self, row: usize) -> Result<HealthState, HealthError> {
        Ok(HealthState { current: self.current[row], maximum: NonZeroU32::new(self.maximum[row]).ok_or(HealthError::Unconfigured)? })
    }
    /// The damage phase already read and validated this row. No callbacks or
    /// membership changes occur between shield settlement and this commit.
    pub(crate) fn damage_validated(&mut self, row: usize, before: HealthState, amount: u32) -> HealthOutcome {
        let HealthOutcome {after,..} = settle_health(before,HealthCommand::Damage{entity:EntityId{index:0,generation:0},amount})
            .expect("validated health state");
        if after.current != before.current { self.current[row] = after.current; }
        HealthOutcome { before, after }
    }
    pub(crate) fn apply(&mut self, row: usize, command: HealthCommand) -> Result<HealthOutcome, HealthError> {
        let before = self.read(row)?;
        let HealthOutcome {after,..} = settle_health(before,command)?;
        if before != after {
            self.current[row] = after.current; self.maximum[row] = after.maximum.get();
        }
        Ok(HealthOutcome { before, after })
    }
    pub(crate) fn record_change(&mut self, row: usize, entity: EntityId, outcome: HealthOutcome,
        changes: &mut [HealthChange], changed: &mut usize) {
        if !outcome.changed() { return; }
        if let Some(index) = self.changed_rows[row] {
            changes[usize::from(index.get()-1)].after = outcome.after;
        } else {
            changes[*changed] = HealthChange { entity, before:outcome.before, after:outcome.after };
            *changed += 1;
            self.changed_rows[row] = Some(NonZeroU16::new(u16::try_from(*changed)
                .expect("bounded changes")).expect("nonempty changes"));
        }
    }
}

impl World {
    /// Attach once to a live entity. Allows zero current health for loading a
    /// depleted entity. Reconfiguration goes through explicit batch commands.
    /// This bootstrap operation is not included in subsequent batch changes.
    pub fn configure_health(&mut self, entity: EntityId, current: u32, maximum: NonZeroU32) -> Result<(), HealthError> {
        let row = self.dense_index(entity).map_err(|_| HealthError::InvalidEntity)?;
        if self.health.maximum[row] != 0 { return Err(HealthError::AlreadyConfigured); }
        if current > maximum.get() { return Err(HealthError::AboveMaximum); }
        self.health.current[row] = current; self.health.maximum[row] = maximum.get();
        Ok(())
    }
    pub fn health(&self, entity: EntityId) -> Result<HealthState, HealthError> {
        let row = self.dense_index(entity).map_err(|_| HealthError::InvalidEntity)?;
        self.health.read(row)
    }

    /// Resolve in input order into caller-owned reusable buffers. Validate all
    /// buffer capacities before any mutation; individual errors do not roll back
    /// other commands. Both buffers must hold commands.len() entries.
    ///
    /// Final changes are coalesced in first-change order, with net-zero entries
    /// removed. Only results[..processed] and changes[..changed_entities] are
    /// meaningful; the remaining change buffer is scratch. No tick is advanced,
    /// no movement is stopped, and no global event/history queue is maintained.
    pub fn apply_health_commands(
        &mut self,
        commands: &[HealthCommand],
        results: &mut [Result<HealthOutcome, HealthError>],
        changes: &mut [HealthChange],
    ) -> Result<HealthBatchReport, HealthBatchError> {
        if commands.len() > MAX_HEALTH_COMMANDS { return Err(HealthBatchError::TooManyCommands); }
        if results.len() < commands.len() { return Err(HealthBatchError::ResultsBufferTooSmall); }
        if changes.len() < commands.len() { return Err(HealthBatchError::ChangesBufferTooSmall); }
        let mut changed = 0;
        let mut accepted = 0;
        for (&command,result) in commands.iter().zip(results.iter_mut()) {
            let entity = command.entity();
            let applied = self.dense_index(entity).map_err(|_| HealthError::InvalidEntity)
                .and_then(|row| self.health.apply(row,command).map(|outcome| (row,outcome)));
            match applied {
                Err(error) => *result = Err(error),
                Ok((row,outcome)) => {
                    accepted += 1;
                    if outcome.depleted() { self.shields.died(row); self.ai.died(row); self.lifecycle.died(row,self.tick()); self.population.release(row,self.tick()); }
                    if outcome.revived() { self.ai.revived(row); self.lifecycle.revived(row); }
                    *result = Ok(outcome);
                    self.health.record_change(row,entity,outcome,changes,&mut changed);
                },
            }
        }
        let written = self.finish_health_changes(changes,changed);
        Ok(HealthBatchReport { tick:self.tick(), processed:commands.len(), accepted,
            rejected:commands.len()-accepted, changed_entities:written })
    }

    pub(crate) fn finish_health_changes(&mut self, changes: &mut [HealthChange], changed: usize) -> usize {
        let mut written = 0;
        for index in 0..changed {
            let change = changes[index];
            // No command changes entity membership, and callbacks are absent.
            let row = self.dense_index(change.entity).expect("batch entity still exists");
            self.health.changed_rows[row] = None;
            if change.before != change.after { changes[written] = change; written += 1; }
        }
        written
    }
}
