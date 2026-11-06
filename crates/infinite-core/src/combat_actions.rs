//! Bounded query-to-combat integration for trusted application commands.
use std::num::NonZeroU32;

use crate::{
    AttackError, AttackProfile, ChaseConfig, CombatArea, CombatEvent, CombatEventKind,
    CombatNearestQuery, CombatNearestResult, CombatQuery, CombatQueryBudget,
    CombatQueryContext, CombatQueryError, CombatQueryHit, CombatQueryWork, EntityId,
    GridPosition, SpatialFrame, SyncError, Tick, World,
    MAX_ACTIVE_COMBATANTS, MAX_COMBAT_QUERIES,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatActionError {
    Spatial(SyncError),
    Query(CombatQueryError),
    Attack(AttackError),
    TooManyActions,
    EventLimitExceeded,
}

/// Explicit acquisition, not a background AI scan. No match preserves the old
/// intent. The application decides when to reacquire instead of keeping a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcquireAttack {
    pub attacker: EntityId,
    pub search_radius: u32,
    pub require_sight: bool,
    pub profile: AttackProfile,
    pub chase: Option<ChaseConfig>,
}

/// One already-authorized impact, not a client cast command. Damage is a fixed
/// final integer amount per target. This does not spend mana, start a windup,
/// reserve cooldown, or deduplicate repeated casts; those are application duties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AreaDamage {
    pub attacker: EntityId,
    pub area: CombatArea,
    pub damage: NonZeroU32,
    /// Sight runs from the attacker, independently of the area's center.
    pub require_sight: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AreaDamageReport {
    pub tick: Tick,
    pub hits: usize,
    pub depleted: usize,
    /// New events occupy events[offset..offset+hits].
    pub offset: usize,
    pub work: CombatQueryWork,
}

/// Reuse one instance across ticks: incremental spatial index and hit storage.
/// There is one capture per phase, not per request. This separate pre-settlement
/// capture cannot replace the outgoing CombatFrame's post-settlement capture.
pub struct CombatActions {
    frame: SpatialFrame,
    hits: Vec<CombatQueryHit>,
}

impl CombatActions {
    pub fn new(cell_size: NonZeroU32) -> Self {
        Self { frame: SpatialFrame::new(cell_size), hits: Vec::new() }
    }

    /// Append to this tick's caller-owned ordered event buffer. Never clear it.
    /// Reserve room for other phases by lowering event_limit; it includes the
    /// existing prefix and must not exceed CombatFrame's 4096-event bound.
    /// Failures preserve world/events. Capture is allowed again in the same tick.
    pub fn begin<'a>(&'a mut self, world: &'a mut World,
        budget: &'a mut CombatQueryBudget, events: &'a mut Vec<CombatEvent>,
        event_limit: usize) -> Result<CombatActionPhase<'a>, CombatActionError>
    {
        if event_limit > MAX_ACTIVE_COMBATANTS || events.len() > event_limit {
            return Err(CombatActionError::EventLimitExceeded);
        }
        self.frame.capture(world).map_err(CombatActionError::Spatial)?;
        self.hits.clear();
        Ok(CombatActionPhase { storage: self, world, budget, events, event_limit, attempted: 0 })
    }
}

/// Exclusively borrows the world: no movement, despawn, tick advance or map
/// replacement can invalidate the captured geometry while actions run. Health
/// and eligibility are read live for each action, so later impacts skip kills.
pub struct CombatActionPhase<'a> {
    storage: &'a mut CombatActions,
    world: &'a mut World,
    budget: &'a mut CombatQueryBudget,
    events: &'a mut Vec<CombatEvent>,
    event_limit: usize,
    attempted: usize,
}

