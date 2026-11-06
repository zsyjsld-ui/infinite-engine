//! Read-only combat geometry over an existing spatial capture.
use crate::collision::SegmentScan;
use crate::sync::key;
use crate::{CollisionMap, EntityId, GridPosition, SpatialFrame, SyncError, Tick,
    ViewRect, within_attack_range};

pub const MAX_COMBAT_QUERIES: usize = 256;
pub const MAX_COMBAT_QUERY_HITS: usize = 4096;
pub const MAX_COMBAT_BATCH_HITS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatQueryError {
    Spatial(SyncError),
    MapRequired,
    WorkLimitExceeded,
    HitLimitExceeded,
    TooManyQueries,
    ResultsTooSmall,
    InvalidSegment,
}

/// Counts operations, not elapsed time. A sight step is scan initialization or
/// one advance of the movement collision scanner, not necessarily one map cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatQueryWork {
    pub cells: u32,
    pub candidates: u32,
    pub sight_steps: u32,
}
impl CombatQueryWork {
    fn since(self, old: Self) -> Self {
        Self { cells: self.cells-old.cells, candidates: self.candidates-old.candidates,
            sight_steps: self.sight_steps-old.sight_steps }
    }
}

/// Share one allowance across all combat queries in the application's tick.
/// Failed requests consume work too. Reset by constructing a new allowance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatQueryBudget {
    limit: u32,
    used: u32,
    work: CombatQueryWork,
}
impl CombatQueryBudget {
    pub const fn new(limit: u32) -> Self {
        Self { limit, used: 0, work: CombatQueryWork { cells: 0, candidates: 0, sight_steps: 0 } }
    }
    pub const fn used(&self) -> u32 { self.used }
    pub const fn remaining(&self) -> u32 { self.limit-self.used }
    pub const fn work(&self) -> CombatQueryWork { self.work }
    fn charge(&mut self) -> Result<(), CombatQueryError> {
        if self.used == self.limit { return Err(CombatQueryError::WorkLimitExceeded); }
        self.used += 1;
        Ok(())
    }
    pub(crate) fn cell(&mut self) -> Result<(), CombatQueryError> {
        self.charge()?; self.work.cells += 1; Ok(())
    }
    pub(crate) fn candidate(&mut self) -> Result<(), CombatQueryError> {
        self.charge()?; self.work.candidates += 1; Ok(())
    }
    fn sight_step(&mut self) -> Result<(), CombatQueryError> {
        self.charge()?; self.work.sight_steps += 1; Ok(())
    }
}

