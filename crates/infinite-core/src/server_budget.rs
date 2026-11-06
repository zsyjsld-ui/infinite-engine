//! Deterministic admission for deferrable work across maps. Units are configured
//! operation weights, not a promise about CPU time. Authority is always executed
//! by the host; accounting reports overload instead of postponing due damage.
use crate::Tick;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_BUDGET: AtomicU64 = AtomicU64::new(1);

pub const SERVER_WORK_LANES: usize = 5;
pub const MAX_BUDGET_MAPS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum ServerWorkLane { Navigation, Perception, Spawn, Network, Persistence }
impl ServerWorkLane {
    const ALL: [Self; SERVER_WORK_LANES] = [Self::Navigation, Self::Perception,
        Self::Spawn, Self::Network, Self::Persistence];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerLaneBudget {
    pub unit_cost: u32,
    pub units_per_tick: u32,
    pub quantum: u32,
    /// Exceeding this age reports overload; it never silently drops work.
    pub max_wait_ticks: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerBudgetConfig {
    pub maps: usize,
    pub weighted_units_per_tick: u64,
    pub lanes: [ServerLaneBudget; SERVER_WORK_LANES],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerBudgetError {
    InvalidConfig, InvalidMap, InvalidUnits, AlreadyPending, TicketExhausted, InstanceExhausted,
    TickNotStarted, TimeNotAdvanced, AccountingOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerWorkTicket { owner: u64, slot: usize, serial: u64 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerWorkGrant {
    pub ticket: ServerWorkTicket,
    pub map: usize,
    pub lane: ServerWorkLane,
    pub units: u32,
    pub weighted_units: u64,
    pub waited_ticks: u64,
    pub remaining: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerBudgetReport {
    pub tick: Tick,
    pub authority_units: u64,
    pub deferred_units: u64,
    pub lane_units: [u32; SERVER_WORK_LANES],
    pub pending: usize,
    pub overdue: usize,
    pub oldest_wait_ticks: u64,
    pub over_budget: bool,
}

#[derive(Clone, Copy)]
struct Request { serial: u64, at: Tick, remaining: u32 }

pub struct ServerWorkBudget {
    owner: u64,
    config: ServerBudgetConfig,
    requests: Vec<Option<Request>>,
    cursor: usize,
    carry_cursor: Option<usize>,
    next_serial: Option<u64>,
    tick: Option<Tick>,
    authority: u64,
    deferred: u64,
    lane_used: [u32; SERVER_WORK_LANES],
    pending: usize,
}

impl ServerWorkBudget {
    pub fn new(config: ServerBudgetConfig) -> Result<Self, ServerBudgetError> {
        if config.maps == 0 || config.maps > MAX_BUDGET_MAPS
            || config.weighted_units_per_tick == 0
            || config.lanes.iter().any(|lane| lane.unit_cost == 0
                || u64::from(lane.unit_cost) > config.weighted_units_per_tick
                || lane.units_per_tick == 0 || lane.quantum == 0
                || lane.quantum > lane.units_per_tick) {
            return Err(ServerBudgetError::InvalidConfig);
        }
        let owner = NEXT_BUDGET.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |id| id.checked_add(1)).map_err(|_| ServerBudgetError::InstanceExhausted)?;
        Ok(Self { owner, requests: vec![None; config.maps * SERVER_WORK_LANES], config,
            cursor: 0, carry_cursor: None, next_serial: Some(1), tick: None, authority: 0, deferred: 0,
            lane_used: [0; SERVER_WORK_LANES], pending: 0 })
    }

    /// One reset per strictly advancing logical tick. Pending work and the fair
    /// cursor survive resets; a busy map cannot regain first place every tick.
    pub fn begin_tick(&mut self, tick: Tick) -> Result<(), ServerBudgetError> {
        if self.tick.is_some_and(|previous| tick <= previous) {
            return Err(ServerBudgetError::TimeNotAdvanced);
        }
        self.tick = Some(tick);
        // Cheap work may use the fragmented remainder of the previous tick,
        // but must not steal next tick's first turn from an unaffordable lane.
        if let Some(cursor) = self.carry_cursor.take() { self.cursor = cursor; }
        self.authority = 0;
        self.deferred = 0;
        self.lane_used.fill(0);
        Ok(())
    }

    /// Account mandatory authority after running it. Even an exhausted optional
    /// budget is never a reason to skip due combat/effects or connection ACKs.
    pub fn record_authority(&mut self, units: u64) -> Result<(), ServerBudgetError> {
        self.tick.ok_or(ServerBudgetError::TickNotStarted)?;
        self.authority = self.authority.checked_add(units)
            .ok_or(ServerBudgetError::AccountingOverflow)?;
        Ok(())
    }

    /// At most one outstanding request per map/lane. The host retains the actual
    /// resumable job; the scheduler owns only its work reservation, not payloads.
    pub fn request(&mut self, map: usize, lane: ServerWorkLane, units: u32)
        -> Result<ServerWorkTicket, ServerBudgetError> {
        let at = self.tick.ok_or(ServerBudgetError::TickNotStarted)?;
        if map >= self.config.maps { return Err(ServerBudgetError::InvalidMap); }
        if units == 0 { return Err(ServerBudgetError::InvalidUnits); }
        let slot = map * SERVER_WORK_LANES + lane as usize;
        if self.requests[slot].is_some() { return Err(ServerBudgetError::AlreadyPending); }
        let serial = self.next_serial.ok_or(ServerBudgetError::TicketExhausted)?;
        self.next_serial = serial.checked_add(1);
        self.requests[slot] = Some(Request { serial, at, remaining: units });
        self.pending += 1;
        Ok(ServerWorkTicket { owner: self.owner, slot, serial })
    }

    /// A stale ticket cannot cancel replacement work in the same map/lane.
    pub fn cancel(&mut self, ticket: ServerWorkTicket) -> bool {
        if ticket.owner != self.owner { return false; }
        let Some(slot) = self.requests.get_mut(ticket.slot) else { return false; };
        if slot.is_some_and(|r| r.serial == ticket.serial) {
            *slot = None;
            self.pending -= 1;
            true
        } else { false }
    }

    /// Grants are conservatively spent, including failed or unused work. Callers
    /// must use them only for operations which may legally be sliced/deferred.
    /// A grant is not an execution, a packet delivery or a durable commit.
    pub fn next_grant(&mut self) -> Option<ServerWorkGrant> {
        let tick = self.tick?;
        if self.pending == 0 { return None; }
        let available = self.config.weighted_units_per_tick
            .saturating_sub(self.authority).saturating_sub(self.deferred);
        if available == 0 { return None; }
        for offset in 0..self.requests.len() {
            let slot = (self.cursor + offset) % self.requests.len();
            let Some(request) = self.requests[slot] else { continue; };
            let lane_index = slot % SERVER_WORK_LANES;
            let lane = self.config.lanes[lane_index];
            let units = u64::from(request.remaining.min(lane.quantum)
                .min(lane.units_per_tick - self.lane_used[lane_index]))
                .min(available / u64::from(lane.unit_cost)) as u32;
            if units == 0 {
                if available < u64::from(lane.unit_cost) && self.lane_used[lane_index] < lane.units_per_tick {
                    self.carry_cursor.get_or_insert(slot);
                }
                continue;
            }
            let weighted_units = u64::from(units) * u64::from(lane.unit_cost);
            let remaining = request.remaining - units;
            self.requests[slot] = if remaining == 0 { self.pending -= 1; None }
                else { Some(Request { remaining, ..request }) };
            self.deferred += weighted_units;
            self.lane_used[lane_index] += units;
            self.cursor = (slot + 1) % self.requests.len();
            return Some(ServerWorkGrant { ticket: ServerWorkTicket { owner: self.owner, slot, serial: request.serial },
                map: slot / SERVER_WORK_LANES, lane: ServerWorkLane::ALL[lane_index],
                units, weighted_units, waited_ticks: tick.0 - request.at.0, remaining });
        }
        None
    }

    pub fn report(&self) -> Result<ServerBudgetReport, ServerBudgetError> {
        let tick = self.tick.ok_or(ServerBudgetError::TickNotStarted)?;
        let mut report = ServerBudgetReport { tick, authority_units: self.authority,
            deferred_units: self.deferred, lane_units: self.lane_used, pending: self.pending,
            over_budget: self.authority > self.config.weighted_units_per_tick
                || self.deferred > self.config.weighted_units_per_tick.saturating_sub(self.authority),
            ..ServerBudgetReport::default() };
        for (slot, request) in self.requests.iter().enumerate() {
            if let Some(request) = request {
                let age = tick.0 - request.at.0;
                report.oldest_wait_ticks = report.oldest_wait_ticks.max(age);
                report.overdue += usize::from(age > u64::from(self.config.lanes[slot % SERVER_WORK_LANES].max_wait_ticks));
            }
        }
        Ok(report)
    }

    pub fn capacity_bytes(&self) -> usize {
        self.requests.capacity() * std::mem::size_of::<Option<Request>>()
    }
}
