//! Integer world state and simulation time for Infinite Engine.
//!
//! Integer movement, static collision and bounded grid navigation are included.
//! Motion replication, integer health and an explicit tick attack phase are
//! included; application combat formulas and rendering are not included.

#![forbid(unsafe_code)]

mod collision;
mod combat;
mod health;
mod action_cadence;
pub use action_cadence::{ActionCadence,CadenceGrace,CadenceTiming,CadenceDeadline,CadenceReadiness,CadenceError,MAX_CADENCE_SKILLS,MAX_CADENCE_GROUPS};
mod commands;
mod movement;
mod directional;
pub use directional::{DirectionRemainder,DirectionError,IntegerSlide,slide_integer,DIRECTION_SCALE,MAX_DIRECTION_MICROS};
mod navigation_budget;
mod navigation_queue;
mod pathfinding;
mod state;
mod step;
mod world;
mod sync;
mod sync_wire;
mod combat_sync;
mod combat_sync_wire;
mod combat_query;
mod combat_actions;
mod segment;
pub use segment::{SegmentSweep,CombatSegmentQuery,CombatSegmentHit,MAX_SEGMENT_AXIS};
mod projectiles;
pub use projectiles::{Projectiles,ProjectilePhase,ProjectileId,ProjectileCastId,ProjectileLaunch,ProjectileState,ProjectileImpact,ProjectileEnd,ProjectileEndReason,ProjectileError,ProjectileReport,MAX_PROJECTILES,MAX_PROJECTILE_CASTS,MAX_PROJECTILE_HITS,MAX_CAST_PROJECTILE_HITS};
mod attributes;
mod resource;
mod tick_queue;
mod skills;
mod effects;
mod shields;
mod ai;
mod lifecycle;
mod population;
mod map_activity;
mod server_budget;
pub use server_budget::{ServerWorkBudget,ServerBudgetConfig,ServerLaneBudget,ServerWorkLane,
    ServerWorkTicket,ServerWorkGrant,ServerBudgetReport,ServerBudgetError,SERVER_WORK_LANES,MAX_BUDGET_MAPS};
mod network;
mod network_window;
mod network_motion;
mod network_component;
pub use network_component::ComponentConnectionStatus;
pub use network_motion::MotionConnectionStatus;
mod observation;
mod component_state;
mod component_projection;
mod component_wire;
mod component_catalog;
pub use component_catalog::{StaticDefinition,MAX_STATIC_DEFINITIONS,MAX_STATIC_DEFINITION_BYTES};
pub use component_wire::{ComponentSender,ComponentReplica,ComponentApplyReport,MAX_COMPONENT_PACKET_BYTES};
pub use component_projection::{AppearanceComponent,GroundItemComponent,ground_members_component,MAX_GROUND_OWNER_MEMBERS};
pub use component_state::{ComponentKind,ComponentScope,ComponentKey,ComponentFields,ComponentRecord,
    ComponentError,VisibleComponent,ComponentFrame,ComponentCaptureReport,ComponentSelector,
    COMPONENT_FIELDS,MAX_COMPONENTS_PER_ENTITY,MAX_COMPONENT_RECORDS,MAX_COMPONENT_VIEW_RECORDS,COMPONENT_JOURNAL_CAPACITY};

pub use collision::{CollisionMap, MAX_COLLISION_CELLS, MapError};
pub use combat::{AttackProfile, AttackIntent, AttackWindup, AttackState, AttackError, CombatEndReason,
    CombatEventKind, CombatEvent, CombatPhaseError, CombatReport, MAX_ACTIVE_COMBATANTS,
    within_attack_range};
pub use combat::{ChaseConfig, ChaseState, ChaseStatus, ChaseFailure, MAX_CHASE_REQUESTS_PER_TICK};
pub use health::{settle_health, HealthState, HealthError, HealthCommand, HealthOutcome, HealthChange,
    HealthBatchError, HealthBatchReport, MAX_HEALTH_COMMANDS};
pub use commands::{CommandBatchError, MAX_MOVEMENT_COMMANDS, MovementCommand};
pub use movement::{DEFAULT_TICK_MS, MovementStatus, POSITION_UNITS_PER_PIXEL};
pub use navigation_budget::{DEFAULT_NAVIGATION_WORK_LIMIT, NavigationBudget};
pub use navigation_queue::{
    DEFAULT_QUEUED_WORK_RESERVE, MAX_NAVIGATION_CANCELLATIONS, MAX_PENDING_NAVIGATIONS,
    MIN_NAVIGATION_WORK_SLICE, NAVIGATION_WORK_SLICE, NavigationCancellation, NavigationCancelReason,
    NavigationConfig, NavigationConfigError, NavigationPhase, NavigationProgress, NavigationStatus, NavigationWait,
};
pub use pathfinding::{MAX_PATH_SEARCH_NODES, MAX_PATH_WAYPOINTS, PathError, PathPlanner, PlannedPath,PathTask};
pub use state::{EntityState, MAX_STATE_READS, StateReadError, StateReadReport};
pub use step::{StepError, StepReport};
pub use world::{World, WorldError, ControlledRouteSample, RouteCheckpoint};
pub use movement::ControlledPresentation;
pub use sync::{MotionState, ViewRect, SpatialFrame, SyncSender, SyncChange, AxisChange, SyncHeader,
    SyncError, MAX_SYNC_ENTITIES, MAX_VISIBLE_ENTITIES};
