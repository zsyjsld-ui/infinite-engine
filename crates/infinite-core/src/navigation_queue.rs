use std::num::{NonZeroU16, NonZeroU32};

use crate::pathfinding::PathSearch;
use crate::{DEFAULT_NAVIGATION_WORK_LIMIT, EntityId, GridPosition, MAX_PATH_WAYPOINTS, PathError, Tick};

/// Bounds retained searches as well as queue length, not just admission per call.
pub const MAX_PENDING_NAVIGATIONS: usize = 32;
pub const NAVIGATION_WORK_SLICE: u32 = 8192;
pub const DEFAULT_QUEUED_WORK_RESERVE: u32 = 32768;
/// Must cover the largest atomic route conversion, including its starting point.
pub const MIN_NAVIGATION_WORK_SLICE: u32 = 4 * (MAX_PATH_WAYPOINTS as u32 + 1) + 1;
pub const MAX_NAVIGATION_CANCELLATIONS: usize = 256;

/// Validated once when creating a World; values cannot change mid-tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationConfig {
    pub work_per_tick: u32,
    pub work_per_slice: u32,
    pub queued_work_reserve: u32,
    pub queue_capacity: usize,
    /// None means no expiry. Includes both queue waiting and active search ticks.
    pub max_wait_ticks: Option<NonZeroU32>,
}

impl Default for NavigationConfig {
    fn default() -> Self {
        Self {
            work_per_tick: DEFAULT_NAVIGATION_WORK_LIMIT,
            work_per_slice: NAVIGATION_WORK_SLICE,
            queued_work_reserve: DEFAULT_QUEUED_WORK_RESERVE,
            queue_capacity: MAX_PENDING_NAVIGATIONS,
            max_wait_ticks: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationConfigError {
    QueueCapacityOutOfRange,
    SliceTooSmall,
    ReserveBelowSlice,
    TickBudgetBelowReserve,
}

impl NavigationConfig {
    pub fn validate(&self) -> Result<(), NavigationConfigError> {
        if !(1..=MAX_PENDING_NAVIGATIONS).contains(&self.queue_capacity) {
            return Err(NavigationConfigError::QueueCapacityOutOfRange);
        }
        if self.work_per_slice < MIN_NAVIGATION_WORK_SLICE {
            return Err(NavigationConfigError::SliceTooSmall);
        }
        if self.queued_work_reserve < self.work_per_slice {
            return Err(NavigationConfigError::ReserveBelowSlice);
        }
        if self.work_per_tick < self.queued_work_reserve {
            return Err(NavigationConfigError::TickBudgetBelowReserve);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationPhase { Queued, CheckingDirect, Connecting, Searching, Tracing, BuildingPath }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationWait {
    pub enqueued_at: Tick,
    pub waited_ticks: u64,
    /// Current zero-based round-robin position, not an estimated completion time.
    pub queue_position: usize,
    pub phase: NavigationPhase,
    pub work_used: u64,
    pub movement_suspended: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationCancelReason { Explicit, Stopped, Replaced, Relocated, Despawned, TimedOut }

/// A bounded record survives replacement of the snapshot or destruction of an entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationCancellation {
    pub entity: EntityId,
    pub target: GridPosition,
    pub reason: NavigationCancelReason,
    pub at_tick: Tick,
    pub waited_ticks: u64,
}

/// Latest queued-planning state, separate from the suspended/active movement.
/// Ready means a route was installed, not that the entity reached its goal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavigationStatus {
    #[default]
    Idle,
    Pending { target: GridPosition },
    Ready { target: GridPosition },
    Failed { target: GridPosition, error: PathError },
    Cancelled { target: GridPosition, reason: NavigationCancelReason },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavigationProgress {
    pub completed: usize,
    pub failed: usize,
    pub pending: usize,
    pub work_used: u32,
    pub timed_out: usize,
    /// Completed requests that reused an exact start/goal route in this World.
    pub cached: usize,
    /// Completed paths assembled from a shared reverse goal search.
    pub shared: usize,
}

pub(crate) struct NavigationJob {
    pub entity: EntityId,
    pub origin: GridPosition,
    pub target: GridPosition,
    pub speed: NonZeroU16,
    pub search: PathSearch,
    pub smooth:bool,
    pub enqueued_at: Tick,
    pub work_used: u64,
}

// World owns an immutable map. Exact integer endpoints are sufficient keys;
// speed, entity identity and movement cursors never enter the cached value.
// FIFO bounds retained waypoint storage to 16 * MAX_PATH_WAYPOINTS points.
pub(crate) const ROUTE_CACHE_CAPACITY: usize = 16;
pub(crate) struct CachedRoute {
    pub origin: GridPosition,
    pub target: GridPosition,
    pub path: crate::PlannedPath,
    pub smooth:bool,
}