impl CombatActionPhase<'_> {
    /// Linear nearest selection with generation-aware tie breaking, followed by
    /// the existing attack/chase admission. Query failure never starts a partial
    /// best target. Sight applies to acquisition only; the normal attack loop's
    /// hit rules remain unchanged. accept must be a lightweight pure predicate.
    pub fn acquire_attack(&mut self, request: AcquireAttack,
        mut accept: impl FnMut(&World, EntityId) -> bool)
        -> Result<CombatNearestResult, CombatActionError>
    {
        self.admit()?;
        let origin = self.attacker_position(request.attacker)?;
        let world = &*self.world;
        let context = CombatQueryContext::new(&self.storage.frame, world.collision_map());
        let result = context.nearest(CombatNearestQuery {
            query: CombatQuery {
                area: CombatArea::Circle { center: origin, radius: request.search_radius },
                exclude: Some(request.attacker),
                sight_origin: request.require_sight.then_some(origin),
            },
            origin,
        }, self.budget, |hit| {
            world.health(hit.entity).is_ok_and(|health| !health.is_depleted())
                && accept(world, hit.entity)
        }).map_err(CombatActionError::Query)?;
        if let Some(hit) = result.target {
            match request.chase {
                Some(chase) => self.world.start_chase_attack(request.attacker, hit.entity, request.profile, chase),
                None => self.world.start_attack(request.attacker, hit.entity, request.profile),
            }.map_err(CombatActionError::Attack)?;
        }
        Ok(result)
    }

    /// Query completely and preflight event space before changing any health.
    /// Failed work still consumes query budget and an action attempt. Earlier
    /// successful actions remain committed; never retry the whole phase blindly.
    /// Each target appears once, sorted by (index,generation); impacts retain
    /// call order. App filters supply faction, materialization and PvP rules.
    pub fn damage_area(&mut self, request: AreaDamage,
        mut accept: impl FnMut(&World, EntityId) -> bool)
        -> Result<AreaDamageReport, CombatActionError>
    {
        self.admit()?;
        let origin = self.attacker_position(request.attacker)?;
        let world = &*self.world;
        let context = CombatQueryContext::new(&self.storage.frame, world.collision_map());
        let report = context.collect(CombatQuery {
            area: request.area,
            exclude: Some(request.attacker),
            sight_origin: request.require_sight.then_some(origin),
        }, self.budget, &mut self.storage.hits, |hit| {
            world.health(hit.entity).is_ok_and(|health| !health.is_depleted())
                && accept(world, hit.entity)
        }).map_err(CombatActionError::Query)?;
        let offset = self.events.len();
        if report.count > self.event_limit - offset {
            return Err(CombatActionError::EventLimitExceeded);
        }
        // Allocate before mutation. Subsequent commits have no callbacks or
        // fallible admission: the phase owns world membership and live health.
        self.events.reserve(report.count);
        let next_ready = self.world.attack_state(request.attacker)
            .map_err(CombatActionError::Attack)?.next_ready;
        let mut depleted = 0;
        for hit in &self.storage.hits {
            let row = self.world.dense_index(hit.entity).expect("phase retains query entities");
            let health = self.world.damage_at(row, request.damage.get())
                .expect("query retained configured live health").health;
            depleted += usize::from(health.depleted());
            self.events.push(CombatEvent { attacker: request.attacker, target: hit.entity,
                kind: CombatEventKind::Hit { health, next_ready } });
        }
        Ok(AreaDamageReport { tick: report.tick, hits: report.count, depleted,
            offset, work: report.work })
    }

    fn admit(&mut self) -> Result<(), CombatActionError> {
        if self.attempted == MAX_COMBAT_QUERIES {
            return Err(CombatActionError::TooManyActions);
        }
        self.attempted += 1;
        Ok(())
    }

    fn attacker_position(&self, attacker: EntityId) -> Result<GridPosition, CombatActionError> {
        let position = self.world.position(attacker)
            .map_err(|_| CombatActionError::Attack(AttackError::InvalidAttacker))?;
        let health = self.world.health(attacker)
            .map_err(|e| CombatActionError::Attack(AttackError::AttackerHealth(e)))?;
        if health.is_depleted() {
            return Err(CombatActionError::Attack(AttackError::AttackerDepleted));
        }
        Ok(position)
    }
}
