use std::num::{NonZeroU16, NonZeroU32};
use std::collections::VecDeque;

use crate::movement::Movement;
use crate::navigation_queue::{NavigationJob, CachedRoute, ROUTE_CACHE_CAPACITY};
use crate::pathfinding::PathSearch;
use crate::{CollisionMap, DEFAULT_TICK_MS, EntityId, EntityState, GridPosition, MovementStatus, Tick};
use crate::{NavigationBudget, PathError, PathPlanner};
use crate::{
    MAX_NAVIGATION_CANCELLATIONS, NavigationCancellation, NavigationCancelReason, NavigationConfig,
    NavigationConfigError, NavigationProgress, NavigationStatus, NavigationWait,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldError {
    EntityCapacityExhausted,
    InvalidEntity,
    TickExhausted,
    BlockedPosition,
    Navigation(PathError),
    NavigationQueueFull,
}

struct Slot {
    generation: u32,
    // One-based dense row; zero represents an unoccupied slot in Option.
    dense: Option<NonZeroU32>,
}

#[derive(Clone, Copy)]
struct ControlledMotion {radius:u32,remainder:u32}

/// Sample for an application-owned route. Forecast is the next 50ms position;
/// route points are a bounded presentation prefix, not a new destination command.
pub struct ControlledRouteSample {
    pub position:GridPosition, pub forecast:GridPosition, pub path:Vec<GridPosition>,
}

/// Exact integer route state for a trusted replica. Includes segment progress
/// and fractional credit: restarting a leg at its rounded current point drifts.
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct RouteCheckpoint {
    pub position:GridPosition, pub origin:GridPosition, pub progress:u64,
    pub remainder:u32, pub radius:u32, pub waypoints:Vec<GridPosition>,
}

/// Single-owner state for one logical world.
///
/// Active coordinates occupy separate, dense columns. Slot indirection keeps
/// entity handles stable when a removed entity is replaced by the last row.
/// This type provides storage, logical time and optional routed movement. It has no
/// wall-clock scheduler or application-specific speed rules. An optional static
/// collision map constrains positions and the segments traversed each tick.
pub struct World {
    tick: Tick,
    tick_ms: NonZeroU32,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    entity_slots: Vec<u32>,
    xs: Vec<i32>,
    ys: Vec<i32>,
    movements: Vec<Option<Movement>>,
    controlled_motion: Vec<Option<ControlledMotion>>,
    movement_statuses: Vec<MovementStatus>,
    pub(crate) health: crate::health::HealthColumns,
    pub(crate) components: crate::component_state::ComponentColumns,
    pub(crate) attributes: crate::attributes::AttributeColumns,
    pub(crate) resource: crate::resource::ResourceColumns,
    pub(crate) skills: crate::skills::SkillColumns,
    pub(crate) effects: crate::effects::EffectColumns,
    pub(crate) shields: crate::shields::ShieldColumns,
    pub(crate) ai: crate::ai::AiColumns,
    pub(crate) lifecycle: crate::lifecycle::LifecycleColumns,
    pub(crate) population: crate::population::PopulationColumns,
    pub(crate) combat: crate::combat::CombatColumns,
    // Active movement rows and the reverse index for constant-time removal.
    moving_entities: Vec<u32>,
    moving_rows: Vec<Option<NonZeroU32>>,
    collision: Option<CollisionMap>,
    path_planner: PathPlanner,
    navigation_budget: NavigationBudget,
    navigation_statuses: Vec<NavigationStatus>,
    navigation_jobs: VecDeque<NavigationJob>,
    navigation_routes: VecDeque<CachedRoute>,
    navigation_goals: crate::pathfinding::SharedGoals,
    last_navigation_budget: NavigationBudget,
    last_navigation_progress: NavigationProgress,
    navigation_config: NavigationConfig,
    navigation_cancellations: VecDeque<NavigationCancellation>,
    dropped_navigation_cancellations: u64,
}

impl Default for World {
    fn default() -> Self {
        Self::new(DEFAULT_TICK_MS)
    }
}

impl World {
    /// The step length is explicit, fixed for this world's lifetime, and nonzero.
    pub fn new(tick_ms: NonZeroU32) -> Self {
        Self::with_navigation_config(tick_ms, NavigationConfig::default()).expect("valid defaults")
    }

    pub fn with_navigation_config(
        tick_ms: NonZeroU32, config: NavigationConfig,
    ) -> Result<Self, NavigationConfigError> {
        config.validate()?;
        Ok(Self {
            tick: Tick(0),
            tick_ms,
            slots: Vec::new(),
            free_slots: Vec::new(),
            entity_slots: Vec::new(),
            xs: Vec::new(),
            ys: Vec::new(),
            movements: Vec::new(),
            controlled_motion: Vec::new(),
            movement_statuses: Vec::new(),
            health: crate::health::HealthColumns::default(),
            components: crate::component_state::ComponentColumns::default(),
            attributes: crate::attributes::AttributeColumns::default(),
            resource: crate::resource::ResourceColumns::default(),
            skills: crate::skills::SkillColumns::default(),
            effects: crate::effects::EffectColumns::default(),
            shields: crate::shields::ShieldColumns::default(),
            ai: crate::ai::AiColumns::default(),
            lifecycle: crate::lifecycle::LifecycleColumns::default(),
            population: crate::population::PopulationColumns::default(),
            combat: crate::combat::CombatColumns::default(),
            moving_entities: Vec::new(),
            moving_rows: Vec::new(),
            collision: None,
            path_planner: PathPlanner::default(),
            navigation_budget: NavigationBudget::new(config.work_per_tick),
            navigation_statuses: Vec::new(),
            navigation_jobs: VecDeque::new(),
            navigation_routes: VecDeque::new(),
            navigation_goals: crate::pathfinding::SharedGoals::default(),
            last_navigation_budget: NavigationBudget::new(config.work_per_tick),
            last_navigation_progress: NavigationProgress::default(),
            navigation_config: config,
            navigation_cancellations: VecDeque::new(),
            dropped_navigation_cancellations: 0,
        })
    }

    /// Create a map-bound world. The map becomes immutable for this first stage.
    pub fn with_map(tick_ms: NonZeroU32, map: CollisionMap) -> Self {
        let mut world = Self::new(tick_ms);
        world.collision = Some(map);
        world
    }

    pub fn with_map_and_navigation_config(
        tick_ms: NonZeroU32, map: CollisionMap, config: NavigationConfig,
    ) -> Result<Self, NavigationConfigError> {
        let mut world = Self::with_navigation_config(tick_ms, config)?;
        world.collision = Some(map);
        Ok(world)
    }

    pub fn navigation_config(&self) -> NavigationConfig { self.navigation_config }

    /// Oldest retained cancellation first, including replaced/despawned requests.
    pub fn pop_navigation_cancellation(&mut self) -> Option<NavigationCancellation> {
        self.navigation_cancellations.pop_front()
    }

    /// Cumulative (saturating) count of oldest records evicted from the bounded log.
    pub fn dropped_navigation_cancellations(&self) -> u64 { self.dropped_navigation_cancellations }

    pub fn collision_map(&self) -> Option<&CollisionMap> {
        self.collision.as_ref()
    }

    pub fn tick(&self) -> Tick {
        self.tick
    }

    pub fn tick_ms(&self) -> NonZeroU32 {
        self.tick_ms
    }

    /// Snapshot of this tick's shared allowance; reading does not replenish it.
    pub fn navigation_budget(&self) -> NavigationBudget {
        self.navigation_budget
    }

    /// Restrict this logical tick's remaining navigation work after global
    /// admission. Never refunds prior work or enlarges the configured budget.
    /// The host calls this before navigation/chase work; advance_tick resets the
    /// next tick normally. Movement and due combat are not delayed by this cap.
    pub fn restrict_navigation_work(&mut self, allowance: u32) -> u32 {
        self.navigation_budget.restrict_remaining(allowance)
    }

    pub fn pending_navigation_count(&self) -> usize { self.navigation_jobs.len() }

    /// Completed tick accounting, retained after the current allowance resets.
    pub fn last_navigation_budget(&self) -> NavigationBudget { self.last_navigation_budget }
    pub fn last_navigation_progress(&self) -> NavigationProgress { self.last_navigation_progress }
    pub(crate) fn has_navigation_feedback(&self)->bool{!self.navigation_cancellations.is_empty()}
    pub(crate) fn jump_idle_tick(&mut self,target:Tick){
        self.last_navigation_progress=NavigationProgress::default();
        self.last_navigation_budget=if target.0-self.tick.0==1{self.navigation_budget}else{NavigationBudget::new(self.navigation_config.work_per_tick)};
        self.navigation_budget=NavigationBudget::new(self.navigation_config.work_per_tick);self.tick=target;
    }

    /// Advance logical time and movement by one fixed step, without wall time.
    /// Tick exhaustion is checked before modifying any entity.
    pub fn advance_tick(&mut self) -> Result<Tick, WorldError> {
        let next = self.tick.0.checked_add(1).ok_or(WorldError::TickExhausted)?;
        self.last_navigation_progress = self.process_navigation_queue();
        let mut cursor = 0;
        while cursor < self.moving_entities.len() {
            let dense = self.moving_entities[cursor] as usize;
            if self.controlled_motion[dense].is_some(){cursor+=1;continue;}
            if matches!(self.navigation_statuses[dense], NavigationStatus::Pending { .. }) {
                cursor += 1;
                continue; // Hold the search origin; no movement credit accrues.
            }
            let from = GridPosition { x: self.xs[dense], y: self.ys[dense] };
            let active = self.movements[dense].as_mut().expect("indexed movement must exist");
            let advanced = active.advance(self.tick_ms, from, self.collision.as_ref());
            let arrived = active.arrived();
            let position = match advanced {
                Ok(position) => position,
                Err(()) => {
                    self.clear_movement(dense);
                    self.movement_statuses[dense] = MovementStatus::Blocked;
                    continue;
                }
            };
            self.xs[dense] = position.x;
            self.ys[dense] = position.y;
            self.ai.position_changed(dense, position);
            if arrived {
                self.clear_movement(dense);
                self.movement_statuses[dense] = MovementStatus::Arrived;
                continue;
            }
            // Removal fills this cursor from the unprocessed tail, so only
            // advance the cursor when the current entity remains in the list.
            cursor += 1;
        }
        self.tick = Tick(next);
        self.last_navigation_budget = self.navigation_budget;
        self.navigation_budget = NavigationBudget::new(self.navigation_config.work_per_tick);
        Ok(self.tick)
    }

    pub fn len(&self) -> usize {
        self.entity_slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entity_slots.is_empty()
    }

    /// Movement intents, including sub-unit speed and temporarily suspended routes.
    pub fn moving_count(&self) -> usize {
        self.moving_entities.len()
    }

    /// Create an entity, reusing an available slot before growing the slot table.
    pub fn spawn(&mut self, position: GridPosition) -> Result<EntityId, WorldError> {
        self.require_walkable(position)?;
        let dense_row = u32::try_from(self.entity_slots.len())
            .ok()
            .and_then(|row| row.checked_add(1))
            .and_then(NonZeroU32::new)
            .ok_or(WorldError::EntityCapacityExhausted)?;
        let index = match self.free_slots.pop() {
            Some(index) => index,
            None => {
                let index = u32::try_from(self.slots.len())
                    .map_err(|_| WorldError::EntityCapacityExhausted)?;
                self.slots.push(Slot { generation: 0, dense: None });
                index
            }
        };
        self.entity_slots.push(index);
        self.xs.push(position.x);
        self.ys.push(position.y);
        self.movements.push(None);
        self.controlled_motion.push(None);
        self.movement_statuses.push(MovementStatus::Idle);
        self.health.spawn();
        self.components.spawn();
        self.attributes.spawn();
        self.resource.spawn();
        self.skills.spawn();
        self.effects.spawn();
        self.shields.spawn();
        self.ai.spawn();
        self.lifecycle.spawn();
        self.population.spawn();
        self.combat.spawn();
        self.navigation_statuses.push(NavigationStatus::Idle);
        self.moving_rows.push(None);
        let slot = &mut self.slots[index as usize];
        slot.dense = Some(dense_row);
        Ok(EntityId { index, generation: slot.generation })
    }

    /// Remove an entity and invalidate its handle. A generation-exhausted slot
    /// is retired permanently instead of wrapping and accepting an ancient ID.
    pub fn despawn(&mut self, entity: EntityId) -> Result<(), WorldError> {
        let dense = self.dense_index(entity)?;
        let dense_row = self.slots[entity.index as usize].dense;
        self.cancel_navigation_at(dense, NavigationCancelReason::Despawned);
        self.clear_movement(dense);
        self.combat.despawn(dense);
        self.entity_slots.swap_remove(dense);
        self.xs.swap_remove(dense);
        self.ys.swap_remove(dense);
        self.movements.swap_remove(dense);
        self.controlled_motion.swap_remove(dense);
        self.movement_statuses.swap_remove(dense);
        self.health.despawn(dense);
        self.components.despawn(dense,entity);
        self.attributes.despawn(dense);
        self.resource.despawn(dense);
        self.skills.despawn(dense);
        self.effects.despawn(dense);
        self.shields.despawn(dense);
        self.ai.despawn(dense);
        self.lifecycle.despawn(dense);
        self.population.despawn(dense,self.tick);
        self.navigation_statuses.swap_remove(dense);
        self.moving_rows.swap_remove(dense);
        if let Some(&moved_index) = self.entity_slots.get(dense) {
            self.slots[moved_index as usize].dense = dense_row;
            // The last entity changed dense row; repair its movement-list entry.
            if let Some(row) = self.moving_rows[dense] {
                self.moving_entities[(row.get() - 1) as usize] = dense as u32;
            }
        }
        let slot = &mut self.slots[entity.index as usize];
        slot.dense = None;
        if let Some(next_generation) = slot.generation.checked_add(1) {
            slot.generation = next_generation;
            self.free_slots.push(entity.index);
        }
        Ok(())
    }

    pub fn position(&self, entity: EntityId) -> Result<GridPosition, WorldError> {
        let dense = self.dense_index(entity)?;
        Ok(self.position_at(dense))
    }

    pub(crate) fn position_at(&self, dense: usize) -> GridPosition {
        GridPosition { x: self.xs[dense], y: self.ys[dense] }
    }

    /// Read the complete public movement state with one entity-handle lookup.
    pub fn entity_state(&self, entity: EntityId) -> Result<EntityState, WorldError> {
        let dense = self.dense_index(entity)?;
        Ok(EntityState {
            entity,
            position: GridPosition { x: self.xs[dense], y: self.ys[dense] },
            movement: self.movement_statuses[dense],
            target: self.movements[dense].as_ref().map(|movement| movement.target),
            navigation: self.navigation_statuses[dense],
            navigation_wait: self.navigation_wait_at(entity, dense),
        })
    }

    /// A trusted state write, not a player movement command. Callers must apply
    /// their authority rules before using this API. A configured map checks the
    /// destination only, permitting trusted relocation across obstacles.
    /// Successful relocation cancels the target and fractional credit, resetting
    /// movement status to Idle rather than reporting a normal arrival or stop.
    pub fn set_position(
        &mut self,
        entity: EntityId,
        position: GridPosition,
    ) -> Result<(), WorldError> {
        let dense = self.dense_index(entity)?;
        self.require_walkable(position)?;
        self.cancel_navigation_at(dense, NavigationCancelReason::Relocated);
        self.combat.interrupt_chase(dense);
        self.xs[dense] = position.x;
        self.ys[dense] = position.y;
        self.ai.position_changed(dense, position);
        self.clear_movement(dense);
        self.controlled_motion[dense]=None;
        self.movement_statuses[dense] = MovementStatus::Idle;
        Ok(())
    }

    /// Set a trusted target and speed in position units/second (1..=65535).
    /// This changes intent only; position changes on the next advance_tick.
    /// A configured map checks the destination now and each traversed segment
    /// during advance_tick. No pathfinding or client speed authorization is done.
    pub fn move_to(
        &mut self,
        entity: EntityId,
        target: GridPosition,
        speed: NonZeroU16,
    ) -> Result<(), WorldError> {
        let dense = self.dense_index(entity)?;
        self.require_walkable(target)?;
        self.cancel_navigation_at(dense, NavigationCancelReason::Replaced);
        self.combat.interrupt_chase(dense);
        self.controlled_motion[dense]=None;
        let origin = GridPosition { x: self.xs[dense], y: self.ys[dense] };
        if origin == target {
            self.clear_movement(dense);
            self.movement_statuses[dense] = MovementStatus::Arrived;
            return Ok(());
        }
        let mut remainder = 0;
        if let Some(active) = &mut self.movements[dense] {
            // Repeated commands and speed changes must not reset segment progress.
            if active.target == target && !active.planned {
                active.speed = speed;
                return Ok(());
            }
            remainder = active.remainder;
        }
        self.movements[dense] = Movement::new(origin, target, speed, remainder);
        self.index_movement(dense);
        self.movement_statuses[dense] = MovementStatus::Moving;
        Ok(())
    }

    /// Plan and follow a static-grid route, using a straight fast path if clear.
    /// Planning errors (including Unreachable) preserve the prior movement.
    /// All new plans share this world's tick allowance, even across batches.
    /// BudgetExhausted is a rejection, not a queued or resumable request.
    /// The snapshot target remains the final goal, not an intermediate waypoint.
    pub fn navigate_to(
        &mut self, entity: EntityId, target: GridPosition, speed: NonZeroU16,
    ) -> Result<(), WorldError> {
        let dense = self.dense_index(entity)?;
        self.require_walkable(target)?;
        let origin = GridPosition { x: self.xs[dense], y: self.ys[dense] };
        if origin == target {
            self.combat.interrupt_chase(dense);
            self.cancel_navigation_at(dense, NavigationCancelReason::Replaced);
            self.controlled_motion[dense]=None;
            self.clear_movement(dense);
            self.movement_statuses[dense] = MovementStatus::Arrived;
            return Ok(());
        }
        let mut remainder = 0;
        if let Some(active) = &mut self.movements[dense] {
            if active.target == target && active.planned {
                active.speed = speed;
                self.controlled_motion[dense]=None;
                self.combat.interrupt_chase(dense);
                self.cancel_navigation_at(dense, NavigationCancelReason::Replaced);
                return Ok(());
            }
            remainder = active.remainder;
        }
        // Pending work gets a protected share on subsequent ticks, even if the
        // host keeps issuing synchronous navigation before advance_tick.
        let reserve = if self.navigation_jobs.is_empty() { 0 } else { self.navigation_config.queued_work_reserve };
        let mut budget = NavigationBudget::new(self.navigation_budget.remaining().saturating_sub(reserve));
        let planned = (|| -> Result<Option<Movement>, WorldError> {
            budget.charge(1).map_err(WorldError::Navigation)?;
            let needs_detour = match self.collision.as_ref() {
                Some(map) => !map.navigation_segment_is_clear(origin, target, &mut budget)
                    .map_err(WorldError::Navigation)?,
                None => false,
            };
            if needs_detour {
                let map = self.collision.as_ref().expect("detour needs map");
                let path = self.path_planner.plan_detour(map, origin, target, &mut budget)
                    .map_err(WorldError::Navigation)?;
                Ok(Movement::from_path(origin, path, speed, remainder))
            } else {
                let mut movement = Movement::new(origin, target, speed, remainder);
                if let Some(active) = &mut movement { active.planned = true; }
                Ok(movement)
            }
        })();
        self.navigation_budget.charge(budget.used()).expect("slice fits world allowance");
        let movement = planned?;
        self.controlled_motion[dense]=None;
        self.combat.interrupt_chase(dense);
        self.cancel_navigation_at(dense, NavigationCancelReason::Replaced);
        self.movements[dense] = movement;
        self.index_movement(dense);
        self.movement_statuses[dense] = MovementStatus::Moving;
        Ok(())
    }

    /// Queue resumable navigation; success is admission, not a planning result.
    /// The old movement is suspended (without accruing time credit) until this
    /// request succeeds, fails, or is cancelled by a successful movement command.
    pub fn queue_navigation(
        &mut self, entity: EntityId, target: GridPosition, speed: NonZeroU16,
    ) -> Result<(), WorldError> {
        self.queue_navigation_internal(entity,target,speed)?;
        let dense = self.dense_index(entity).expect("accepted live entity");
        self.controlled_motion[dense]=None;
        self.combat.interrupt_chase(dense);
        Ok(())
    }

    /// Same resumable queue, exact-route cache and shared-goal fields as normal
    /// navigation. World ticks plan the route; only the application advances it.
    pub fn queue_controlled_navigation(&mut self,entity:EntityId,target:GridPosition,speed:NonZeroU16,radius:u32)->Result<(),WorldError>{
        self.queue_navigation_internal(entity,target,speed)?;
        if let Some(job)=self.navigation_jobs.iter_mut().find(|job|job.entity==entity){job.smooth=true;job.search.enable_smoothing();}
        let dense=self.dense_index(entity)?;
        let remainder=self.controlled_motion[dense].map_or(0,|s|s.remainder);
        self.controlled_motion[dense]=Some(ControlledMotion{radius,remainder});
        self.combat.interrupt_chase(dense);Ok(())
    }

    /// Spend this logical tick's existing allowance without advancing time.
    /// Repeated input packets cannot replenish it or discard unfinished searches.
    pub fn plan_pending_navigation(&mut self)->NavigationProgress {self.process_navigation_queue()}

    pub fn controlled_route(&mut self,entity:EntityId,micros:u32,speed:NonZeroU16,preview:bool)->Result<ControlledRouteSample,WorldError>{
        self.controlled_route_sample(entity,micros,speed,preview,true)
    }
    pub fn controlled_presentation(&self,entity:EntityId,micros:u32,speed:NonZeroU16)->Result<crate::movement::ControlledPresentation,WorldError>{
        let dense=self.dense_index(entity)?;
        if micros>200_000{return Err(WorldError::Navigation(PathError::SearchLimit));}
        let from=GridPosition{x:self.xs[dense],y:self.ys[dense]};
        let settings=self.controlled_motion[dense].ok_or(WorldError::InvalidEntity)?;
        let active=self.movements[dense].as_ref().ok_or(WorldError::InvalidEntity)?;
        active.controlled_presentation(from,micros,speed,settings.radius,settings.remainder,self.collision.as_ref()).map_err(|_|WorldError::BlockedPosition)
    }
    /// Local replica needs only its new position and current segment tangent.
    /// It consumes exactly the same budget without a second 50ms forecast/path copy.
    pub fn controlled_route_lite(&mut self,entity:EntityId,micros:u32,speed:NonZeroU16)->Result<ControlledRouteSample,WorldError>{
        self.controlled_route_sample(entity,micros,speed,false,false)
    }
    fn controlled_route_sample(&mut self,entity:EntityId,micros:u32,speed:NonZeroU16,preview:bool,details:bool)->Result<ControlledRouteSample,WorldError>{
        let dense=self.dense_index(entity)?;
        if micros>200_000{return Err(WorldError::Navigation(PathError::SearchLimit));}
        let from=GridPosition{x:self.xs[dense],y:self.ys[dense]};
        let mut settings=self.controlled_motion[dense].ok_or(WorldError::InvalidEntity)?;
        if !matches!(self.navigation_statuses[dense],NavigationStatus::Ready{..})||self.movements[dense].is_none(){
            return Ok(ControlledRouteSample{position:from,forecast:from,path:Vec::new()});
        }
        if preview {
            let active=self.movements[dense].as_ref().unwrap();
            let position=active.controlled_preview(from,micros,speed,settings.radius,settings.remainder,self.collision.as_ref()).map_err(|_|WorldError::BlockedPosition)?;
            return Ok(ControlledRouteSample{position,forecast:position,path:active.remaining_waypoints(8)});
        }
        let outcome=self.movements[dense].as_mut().unwrap().controlled_step(from,micros,speed,settings.radius,&mut settings.remainder,self.collision.as_ref());
        let (position,arrived)=match outcome{Ok(result)=>result,Err(())=>{self.clear_movement(dense);self.movement_statuses[dense]=MovementStatus::Blocked;return Ok(ControlledRouteSample{position:from,forecast:from,path:Vec::new()});}};
        self.xs[dense]=position.x;self.ys[dense]=position.y;self.ai.position_changed(dense,position);
        self.controlled_motion[dense]=Some(settings);
        if arrived {self.clear_movement(dense);self.movement_statuses[dense]=MovementStatus::Arrived;return Ok(ControlledRouteSample{position,forecast:position,path:Vec::new()});}
        let active=self.movements[dense].as_ref().unwrap();
        if !details{return Ok(ControlledRouteSample{position,forecast:active.segment_target(),path:Vec::new()});}
        let forecast=active.controlled_preview(position,50_000,speed,settings.radius,settings.remainder,self.collision.as_ref()).unwrap_or(position);
        Ok(ControlledRouteSample{position,forecast,path:active.remaining_waypoints(8)})
    }

    pub fn route_checkpoint(&self,entity:EntityId)->Result<Option<RouteCheckpoint>,WorldError>{
        let dense=self.dense_index(entity)?;
        let (Some(settings),Some(active))=(self.controlled_motion[dense],self.movements[dense].as_ref())else{return Ok(None)};
        if !matches!(self.navigation_statuses[dense],NavigationStatus::Ready{..}){return Ok(None)}
        let (origin,progress,waypoints)=active.cursor();
        Ok(Some(RouteCheckpoint{position:GridPosition{x:self.xs[dense],y:self.ys[dense]},origin,progress,remainder:settings.remainder,radius:settings.radius,waypoints}))
    }

    /// Application-authorized replication only, never a player movement command.
    /// Validate the entire snapshot before changing any world state.
    pub fn restore_route_checkpoint(&mut self,entity:EntityId,snapshot:RouteCheckpoint)->Result<(),WorldError>{
        let dense=self.dense_index(entity)?;
        if snapshot.waypoints.is_empty()||snapshot.waypoints.len()>crate::MAX_PATH_WAYPOINTS||snapshot.remainder>=1_000_000{
            return Err(WorldError::Navigation(PathError::SearchLimit));
        }
        self.require_walkable(snapshot.position)?;
        let certified=self.controlled_motion[dense].is_some()&&self.movements[dense].as_ref().is_some_and(|m|m.certifies_suffix(snapshot.origin,&snapshot.waypoints));
        if !certified{self.require_walkable(snapshot.origin)?;
            let mut from=snapshot.origin;for &point in &snapshot.waypoints{
                self.require_walkable(point)?;
                if point==from||self.collision.as_ref().is_some_and(|m|!m.segment_is_clear(from,point)){return Err(WorldError::BlockedPosition)}
                from=point;
            }
        }
        let previous=*snapshot.waypoints.last().expect("validated nonempty route");
        let mut movement=Movement::from_waypoints(snapshot.origin,snapshot.waypoints,NonZeroU16::new(1).unwrap(),0).ok_or(WorldError::BlockedPosition)?;
        movement.restore_progress(snapshot.progress,snapshot.position).map_err(|_|WorldError::BlockedPosition)?;
        self.install_route_checkpoint(dense,snapshot.position,movement,ControlledMotion{radius:snapshot.radius,remainder:snapshot.remainder},previous);Ok(())
    }

    /// Copy a validated controlled route within this world/map. The source is
    /// normally the confirmed replica and the destination the predicted copy.
    /// External packets cannot construct a certification or skip validation.
    pub fn copy_route_checkpoint(&mut self,source:EntityId,destination:EntityId)->Result<(),WorldError>{
        let from=self.dense_index(source)?;let to=self.dense_index(destination)?;
        let settings=self.controlled_motion[from].ok_or(WorldError::BlockedPosition)?;
        if !matches!(self.navigation_statuses[from],NavigationStatus::Ready{..}){return Err(WorldError::BlockedPosition);}
        let movement=self.movements[from].as_ref().ok_or(WorldError::BlockedPosition)?.clone();let goal=movement.target;
        let position=GridPosition{x:self.xs[from],y:self.ys[from]};
        if from!=to{self.install_route_checkpoint(to,position,movement,settings,goal);}Ok(())
    }
    fn install_route_checkpoint(&mut self,dense:usize,position:GridPosition,movement:Movement,settings:ControlledMotion,goal:GridPosition){
        self.cancel_navigation_at(dense,NavigationCancelReason::Replaced);
        self.xs[dense]=position.x;self.ys[dense]=position.y;self.ai.position_changed(dense,position);
        self.movements[dense]=Some(movement);
        self.index_movement(dense);
        self.controlled_motion[dense]=Some(settings);
        self.navigation_statuses[dense]=NavigationStatus::Ready{target:goal};self.movement_statuses[dense]=MovementStatus::Moving;
        self.combat.interrupt_chase(dense);
    }

    // Same queue/budget, with ownership handled by the combat controller.
    pub(crate) fn queue_navigation_internal(
        &mut self, entity: EntityId, target: GridPosition, speed: NonZeroU16,
    ) -> Result<(), WorldError> {
        let dense = self.dense_index(entity)?;
        self.require_walkable(target)?;
        let origin = GridPosition { x: self.xs[dense], y: self.ys[dense] };
        if origin == target {
            self.cancel_navigation_at(dense, NavigationCancelReason::Replaced);
            self.clear_movement(dense);
            self.movement_statuses[dense] = MovementStatus::Arrived;
            self.navigation_statuses[dense] = NavigationStatus::Ready { target };
            return Ok(());
        }
        if let Some(job) = self.navigation_jobs.iter_mut().find(|job| job.entity == entity) {
            if job.target == target {
                job.speed = speed; // Preserve search progress and queue position.
                return Ok(());
            }
        } else {
            if let Some(active) = &mut self.movements[dense] {
                if active.planned && active.target == target {
                    active.speed = speed;
                    self.navigation_statuses[dense] = NavigationStatus::Ready { target };
                    return Ok(());
                }
            }
            if self.navigation_jobs.len() >= self.navigation_config.queue_capacity {
                return Err(WorldError::NavigationQueueFull);
            }
        }
        self.cancel_navigation_at(dense, NavigationCancelReason::Replaced);
        self.navigation_jobs.push_back(NavigationJob {
            entity, origin, target, speed, search: PathSearch::new(origin, target),smooth:false,
            enqueued_at: self.tick, work_used: 0,
        });
        self.navigation_statuses[dense] = NavigationStatus::Pending { target };
        Ok(())
    }

    fn process_navigation_queue(&mut self) -> NavigationProgress {
        let mut report = NavigationProgress::default();
        if let Some(max_wait) = self.navigation_config.max_wait_ticks {
            let mut cursor = 0;
            // Expire even when no search budget remains; at most queue_capacity
            // jobs are examined. Cancellation-log storage is separately bounded;
            // this performs no path-search work.
            while cursor < self.navigation_jobs.len() {
                let job = &self.navigation_jobs[cursor];
                if self.tick.0 - job.enqueued_at.0 >= u64::from(max_wait.get()) {
                    let entity = job.entity;
                    let dense = self.dense_index(entity).expect("pending entity remains live");
                    self.cancel_navigation_at(dense, NavigationCancelReason::TimedOut);
                    report.timed_out += 1;
                } else {
                    cursor += 1;
                }
            }
        }
        let before = self.navigation_budget.used();
        while self.navigation_budget.remaining() != 0 {
            let Some(mut job) = self.navigation_jobs.pop_front() else { break; };
            let Ok(dense) = self.dense_index(job.entity) else { continue; };
            let mut slice = NavigationBudget::new(self.navigation_budget.remaining().min(self.navigation_config.work_per_slice));
            // The at-most-16 key comparisons are bounded queue bookkeeping.
            // Copying/installing a route is charged, including cache hits;
            // insufficient allowance preserves the request and its turn.
            let cached = self.navigation_routes.iter().find(|entry|
                entry.origin == job.origin && entry.target == job.target && entry.smooth==job.smooth);
            let result = if let Some(entry) = cached {
                let cost = 4 * (entry.path.waypoints().len() as u32 + 1);
                if slice.try_charge(cost) {
                    report.cached += 1;
                    Some(Ok(entry.path.clone()))
                } else { None }
            } else {
                // Identical origins benefit from the exact-route cache. Starting
                // a reverse frontier for just duplicated clicks can expand far
                // more cells than A*, particularly across many distinct goals.
                // Preserve shared goal search for genuinely different origins.
                let grouped=self.navigation_jobs.iter().any(|other|other.target==job.target&&other.origin!=job.origin);
                let result = job.search.resume_shared(self.collision.as_ref(), &mut slice,
                    &mut self.navigation_goals,grouped);
                if matches!(&result,Some(Ok(_))) && job.search.used_shared_route(){report.shared+=1;}
                if let Some(Ok(path)) = &result {
                    // Cache insertion is optional. Never exceed a slice or
                    // delay a finished request merely to populate the cache.
                    let cost = 4 * (path.waypoints().len() as u32 + 1);
                    if slice.try_charge(cost) {
                        if self.navigation_routes.len() == ROUTE_CACHE_CAPACITY {
                            self.navigation_routes.pop_front();
                        }
                        self.navigation_routes.push_back(CachedRoute {
                            origin:job.origin, target:job.target, path:path.clone(),smooth:job.smooth,
                        });
                    }
                }
                result
            };
            self.navigation_budget.charge(slice.used()).expect("slice fits world allowance");
            job.work_used = job.work_used.saturating_add(u64::from(slice.used()));
            match result {
                None => {
                    if slice.used() == 0 {
                        // Preserve this job's turn when the remaining frame
                        // allowance cannot fit its next atomic operation.
                        self.navigation_jobs.push_front(job);
                        break;
                    }
                    self.navigation_jobs.push_back(job);
                }
                Some(Err(error)) => {
                    self.navigation_statuses[dense] = NavigationStatus::Failed { target: job.target, error };
                    report.failed += 1; // The suspended old movement resumes below.
                }
                Some(Ok(path)) => {
                    let origin = GridPosition { x: self.xs[dense], y: self.ys[dense] };
                    let remainder = self.movements[dense].as_ref().map_or(0, |active| active.remainder);
                    let movement = Movement::from_path(origin, path, job.speed, remainder);
                    self.clear_movement(dense);
                    self.movements[dense] = movement;
                    if self.movements[dense].is_some() {
                        self.index_movement(dense);
                        self.movement_statuses[dense] = MovementStatus::Moving;
                    } else {
                        self.movement_statuses[dense] = MovementStatus::Arrived;
                    }
                    self.navigation_statuses[dense] = NavigationStatus::Ready { target: job.target };
                    report.completed += 1;
                }
            }
        }
        report.pending = self.navigation_jobs.len();
        report.work_used = self.navigation_budget.used() - before;
        report
    }

    /// Cancels planning only; an old suspended route resumes next tick. Repeated
    /// cancellation returns false and preserves the previous cancellation reason.
    pub fn cancel_navigation(&mut self, entity: EntityId) -> Result<bool, WorldError> {
        let dense = self.dense_index(entity)?;
        if !matches!(self.navigation_statuses[dense], NavigationStatus::Pending { .. }) {
            return Ok(false);
        }
        self.combat.interrupt_chase(dense);
        Ok(self.cancel_navigation_at(dense, NavigationCancelReason::Explicit))
    }

    pub fn navigation_wait(&self, entity: EntityId) -> Result<Option<NavigationWait>, WorldError> {
        let dense = self.dense_index(entity)?;
        Ok(self.navigation_wait_at(entity, dense))
    }

    fn navigation_wait_at(&self, entity: EntityId, dense: usize) -> Option<NavigationWait> {
        if !matches!(self.navigation_statuses[dense], NavigationStatus::Pending { .. }) { return None; }
        let (queue_position, job) = self.navigation_jobs.iter().enumerate()
            .find(|(_, job)| job.entity == entity).expect("pending request is queued");
        Some(NavigationWait {
            enqueued_at: job.enqueued_at, waited_ticks: self.tick.0 - job.enqueued_at.0,
            queue_position, phase: job.search.phase(), work_used: job.work_used,
            movement_suspended: self.movements[dense].is_some(),
        })
    }

    fn cancel_navigation_at(&mut self, dense: usize, reason: NavigationCancelReason) -> bool {
        let NavigationStatus::Pending { target } = self.navigation_statuses[dense] else {
            if !matches!(self.navigation_statuses[dense], NavigationStatus::Cancelled { .. }) {
                self.navigation_statuses[dense] = NavigationStatus::Idle;
            }
            return false;
        };
        let index = self.entity_slots[dense];
        let entity = EntityId { index, generation: self.slots[index as usize].generation };
        let cursor = self.navigation_jobs.iter().position(|job| job.entity == entity).expect("pending request is queued");
        let job = self.navigation_jobs.remove(cursor).expect("located job");
        let cancellation = NavigationCancellation {
            entity, target, reason, at_tick: self.tick,
            waited_ticks: self.tick.0 - job.enqueued_at.0,
        };
        if self.navigation_cancellations.len() == MAX_NAVIGATION_CANCELLATIONS {
            self.navigation_cancellations.pop_front();
            self.dropped_navigation_cancellations = self.dropped_navigation_cancellations.saturating_add(1);
        }
        self.navigation_cancellations.push_back(cancellation);
        self.navigation_statuses[dense] = NavigationStatus::Cancelled { target, reason };
        true
    }

    /// Cancel pending navigation, stop movement and discard fractional credit.
    /// If neither exists, preserve the previous movement outcome.
    pub fn stop_moving(&mut self, entity: EntityId) -> Result<(), WorldError> {
        let dense = self.dense_index(entity)?;
        self.combat.interrupt_chase(dense);
        self.halt_movement_at(dense);
        Ok(())
    }

    pub(crate) fn halt_movement_at(&mut self, dense: usize) {
        self.controlled_motion[dense]=None;
        let cancelled = self.cancel_navigation_at(dense, NavigationCancelReason::Stopped);
        if cancelled || self.movements[dense].is_some() {
            self.clear_movement(dense);
            self.movement_statuses[dense] = MovementStatus::Stopped;
        }
    }

    pub(crate) fn navigation_motion_at(&self, dense: usize) -> (NavigationStatus, MovementStatus) {
        (self.navigation_statuses[dense], self.movement_statuses[dense])
    }

    /// Read without consuming the outcome. Later valid commands may replace it;
    /// callers needing every transition will need a separate event mechanism.
    pub fn movement_status(&self, entity: EntityId) -> Result<MovementStatus, WorldError> {
        let dense = self.dense_index(entity)?;
        Ok(self.movement_statuses[dense])
    }

    pub fn movement_target(&self, entity: EntityId) -> Result<Option<GridPosition>, WorldError> {
        let dense = self.dense_index(entity)?;
        Ok(self.movements[dense].as_ref().map(|movement| movement.target))
    }

    /// Traverse only live entities without collecting a temporary snapshot.
    /// Order is dense storage order and may change after removal.
    pub fn positions(&self) -> impl Iterator<Item = (EntityId, GridPosition)> + '_ {
        self.entity_slots.iter().enumerate().map(|(dense, &index)| {
            let entity = EntityId { index, generation: self.slots[index as usize].generation };
            (entity, GridPosition { x: self.xs[dense], y: self.ys[dense] })
        })
    }

    /// Public motion projection for replication. Navigation diagnostics and
    /// private application state are intentionally not part of this projection.
    pub fn motion_states(&self) -> impl Iterator<Item = crate::MotionState> + '_ {
        self.entity_slots.iter().enumerate().map(|(dense, &index)| crate::MotionState {
            entity: EntityId { index, generation: self.slots[index as usize].generation },
            position: GridPosition { x: self.xs[dense], y: self.ys[dense] },
            movement: self.movement_statuses[dense],
        })
    }

    fn index_movement(&mut self, dense: usize) {
        if self.moving_rows[dense].is_none() {
            // spawn caps the live count at u32::MAX. This entity is not yet
            // indexed, so len + 1 is nonzero and fits in u32.
            self.moving_rows[dense] = NonZeroU32::new(self.moving_entities.len() as u32 + 1);
            self.moving_entities.push(dense as u32);
        }
    }

    fn clear_movement(&mut self, dense: usize) {
        if let Some(row) = self.moving_rows[dense].take() {
            let cursor = (row.get() - 1) as usize;
            self.moving_entities.swap_remove(cursor);
            if let Some(&moved_dense) = self.moving_entities.get(cursor) {
                self.moving_rows[moved_dense as usize] = Some(row);
            }
        }
        self.movements[dense] = None;
    }

    fn require_walkable(&self, position: GridPosition) -> Result<(), WorldError> {
        if self.collision.as_ref().is_some_and(|map| !map.is_walkable(position)) {
            Err(WorldError::BlockedPosition)
        } else {
            Ok(())
        }
    }
    pub(crate) fn preflight_spawns(&self,positions:&[GridPosition])->Result<(),WorldError>{
        if self.entity_slots.len()as u128+positions.len()as u128>u128::from(u32::MAX)
            ||self.slots.len()as u128+positions.len().saturating_sub(self.free_slots.len())as u128>u128::from(u32::MAX){return Err(WorldError::EntityCapacityExhausted);}
        for &position in positions{self.require_walkable(position)?;}Ok(())
    }

    pub(crate) fn dense_index(&self, entity: EntityId) -> Result<usize, WorldError> {
        let slot = self.slots.get(entity.index as usize).ok_or(WorldError::InvalidEntity)?;
        if slot.generation != entity.generation {
            return Err(WorldError::InvalidEntity);
        }
        slot.dense
            .map(|row| (row.get() - 1) as usize)
            .ok_or(WorldError::InvalidEntity)
    }
}