pub use sync_wire::{Replica, ApplyReport, SYNC_HEADER_BYTES, MAX_SYNC_PACKET_BYTES};
pub use combat_sync::{CombatFrame,CombatSyncSender,CombatSyncConfig,CombatSyncError,CombatObserveReport,VitalState,
    WindupView,CombatFeedback,CombatFeedbackKind,MAX_COMBAT_EVENTS_PER_PACKET,MAX_PENDING_COMBAT_EVENTS};
pub use combat_sync_wire::{CombatReplica,CombatApplyReport,COMBAT_SYNC_HEADER_BYTES,MAX_COMBAT_SYNC_PACKET_BYTES};
pub use combat_query::{CombatArea, CombatQuery, CombatNearestQuery, CombatQueryHit,
    CombatQueryContext, CombatQueryBudget, CombatQueryWork, CombatQueryError,
    CombatQueryReport, CombatNearestResult, MAX_COMBAT_QUERIES,
    MAX_COMBAT_QUERY_HITS, MAX_COMBAT_BATCH_HITS};
pub use combat_actions::{CombatActions, CombatActionPhase, CombatActionError,
    AcquireAttack, AreaDamage, AreaDamageReport};
pub use attributes::{CombatAttributes, AttributeKind, AttributeModifiers, AttributeError,
    AttributeChange, AttributeReport, ArmorCurve, physical_damage,
    ATTRIBUTE_COUNT, ATTRIBUTE_RATE_SCALE, MAX_DIRTY_ATTRIBUTES};
pub use resource::{ResourceState,ResourceOutcome,ResourceCommand,ResourceError};
pub use tick_queue::{TickQueue,TickTask,TickTaskId,TickQueueError,MAX_TICK_TASKS};
pub use skills::{SkillId,SkillRecovery,SkillSpec,CastTarget,CastRequest,CastTicket,
    CastReceipt,CastAdmission,CastCancelReason,SkillEvent,SkillError,SkillState,SkillReport,
    MAX_SKILLS_PER_ENTITY,MAX_RECOVERY_GROUPS,MAX_ACTIVE_CASTS};
pub use effects::{EffectId,PeriodicKind,PeriodicRefresh,PeriodicEffect,BuffModifiers,AttributeBuff,
    EffectDisposition,EffectApplication,EffectState,EffectStateKind,EffectEnd,EffectEventKind,EffectEvent,EffectError,EffectReport,
    MAX_ACTIVE_EFFECTS,MAX_EFFECTS_PER_ENTITY,MAX_EFFECT_WORK};
pub use shields::{ShieldProfile,ShieldSpec,ShieldState,ShieldError,ShieldExpired,ShieldReport,
    DamageOutcome,MAX_ACTIVE_SHIELDS,MAX_SHIELD_WORK};
pub use ai::{AiProfile,AiMode,AiState,AiError,AiThreat,AiReason,AiEvent,AiReport,AiSystem,
    MAX_AI_ACTORS,MAX_AI_TARGETS,MAX_AI_THREATS,MAX_AI_TOTAL_THREATS,MAX_AI_WORK};
pub use lifecycle::{LifecycleProfile,LifecyclePhase,LifecycleState,RespawnTicket,LifecycleEventKind,LifecycleEvent,
    LifecycleError,LifecycleReport,LifecycleActivity,RespawnOutcome,MAX_LIFECYCLE_ENTITIES,MAX_LIFECYCLE_WORK};
pub use population::{SpawnGroupId,SpawnReservation,SpawnTemplate,SpawnBudget,PopulationError,PopulationState,
    MAX_POPULATION_SLOTS,MAX_SPAWN_GROUP_MEMBERS,MAX_SPAWN_SPECIES_GATES,MAX_SPAWN_WORK};
pub use map_activity::{MapWork,MapActivity,MapSleepError};
pub use network::{CombatConnections,ConnectionId,ConnectionStatus,NetworkConfig,NetworkBudget,
    NetworkError,WriteProgress,NetworkNotice,NetworkNoticeKind,NetworkReport,AckDisposition,
    MAX_NETWORK_CONNECTIONS,MAX_NETWORK_WORK};
pub use observation::{ObservationQueue,ObservationTarget,ObservationRecovery,ObservationRestart,ObservationError,
    ObservationFailure,ObservationReport,MAX_OBSERVATION_FRAMES};
pub use network_window::{RetainedCombatSender,RetainedCombatPacket,MAX_COMBAT_WINDOW_PACKETS};

/// A simulation step number. Tick duration belongs to the simulation rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tick(pub u64);

/// An integer position in units of 1/16 world pixel, including negative positions.
///
/// These are fine simulation units, not navigation cells or screen pixels.
/// Projection and interpolation into screen coordinates are client concerns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GridPosition {
    pub x: i32,
    pub y: i32,
}

/// A world-local entity handle, including a generation for slot reuse.
///
/// Handles are valid only within the world that allocated them. They do not
/// define a persistence or network encoding, or identify entities across worlds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntityId {
    pub index: u32,
    pub generation: u32,
}
