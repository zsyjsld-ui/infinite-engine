use crate::{EntityId, GridPosition, MovementStatus, NavigationStatus, NavigationWait, Tick, World, WorldError};

pub const MAX_STATE_READS: usize = 256;

/// Application-facing state, not a byte layout or a network authorization grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntityState {
    pub entity: EntityId,
    pub position: GridPosition,
    pub movement: MovementStatus,
    pub target: Option<GridPosition>,
    pub navigation: NavigationStatus,
    /// Pending-request details; None once completed, failed or cancelled.
    pub navigation_wait: Option<NavigationWait>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateReadError {
    TooManyEntities,
    ResultsBufferTooSmall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateReadReport {
    pub tick: Tick,
    /// Number of result slots written, including invalid-entity errors.
    pub written: usize,
}

pub(crate) fn validate_state_batch(count: usize, capacity: usize) -> Result<(), StateReadError> {
    if count > MAX_STATE_READS {
        return Err(StateReadError::TooManyEntities);
    }
    if capacity < count {
        return Err(StateReadError::ResultsBufferTooSmall);
    }
    Ok(())
}

impl World {
    /// Read in requested order into a reusable buffer without modifying the world.
    /// Repeated IDs are not deduplicated; stale IDs yield an individual error.
    /// A batch error leaves the output intact; unused output slots are untouched.
    /// All rows share the reported tick, but this is not a transition/event log.
    pub fn read_entity_states(
        &self,
        entities: &[EntityId],
        results: &mut [Result<EntityState, WorldError>],
    ) -> Result<StateReadReport, StateReadError> {
        validate_state_batch(entities.len(), results.len())?;
        for (&entity, result) in entities.iter().zip(results.iter_mut()) {
            *result = self.entity_state(entity);
        }
        Ok(StateReadReport { tick: self.tick(), written: entities.len() })
    }
}
