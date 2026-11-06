use crate::commands::validate_command_batch;
use crate::state::validate_state_batch;
use crate::{
    CommandBatchError, EntityId, EntityState, MovementCommand, StateReadError,
    NavigationBudget, NavigationProgress, PathError, Tick, World, WorldError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepError {
    Commands(CommandBatchError),
    StateRead(StateReadError),
    TickExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepReport {
    pub tick: Tick,
    pub accepted_commands: usize,
    pub rejected_commands: usize,
    /// Result slots written, including individual invalid-entity errors.
    pub states_written: usize,
    pub moving_entities: usize,
    /// Entire completed tick's allowance, including calls made before step.
    pub navigation_budget: NavigationBudget,
    /// Budget rejections in this call's command batch only.
    pub navigation_budget_rejections: usize,
    pub navigation_progress: NavigationProgress,
}

impl World {
    /// Apply commands, advance exactly one tick, then read requested entity state.
    ///
    /// All batch bounds, output capacities and tick exhaustion are checked before
    /// modifying the world or either output. Admitted commands still have
    /// individual results; failed commands do not roll back successful ones or
    /// prevent the tick. Invalid read IDs likewise produce individual errors.
    /// Empty commands still advance queued navigation and movement by one tick.
    /// No timer, network or renderer is started; the host supplies the cadence.
    pub fn step(
        &mut self,
        commands: &[MovementCommand],
        command_results: &mut [Result<(), WorldError>],
        entities: &[EntityId],
        states: &mut [Result<EntityState, WorldError>],
    ) -> Result<StepReport, StepError> {
        validate_command_batch(commands.len(), command_results.len()).map_err(StepError::Commands)?;
        validate_state_batch(entities.len(), states.len()).map_err(StepError::StateRead)?;
        self.tick().0.checked_add(1).ok_or(StepError::TickExhausted)?;

        // Preflight remains valid: these synchronous core calls invoke no user
        // callbacks, and commands cannot modify the clock or buffer capacities.
        self.apply_movement_commands(commands, command_results).expect("preflight checked commands");
        let tick = self.advance_tick().expect("preflight checked tick capacity");
        let state_report = self.read_entity_states(entities, states).expect("preflight checked state output");
        let accepted = command_results[..commands.len()].iter().filter(|result| result.is_ok()).count();
        Ok(StepReport {
            tick,
            accepted_commands: accepted,
            rejected_commands: commands.len() - accepted,
            states_written: state_report.written,
            moving_entities: self.moving_count(),
            navigation_budget: self.last_navigation_budget(),
            navigation_progress: self.last_navigation_progress(),
            navigation_budget_rejections: command_results[..commands.len()].iter()
                .filter(|result| matches!(result, Err(WorldError::Navigation(PathError::BudgetExhausted))))
                .count(),
        })
    }
}