/// Inclusive point geometry, in the same integer units as movement. A circle's
/// bounding box clips to i32 coordinates; the exact radius is never clipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatArea {
    Circle { center: GridPosition, radius: u32 },
    Rect(ViewRect),
}
impl CombatArea {
    fn bounds(self) -> ViewRect {
        match self {
            Self::Rect(rect) => rect,
            Self::Circle { center, radius } => {
                let radius = i64::from(radius);
                let clamp = |n: i64| n.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                ViewRect {
                    min: GridPosition { x: clamp(i64::from(center.x)-radius), y: clamp(i64::from(center.y)-radius) },
                    max: GridPosition { x: clamp(i64::from(center.x)+radius), y: clamp(i64::from(center.y)+radius) },
                }
            }
        }
    }
    fn contains(self, point: GridPosition) -> bool {
        match self {
            Self::Circle { center, radius } => within_attack_range(center, point, radius),
            Self::Rect(rect) => rect.contains(point),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatQuery {
    pub area: CombatArea,
    pub exclude: Option<EntityId>,
    /// None disables wall filtering. Some requires the supplied collision map.
    /// The caster need not coincide with the center of a ground-targeted area.
    pub sight_origin: Option<GridPosition>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatNearestQuery {
    pub query: CombatQuery,
    pub origin: GridPosition,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatQueryHit {
    pub entity: EntityId,
    pub position: GridPosition,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatQueryReport {
    pub tick: Tick,
    /// Successful results occupy output[offset..offset+count].
    pub offset: usize,
    pub count: usize,
    pub work: CombatQueryWork,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatNearestResult {
    pub tick: Tick,
    pub target: Option<CombatQueryHit>,
    pub work: CombatQueryWork,
}

/// Borrow the same capture used for sync; no second index or world copy.
/// The caller supplies the immutable collision map for that capture's world.
/// Positions are from the capture, not live World reads. Predicates must use
/// compatible state and have no gameplay side effects: visitation order and
/// whether a farther nearest-query candidate reaches the predicate are unspecified.
pub struct CombatQueryContext<'a> {
    frame: &'a SpatialFrame,
    map: Option<&'a CollisionMap>,
}
impl<'a> CombatQueryContext<'a> {
    pub fn new(frame: &'a SpatialFrame, map: Option<&'a CollisionMap>) -> Self { Self { frame, map } }

    /// Exact capsule sweep, ordered by along-path projection, distance from
    /// path, then entity handle. No partial result escapes a budget failure.
    pub fn collect_segment(&self,query:crate::CombatSegmentQuery,budget:&mut CombatQueryBudget,
        output:&mut Vec<crate::CombatSegmentHit>,accept:impl FnMut(CombatQueryHit)->bool)
        ->Result<CombatQueryReport,CombatQueryError>
    {
        let report=self.collect_segment_unsorted(query,budget,output,accept)?;
        output.sort_unstable_by_key(|h|(h.along,h.distance_scaled,key(h.hit.entity)));
        Ok(report)
    }

    // Projectile consumers need only a small ordered prefix, after policy
    // rejection. Keep the full query/budget checks but defer ordering to them.
    pub(crate) fn collect_segment_unsorted(&self,query:crate::CombatSegmentQuery,budget:&mut CombatQueryBudget,
        output:&mut Vec<crate::CombatSegmentHit>,mut accept:impl FnMut(CombatQueryHit)->bool)
        ->Result<CombatQueryReport,CombatQueryError>
    {
        output.clear();let geometry=crate::segment::SegmentGeometry::new(query.sweep)?;
        let filter=CombatQuery{area:CombatArea::Rect(geometry.bounds()),exclude:query.exclude,sight_origin:query.sight_origin};let tick=self.validate(filter)?;let before=budget.work();
        let result=self.frame.visit_combat_candidates(geometry.bounds(),budget,|motion,budget|{
            if query.exclude==Some(motion.entity){return Ok(());}let Some((along,distance_scaled))=geometry.rank(motion.position)else{return Ok(());};let hit=CombatQueryHit{entity:motion.entity,position:motion.position};
            if accept(hit)&&self.can_see(filter,hit,budget)?{if output.len()==MAX_COMBAT_QUERY_HITS{return Err(CombatQueryError::HitLimitExceeded);}output.push(crate::CombatSegmentHit{hit,along,distance_scaled});}Ok(())
        });if let Err(error)=result{output.clear();return Err(error);}Ok(CombatQueryReport{tick,offset:0,count:output.len(),work:budget.work().since(before)})
    }

    /// Geometry-only point sight. Uses exactly the static movement obstruction
    /// rules (including square/staggered edges and corners), with no allocation.
    /// A missing map is an error, not an implicit clear ray. Does not need a capture.
    pub fn line_of_sight(&self, from: GridPosition, to: GridPosition,
        budget: &mut CombatQueryBudget) -> Result<bool, CombatQueryError>
    {
        let map = self.map.ok_or(CombatQueryError::MapRequired)?;
        budget.sight_step()?;
        let mut scan = SegmentScan::new(map, from, to);
        loop {
            if let Some(clear) = scan.result { return Ok(clear); }
            budget.sight_step()?;
            scan.advance(map);
        }
    }

    /// Clears output on entry and on failure. Reuses its allocation; successful
    /// hits sort by (entity.index, generation), independent of bucket/dense order.
    pub fn collect(&self, query: CombatQuery, budget: &mut CombatQueryBudget,
        output: &mut Vec<CombatQueryHit>, mut accept: impl FnMut(CombatQueryHit) -> bool)
        -> Result<CombatQueryReport, CombatQueryError>
    {
        output.clear();
        self.collect_append(query, budget, output, MAX_COMBAT_QUERY_HITS, &mut accept)
    }

    /// Linear minimum selection, without building/sorting a hit vector. Distance
    /// ties use the entity handle. Failure never returns a partial "best" target.
    pub fn nearest(&self, request: CombatNearestQuery, budget: &mut CombatQueryBudget,
        mut accept: impl FnMut(CombatQueryHit) -> bool) -> Result<CombatNearestResult, CombatQueryError>
    {
        let tick = self.validate(request.query)?;
        let before = budget.work();
        let mut best: Option<(u128, CombatQueryHit)> = None;
        self.frame.visit_combat_candidates(request.query.area.bounds(), budget, |motion, budget| {
            if request.query.exclude == Some(motion.entity) || !request.query.area.contains(motion.position) { return Ok(()); }
            let hit = CombatQueryHit { entity: motion.entity, position: motion.position };
            let distance = distance_squared(request.origin, hit.position);
            if best.is_some_and(|(old, target)| (distance, key(hit.entity)) >= (old, key(target.entity))) { return Ok(()); }
            if accept(hit) && self.can_see(request.query, hit, budget)? { best = Some((distance, hit)); }
            Ok(())
        })?;
        Ok(CombatNearestResult { tick, target: best.map(|(_, hit)| hit), work: budget.work().since(before) })
    }

    /// Structural errors preserve all outputs. Requests then run in input order
    /// with one shared allowance; each failure discards only that request's hits.
    /// Results after queries.len() are untouched. No automatic retry or deferral.
    pub fn collect_batch(&self, queries: &[CombatQuery], budget: &mut CombatQueryBudget,
        output: &mut Vec<CombatQueryHit>, results: &mut [Result<CombatQueryReport, CombatQueryError>],
        mut accept: impl FnMut(usize, CombatQueryHit) -> bool) -> Result<usize, CombatQueryError>
    {
        check_batch(queries.len(), results.len())?;
        output.clear();
        for (index, &query) in queries.iter().enumerate() {
            results[index] = self.collect_append(query, budget, output, MAX_COMBAT_BATCH_HITS,
                &mut |hit| accept(index, hit));
        }
        Ok(queries.len())
    }

    pub fn nearest_batch(&self, queries: &[CombatNearestQuery], budget: &mut CombatQueryBudget,
        results: &mut [Result<CombatNearestResult, CombatQueryError>],
        mut accept: impl FnMut(usize, CombatQueryHit) -> bool) -> Result<usize, CombatQueryError>
    {
        check_batch(queries.len(), results.len())?;
        for (index, &query) in queries.iter().enumerate() {
            results[index] = self.nearest(query, budget, |hit| accept(index, hit));
        }
        Ok(queries.len())
    }

    fn validate(&self, query: CombatQuery) -> Result<Tick, CombatQueryError> {
        let tick = self.frame.tick.ok_or(CombatQueryError::Spatial(SyncError::NoFrame))?;
        if query.sight_origin.is_some() && self.map.is_none() { return Err(CombatQueryError::MapRequired); }
        Ok(tick)
    }
    fn can_see(&self, query: CombatQuery, hit: CombatQueryHit,
        budget: &mut CombatQueryBudget) -> Result<bool, CombatQueryError>
    {
        match query.sight_origin {
            Some(from) => self.line_of_sight(from, hit.position, budget),
            None => Ok(true),
        }
    }
    fn collect_append(&self, query: CombatQuery, budget: &mut CombatQueryBudget,
        output: &mut Vec<CombatQueryHit>, total_limit: usize, accept: &mut impl FnMut(CombatQueryHit) -> bool)
        -> Result<CombatQueryReport, CombatQueryError>
    {
        let tick = self.validate(query)?;
        let before = budget.work();
        let offset = output.len();
        let result = self.frame.visit_combat_candidates(query.area.bounds(), budget, |motion, budget| {
            if query.exclude == Some(motion.entity) || !query.area.contains(motion.position) { return Ok(()); }
            let hit = CombatQueryHit { entity: motion.entity, position: motion.position };
            if !accept(hit) || !self.can_see(query, hit, budget)? { return Ok(()); }
            if output.len()-offset == MAX_COMBAT_QUERY_HITS || output.len() == total_limit {
                return Err(CombatQueryError::HitLimitExceeded);
            }
            output.push(hit);
            Ok(())
        });
        if let Err(error) = result { output.truncate(offset); return Err(error); }
        output[offset..].sort_unstable_by_key(|hit| key(hit.entity));
        Ok(CombatQueryReport { tick, offset, count: output.len()-offset, work: budget.work().since(before) })
    }
}

fn check_batch(count: usize, available: usize) -> Result<(), CombatQueryError> {
    if count > MAX_COMBAT_QUERIES { return Err(CombatQueryError::TooManyQueries); }
    if available < count { return Err(CombatQueryError::ResultsTooSmall); }
    Ok(())
}
fn distance_squared(a: GridPosition, b: GridPosition) -> u128 {
    // The sum for opposite i32 corners can exceed u64; ranking must not wrap.
    let x = u128::from((i64::from(a.x)-i64::from(b.x)).unsigned_abs());
    let y = u128::from((i64::from(a.y)-i64::from(b.y)).unsigned_abs());
    x*x+y*y
}
