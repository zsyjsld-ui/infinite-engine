use std::num::NonZeroU16;

use crate::{EntityId, GridPosition, World, WorldError};

/// Per-call work bound, not a per-client or per-tick rate limit.
pub const MAX_MOVEMENT_COMMANDS: usize = 256;

/// Trusted application commands. These types do not define a wire format or
/// grant callers authority over an entity or its speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovementCommand {
    MoveTo {
        entity: EntityId,
        target: GridPosition,
        speed: NonZeroU16,
    },
    NavigateTo {
        entity: EntityId,
        target: GridPosition,
        speed: NonZeroU16,
    },
    /// Accepted means queued. Read EntityState.navigation for the later outcome.
    QueueNavigation {
        entity: EntityId,
        target: GridPosition,
        speed: NonZeroU16,
    },
    Stop {
        entity: EntityId,
    },
    /// Cancel only pending planning; the suspended old movement may resume.
    CancelNavigation {
        entity: EntityId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandBatchError {
    TooManyCommands,
    ResultsBufferTooSmall,
}

pub(crate) fn validate_command_batch(count: usize, capacity: usize) -> Result<(), CommandBatchError> {
    if count > MAX_MOVEMENT_COMMANDS {
        return Err(CommandBatchError::TooManyCommands);
    }
    if capacity < count {
        return Err(CommandBatchError::ResultsBufferTooSmall);
    }
    Ok(())
}

impl World {
    /// Apply a bounded batch in slice order, including repeated entity commands.
    ///
    /// Batch-size and output-capacity errors leave both world and results intact.
    /// Once admitted, each command has its own result: an individual failure
    /// leaves that command's state unchanged and does not stop later commands.
    /// This is not an all-or-nothing transaction.
    ///
    /// Only results[..commands.len()] is written; the output buffer is caller
    /// owned and reusable. Success means intent accepted, not arrival. No tick
    /// is advanced here, so multiple calls do not grant extra movement distance.
    pub fn apply_movement_commands(
        &mut self,
        commands: &[MovementCommand],
        results: &mut [Result<(), WorldError>],
    ) -> Result<usize, CommandBatchError> {
        validate_command_batch(commands.len(), results.len())?;
        for (command, result) in commands.iter().zip(results.iter_mut()) {
            *result = match *command {
                MovementCommand::MoveTo { entity, target, speed } => {
                    self.move_to(entity, target, speed)
                }
                MovementCommand::NavigateTo { entity, target, speed } => {
                    self.navigate_to(entity, target, speed)
                }
                MovementCommand::QueueNavigation { entity, target, speed } => {
                    self.queue_navigation(entity, target, speed)
                }
                MovementCommand::Stop { entity } => self.stop_moving(entity),
                MovementCommand::CancelNavigation { entity } => self.cancel_navigation(entity).map(|_| ()),
            };
        }
        Ok(commands.len())
    }
}
