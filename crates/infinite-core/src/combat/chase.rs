use std::num::{NonZeroU16, NonZeroU32};
use crate::{EntityId, GridPosition, MovementStatus, NavigationCancelReason, NavigationStatus, PathError, Tick, World, WorldError};
use super::{AttackError, AttackProfile, CombatReport, within_attack_range};

/// Counts admission attempts, including queue-full failures, per combat phase.
pub const MAX_CHASE_REQUESTS_PER_TICK: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChaseConfig {
    pub speed: NonZeroU16,
    pub repath_ticks: NonZeroU16,
    pub retry_ticks: NonZeroU16,
    /// Target displacement must exceed this radius to replace a moving route.
    pub retarget_distance: NonZeroU32,
}
impl Default for ChaseConfig {
    fn default() -> Self {
        Self { speed:NonZeroU16::new(2240).unwrap(), repath_ticks:NonZeroU16::new(6).unwrap(),
            retry_ticks:NonZeroU16::new(10).unwrap(), retarget_distance:NonZeroU32::new(32).unwrap() }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChaseFailure { Request(WorldError), Path(PathError), Cancelled(NavigationCancelReason), MovementBlocked }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChaseStatus { Ready, Waiting, Following, InRange, Throttled, Retrying(ChaseFailure), Interrupted }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChaseState {
    pub config: ChaseConfig,
    pub status: ChaseStatus,
    pub last_goal: Option<GridPosition>,
    pub next_request: Tick,
}
#[derive(Clone, Copy)]
pub(super) struct ChaseRuntime { pub(super) state: ChaseState, pub(super) owns_motion: bool }
impl ChaseRuntime {
    pub(super) fn new(config: ChaseConfig, tick: Tick) -> Self {
        Self { state:ChaseState { config,status:ChaseStatus::Ready,last_goal:None,next_request:tick },owns_motion:false }
    }
}

impl World {
    /// Opt-in chase speed/policy; stationary start_attack retains its old meaning.
    /// A new chase takes movement control. Repeated identical intents preserve
    /// path progress and retry timers; an interrupted chase can be resumed here.
    pub fn start_chase_attack(&mut self, attacker: EntityId, target: EntityId,
        profile: AttackProfile, chase: ChaseConfig) -> Result<(),AttackError> {
        self.start_attack_impl(attacker,target,profile,Some(chase))
    }
    pub(super) fn stop_owned_chase(&mut self, row: usize) {
        let Some(index) = self.combat.rows[row].map(|n|usize::from(n.get()-1)) else { return; };
        if self.combat.active[index].chase.is_some_and(|c|c.owns_motion) {
            self.halt_movement_at(row);
            self.combat.active[index].chase.as_mut().unwrap().owns_motion = false;
        }
    }
    pub(super) fn chase_in_range(&mut self, row: usize) {
        let index = usize::from(self.combat.rows[row].unwrap().get()-1);
        if self.combat.active[index].chase.is_some_and(|c|c.state.status != ChaseStatus::Interrupted) {
            self.stop_owned_chase(row);
            self.combat.active[index].chase.as_mut().unwrap().state.status = ChaseStatus::InRange;
        }
    }
    // A second bounded active-list pass rotates admission priority independently
    // of attack order. Repeated failures at the list head cannot monopolize all
    // 32 attempts. No extra queue of stale/deleted entity handles is retained.
    pub(super) fn service_chases(&mut self, report: &mut CombatReport) {
        if self.combat.chase_count == 0 { self.combat.chase_cursor = 0; return; }
        let count = self.combat.active.len();
        if count == 0 { self.combat.chase_cursor = 0; return; }
        let start = self.combat.chase_cursor % count;
        for offset in 0..count {
            let index = (start+offset)%count;
            let active = self.combat.active[index];
            if active.chase.is_none_or(|c|c.state.status == ChaseStatus::Interrupted) { continue; }
            let target_row = self.dense_index(active.intent.target).ok();
            // Another attack later in the first pass may have killed this actor
            // or its target. Halt its route without changing this pass's list.
            if self.health.read(active.row).map_or(true,|h|h.is_depleted()) ||
                target_row.is_none_or(|row|self.health.read(row).map_or(true,|h|h.is_depleted())) {
                self.stop_owned_chase(active.row); continue;
            }
            let goal = self.position_at(target_row.unwrap());
            if within_attack_range(self.position_at(active.row),goal,active.intent.profile.range) { continue; }
            let before = report.chase_requests;
            self.update_chase(active.row,goal,&mut report.chase_requests,&mut report.chase_deferred);
            if before < MAX_CHASE_REQUESTS_PER_TICK && report.chase_requests == MAX_CHASE_REQUESTS_PER_TICK {
                self.combat.chase_cursor = (index+1)%count;
            }
        }
    }
    pub(super) fn update_chase(&mut self, row: usize, goal: GridPosition, requests: &mut usize, deferred: &mut usize) {
        let index = usize::from(self.combat.rows[row].unwrap().get()-1);
        let active = self.combat.active[index];
        let Some(mut chase) = active.chase else { return; };
        if chase.state.status == ChaseStatus::Interrupted { return; }
        let tick = self.tick();
        let (navigation,movement) = self.navigation_motion_at(row);
        if chase.owns_motion {
            if matches!(navigation, NavigationStatus::Pending { .. }) {
                // Let an admitted search finish, even if its target has moved.
                // Cancelling it each repath interval would starve long routes.
                chase.state.status = ChaseStatus::Waiting;
                self.combat.active[index].chase = Some(chase); return;
            }
            let failure = match navigation {
                NavigationStatus::Failed { error,.. } => Some(ChaseFailure::Path(error)),
                NavigationStatus::Cancelled { reason,.. } => Some(ChaseFailure::Cancelled(reason)),
                _ if movement == MovementStatus::Blocked => Some(ChaseFailure::MovementBlocked),
                _ => None,
            };
            if let Some(failure) = failure {
                self.stop_owned_chase(row);
                chase.owns_motion = false;
                chase.state.status = ChaseStatus::Retrying(failure);
                chase.state.next_request = Tick(tick.0.saturating_add(u64::from(chase.state.config.retry_ticks.get())));
                self.combat.active[index].chase = Some(chase); return;
            }
            let moved_goal = chase.state.last_goal.is_none_or(|old|
                !within_attack_range(old,goal,chase.state.config.retarget_distance.get()));
            if movement == MovementStatus::Moving && !moved_goal {
                chase.state.status = ChaseStatus::Following;
                self.combat.active[index].chase = Some(chase); return;
            }
            chase.state.status = if movement == MovementStatus::Moving { ChaseStatus::Following } else { ChaseStatus::Ready };
        }
        if tick < chase.state.next_request { self.combat.active[index].chase = Some(chase); return; }
        if *requests == MAX_CHASE_REQUESTS_PER_TICK {
            if !matches!(chase.state.status,ChaseStatus::Retrying(_)) { chase.state.status = ChaseStatus::Throttled; }
            *deferred += 1;
            self.combat.active[index].chase = Some(chase); return;
        }
        *requests += 1;
        // Do not let the queued planner restore a stale chase route on failure.
        self.stop_owned_chase(row);
        chase.owns_motion = false;
        match self.queue_navigation_internal(active.entity,goal,chase.state.config.speed) {
            Ok(()) => {
                chase.owns_motion = true;
                chase.state.last_goal = Some(goal);
                chase.state.status = ChaseStatus::Waiting;
                chase.state.next_request = Tick(tick.0.saturating_add(u64::from(chase.state.config.repath_ticks.get())));
            },
            Err(WorldError::NavigationQueueFull) => {
                // Capacity pressure is not a failed route. Rejoin the rotating
                // admission pass next tick, still within its 32-attempt cap.
                chase.state.status = ChaseStatus::Throttled;
                chase.state.next_request = Tick(tick.0.saturating_add(1));
            },
            Err(error) => {
                chase.state.status = ChaseStatus::Retrying(ChaseFailure::Request(error));
                chase.state.next_request = Tick(tick.0.saturating_add(u64::from(chase.state.config.retry_ticks.get())));
            },
        }
        self.combat.active[index].chase = Some(chase);
    }
}
