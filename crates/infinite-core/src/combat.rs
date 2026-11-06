use std::num::{NonZeroU16, NonZeroU32};
use crate::{EntityId, GridPosition, HealthChange, HealthError, HealthOutcome, Tick, World};
mod chase;
pub use chase::{ChaseConfig, ChaseState, ChaseStatus, ChaseFailure, MAX_CHASE_REQUESTS_PER_TICK};
use chase::ChaseRuntime;

/// Bound both the active list and one phase's work/output. Not an online-user cap.
pub const MAX_ACTIVE_COMBATANTS: usize = 4096;

/// Trusted fixed mitigated damage before shields, not a skill formula.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttackProfile {
    /// Inclusive centre-to-centre radius, in 1/16 world-pixel units.
    pub range: u32,
    pub damage: NonZeroU32,
    pub cooldown_ticks: NonZeroU16,
    /// Delay before damage. Zero preserves the immediate-hit behavior.
    pub windup_ticks: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttackIntent { pub target: EntityId, pub profile: AttackProfile }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttackWindup {
    pub target: EntityId,
    pub profile: AttackProfile,
    pub started_at: Tick,
    pub hit_at: Tick,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttackState {
    pub intent: Option<AttackIntent>, pub next_ready: Tick,
    pub chase: Option<ChaseState>, pub windup: Option<AttackWindup>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttackError {
    InvalidAttacker, InvalidTarget, SelfTarget, AttackerDepleted, TargetDepleted,
    AttackerHealth(HealthError), TargetHealth(HealthError), CapacityExhausted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatEndReason { AttackerDepleted, TargetDepleted, TargetGone, HealthUnavailable, CooldownOverflow }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatEventKind {
    WindupStarted { windup: AttackWindup, next_ready: Tick },
    /// The phase observed a live target outside the snapshotted attack range.
    /// Explicit command cancellation is reflected directly by AttackState.
    WindupCancelled { windup: AttackWindup },
    Hit { health: HealthOutcome, next_ready: Tick },
    Ended(CombatEndReason),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatEvent { pub attacker: EntityId, pub target: EntityId, pub kind: CombatEventKind }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatPhaseError { AlreadyResolved, EventsBufferTooSmall, ChangesBufferTooSmall }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatReport {
    pub tick: Tick,
    pub visited: usize,
    pub cooling_down: usize,
    pub out_of_range: usize,
    pub hits: usize,
    pub ended: usize,
    pub events_written: usize,
    pub changed_entities: usize,
    pub remaining_active: usize,
    pub chase_requests: usize,
    pub chase_deferred: usize,
    pub windups_started: usize,
    pub winding_up: usize,
    pub windups_cancelled: usize,
    pub waiting_for_stop: usize,
}

#[derive(Clone, Copy)]
struct ActiveAttack {
    entity: EntityId, row: usize, intent: AttackIntent,
    chase: Option<ChaseRuntime>, windup: Option<AttackWindup>,
}
#[derive(Default)]
pub(crate) struct CombatColumns {
    rows: Vec<Option<NonZeroU16>>,
    ready: Vec<Tick>,
    active: Vec<ActiveAttack>,
    last_resolved: Option<Tick>,
    chase_cursor: usize,
    chase_count: usize,
}
impl CombatColumns {
    pub(crate) fn interrupt_chase(&mut self, row: usize) {
        if let Some(index) = self.rows[row] {
            let active = &mut self.active[usize::from(index.get()-1)];
            // A successful external movement command cancels a pending strike
            // even for a stationary attacker. Its reserved cooldown survives.
            active.windup = None;
            if let Some(chase) = &mut active.chase {
                chase.owns_motion = false;
                chase.state.status = ChaseStatus::Interrupted;
            }
        }
    }
    pub(crate) fn spawn(&mut self) { self.rows.push(None); self.ready.push(Tick(0)); }
    pub(crate) fn reset_life(&mut self,row:usize){debug_assert!(self.rows[row].is_none());self.ready[row]=Tick(0);}
    pub(crate) fn despawn(&mut self, row: usize) {
        self.remove(row);
        self.rows.swap_remove(row); self.ready.swap_remove(row);
        // The world's last dense row moved into this row. Its handle is stable.
        if let Some(Some(index)) = self.rows.get(row) { self.active[usize::from(index.get()-1)].row = row; }
    }
    fn remove(&mut self, row: usize) -> bool {
        let Some(index) = self.rows[row].take() else { return false; };
        let index = usize::from(index.get()-1);
        let removed = self.active.swap_remove(index);
        if removed.chase.is_some() { self.chase_count -= 1; }
        if let Some(moved) = self.active.get(index) {
            self.rows[moved.row] = NonZeroU16::new((index+1) as u16);
        }
        true
    }
}

/// Exact integer circle check, including i32 extremes and u32::MAX radius.
/// Axis rejection and subtracting one square avoid overflowing the square sum.
pub fn within_attack_range(from: GridPosition, to: GridPosition, range: u32) -> bool {
    let dx = (i64::from(to.x)-i64::from(from.x)).unsigned_abs();
    let dy = (i64::from(to.y)-i64::from(from.y)).unsigned_abs();
    let radius = u64::from(range);
    dx <= radius && dy <= radius && dx*dx <= radius*radius-dy*dy
}

impl World {
    /// Start/replace an intent without resetting the attacker's existing cooldown.
    /// Validation failure preserves any old intent; no chasing is started.
    pub fn start_attack(&mut self, attacker: EntityId, target: EntityId, profile: AttackProfile) -> Result<(), AttackError> {
        self.start_attack_impl(attacker,target,profile,None)
    }
    fn start_attack_impl(&mut self, attacker: EntityId, target: EntityId, profile: AttackProfile, chase: Option<ChaseConfig>) -> Result<(), AttackError> {
        let row = self.dense_index(attacker).map_err(|_| AttackError::InvalidAttacker)?;
        let target_row = self.dense_index(target).map_err(|_| AttackError::InvalidTarget)?;
        if attacker == target { return Err(AttackError::SelfTarget); }
        if self.health.read(row).map_err(AttackError::AttackerHealth)?.is_depleted() { return Err(AttackError::AttackerDepleted); }
        if self.health.read(target_row).map_err(AttackError::TargetHealth)?.is_depleted() { return Err(AttackError::TargetDepleted); }
        let intent = AttackIntent { target, profile };
        if let Some(index) = self.combat.rows[row] {
            let index = usize::from(index.get()-1);
            let old = self.combat.active[index];
            if old.chase.is_none() && chase.is_some() { self.combat.chase_count += 1; }
            if old.chase.is_some() && chase.is_none() { self.combat.chase_count -= 1; }
            let same = old.intent.target == target && old.chase.map(|c|c.state.config) == chase;
            if !same || old.intent.profile != profile {
                self.combat.active[index].windup = None;
            }
            if !same || old.chase.is_some_and(|c|c.state.status == ChaseStatus::Interrupted) {
                self.stop_owned_chase(row);
                if chase.is_some() { self.halt_movement_at(row); }
                self.combat.active[index].chase = chase.map(|c|ChaseRuntime::new(c,self.tick()));
            }
            self.combat.active[index].intent = intent;
        } else {
            if self.combat.active.len() == MAX_ACTIVE_COMBATANTS { return Err(AttackError::CapacityExhausted); }
            if chase.is_some() { self.halt_movement_at(row); }
            let chase = chase.map(|c|ChaseRuntime::new(c,self.tick()));
            if chase.is_some() { self.combat.chase_count += 1; }
            self.combat.active.push(ActiveAttack { entity:attacker, row, intent, chase, windup:None });
            self.combat.rows[row] = NonZeroU16::new(self.combat.active.len() as u16);
        }
        Ok(())
    }
    /// Cooldown survives stop/restart; stopping an inactive entity is a no-op.
    pub fn stop_attack(&mut self, attacker: EntityId) -> Result<bool, AttackError> {
        let row = self.dense_index(attacker).map_err(|_| AttackError::InvalidAttacker)?;
        self.stop_owned_chase(row);
        Ok(self.combat.remove(row))
    }
    pub fn active_combatants(&self) -> usize { self.combat.active.len() }
    pub fn attack_state(&self, attacker: EntityId) -> Result<AttackState, AttackError> {
        let row = self.dense_index(attacker).map_err(|_| AttackError::InvalidAttacker)?;
        let intent = self.combat.rows[row].map(|index| self.combat.active[usize::from(index.get()-1)].intent);
        let chase = self.combat.rows[row].and_then(|index| self.combat.active[usize::from(index.get()-1)].chase.map(|c|c.state));
        let windup = self.combat.rows[row].and_then(|index| self.combat.active[usize::from(index.get()-1)].windup);
        Ok(AttackState { intent, next_ready:self.combat.ready[row], chase, windup })
    }

    /// Explicit phase: call after movement and before capturing outgoing state.
    /// At most once per world tick, including an empty phase. Preflight errors
    /// leave state/output untouched and do not consume that tick's phase.
    /// Each output buffer must fit the active count at entry, at most 4096.
    /// No callbacks, catch-up bursts, event queue or all-world scan are involved.
    pub fn resolve_combat(&mut self, events: &mut [CombatEvent], changes: &mut [HealthChange]) -> Result<CombatReport, CombatPhaseError> {
        let tick = self.tick();
        if self.combat.last_resolved == Some(tick) { return Err(CombatPhaseError::AlreadyResolved); }
        let count = self.combat.active.len();
        if events.len() < count { return Err(CombatPhaseError::EventsBufferTooSmall); }
        if changes.len() < count { return Err(CombatPhaseError::ChangesBufferTooSmall); }
        self.combat.last_resolved = Some(tick);
        let mut report = CombatReport { tick, visited:0, cooling_down:0, out_of_range:0, hits:0,
            ended:0, events_written:0, changed_entities:0, remaining_active:0, chase_requests:0, chase_deferred:0,
            windups_started:0, winding_up:0, windups_cancelled:0, waiting_for_stop:0 };
        let mut changed = 0;
        let mut cursor = 0;
        while cursor < self.combat.active.len() {
            let active = self.combat.active[cursor];
            report.visited += 1;
            let target = active.windup.map_or(active.intent.target,|windup|windup.target);
            let profile = active.windup.map_or(active.intent.profile,|windup|windup.profile);
            let target_row = self.dense_index(target).ok();
            // Validate life/handle even during cooldown so stale intents retire.
            let ended = match self.health.read(active.row) {
                Err(_) => Some(CombatEndReason::HealthUnavailable),
                Ok(health) if health.is_depleted() => Some(CombatEndReason::AttackerDepleted),
                Ok(_) => match target_row {
                    None => Some(CombatEndReason::TargetGone),
                    Some(row) => match self.health.read(row) {
                        Err(_) => Some(CombatEndReason::HealthUnavailable),
                        Ok(health) if health.is_depleted() => Some(CombatEndReason::TargetDepleted),
                        Ok(_) => None,
                    },
                },
            };
            if let Some(reason) = ended {
                events[report.events_written] = CombatEvent { attacker:active.entity, target, kind:CombatEventKind::Ended(reason) };
                report.events_written += 1; report.ended += 1;
                self.stop_owned_chase(active.row);
                self.combat.remove(active.row);
                continue; // The unvisited tail filled this cursor.
            }
            let chasing = active.chase.is_some_and(|c|c.state.status != ChaseStatus::Interrupted);
            if active.windup.is_none() && !chasing && tick < self.combat.ready[active.row] { report.cooling_down += 1; cursor += 1; continue; }
            let target_row = target_row.expect("validated live target");
            let from = self.position_at(active.row);
            let to = self.position_at(target_row);
            if !within_attack_range(from,to,profile.range) {
                if let Some(windup) = active.windup {
                    self.combat.active[cursor].windup = None;
                    events[report.events_written] = CombatEvent { attacker:active.entity, target,
                        kind:CombatEventKind::WindupCancelled { windup } };
                    report.events_written += 1; report.windups_cancelled += 1;
                }
                report.out_of_range += 1; cursor += 1; continue;
            }
            self.chase_in_range(active.row);
            if let Some(windup) = active.windup {
                if tick < windup.hit_at { report.winding_up += 1; cursor += 1; continue; }
            } else {
                if tick < self.combat.ready[active.row] { report.cooling_down += 1; cursor += 1; continue; }
                if profile.windup_ticks != 0 {
                    let (navigation,movement) = self.navigation_motion_at(active.row);
                    // Do not steal movement back after the player interrupted a
                    // chase. Only the chase-owned route was stopped above.
                    if matches!(navigation,crate::NavigationStatus::Pending { .. }) || movement == crate::MovementStatus::Moving {
                        report.waiting_for_stop += 1; cursor += 1; continue;
                    }
                    let deadlines = tick.0.checked_add(u64::from(profile.windup_ticks)).and_then(|hit_at|
                        hit_at.checked_add(u64::from(profile.cooldown_ticks.get())).map(|ready|(Tick(hit_at),Tick(ready))));
                    let Some((hit_at,next_ready)) = deadlines else {
                        events[report.events_written] = CombatEvent { attacker:active.entity, target,
                            kind:CombatEventKind::Ended(CombatEndReason::CooldownOverflow) };
                        report.events_written += 1; report.ended += 1;
                        self.stop_owned_chase(active.row); self.combat.remove(active.row); continue;
                    };
                    let windup = AttackWindup { target,profile,started_at:tick,hit_at };
                    self.combat.active[cursor].windup = Some(windup);
                    self.combat.ready[active.row] = next_ready;
                    events[report.events_written] = CombatEvent { attacker:active.entity, target,
                        kind:CombatEventKind::WindupStarted { windup,next_ready } };
                    report.events_written += 1; report.windups_started += 1;
                    cursor += 1; continue;
                }
            }
            // A late phase still delivers at most one hit and starts cooldown
            // from the actual hit tick, never catches up a burst of missed hits.
            let Some(next_ready) = tick.0.checked_add(u64::from(profile.cooldown_ticks.get())).map(Tick) else {
                events[report.events_written] = CombatEvent { attacker:active.entity, target, kind:CombatEventKind::Ended(CombatEndReason::CooldownOverflow) };
                report.events_written += 1; report.ended += 1;
                self.stop_owned_chase(active.row);
                self.combat.remove(active.row); continue;
            };
            self.combat.active[cursor].windup = None;
            let outcome = self.damage_at(target_row,profile.damage.get())
                .expect("validated configured target").health;
            self.health.record_change(target_row,target,outcome,changes,&mut changed);
            self.combat.ready[active.row] = next_ready;
            events[report.events_written] = CombatEvent { attacker:active.entity, target,
                kind:CombatEventKind::Hit { health:outcome, next_ready } };
            report.events_written += 1; report.hits += 1;
            if outcome.depleted() {
                // This hit already reports the target's death; no second event.
                self.combat.remove(active.row); report.ended += 1;
            } else { cursor += 1; }
        }
        self.service_chases(&mut report);
        report.changed_entities = self.finish_health_changes(changes,changed);
        report.remaining_active = self.combat.active.len();
        Ok(report)
    }
}
