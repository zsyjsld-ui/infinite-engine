use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::collections::hash_map::Entry;
use std::hash::{BuildHasherDefault, Hasher};

use crate::collision::{SegmentScan, NAV_STEPS};
use crate::{CollisionMap, GridPosition, NavigationBudget};
mod goal;
pub(crate) use goal::SharedGoals;

pub const MAX_PATH_SEARCH_NODES: usize = 8192;
pub const MAX_PATH_WAYPOINTS: usize = 1024;
const MAX_OPEN_ENTRIES: usize = 32768;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    InvalidStart,
    InvalidGoal,
    /// The reachable component of the static center-cell graph was exhausted.
    Unreachable,
    /// Resource limit reached; this does not prove that no path exists.
    SearchLimit,
    PathTooLong,
    /// Shared allowance exhausted; retry later, not evidence of no route.
    BudgetExhausted,
}

/// Ordered destinations, excluding the starting point and including the goal.
/// An already-reached goal has an empty route. No navigation radius is implied.
#[derive(Clone, Debug)]
pub struct PlannedPath {
    waypoints: Vec<GridPosition>,
}

impl PlannedPath {
    pub fn waypoints(&self) -> &[GridPosition] {
        &self.waypoints
    }

    pub(crate) fn into_waypoints(self) -> Vec<GridPosition> {
        self.waypoints
    }
}

/// Public bounded continuation over the same search used by World's queue.
/// Useful for presentation-only recovery; callers supply their own shared
/// allowance and never gain extra work by polling again.
pub struct PathTask { search:PathSearch }
impl PathTask {
    pub fn new(from:GridPosition,goal:GridPosition)->Self{let mut search=PathSearch::new(from,goal);search.enable_smoothing();Self{search}}
    pub fn advance(&mut self,map:&CollisionMap,budget:&mut NavigationBudget)->Option<Result<PlannedPath,PathError>>{self.search.resume(Some(map),budget)}
}

#[derive(Clone, Copy)]
struct Node {
    parent: Option<u32>,
    cost: u64,
    closed: bool,
}

/// Only bounded map-cell u32 indices are keys here, never external strings or
/// secrets. An integer avalanche avoids general-purpose byte hashing in A*.
#[derive(Default)]
struct CellKeyHasher(u64);
impl Hasher for CellKeyHasher {
    fn write_u32(&mut self, value: u32) { self.0 = u64::from(value); }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes { self.0 = self.0.rotate_left(8) ^ u64::from(byte); }
    }
    fn finish(&self) -> u64 {
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
}

/// Reusable sparse A* scratch space, bounded independently of total map area.
/// Heap ties use cost and cell index; HashMap iteration never determines the route.
#[derive(Default)]
pub struct PathPlanner {
    nodes: HashMap<u32, Node, BuildHasherDefault<CellKeyHasher>>,
    open: BinaryHeap<Reverse<(u64, u64, u32)>>,
    chain: Vec<u32>,
}

impl PathPlanner {
    pub fn plan(
        &mut self, map: &CollisionMap, from: GridPosition, goal: GridPosition,
    ) -> Result<PlannedPath, PathError> {
        self.plan_with_budget(map, from, goal, &mut NavigationBudget::default())
    }

    /// Synchronous adapter over the same resumable search used by the queue.
    /// Share a budget across calls to bound a batch. On exhaustion this adapter
    /// returns an error and discards progress; use World's queue to retain it.
    pub fn plan_with_budget(
        &mut self, map: &CollisionMap, from: GridPosition, goal: GridPosition,
        budget: &mut NavigationBudget,
    ) -> Result<PlannedPath, PathError> {
        self.run(map, from, goal, budget, false)
    }

    /// World already checked the direct segment with the shared allowance.
    pub(crate) fn plan_detour(
        &mut self, map: &CollisionMap, from: GridPosition, goal: GridPosition,
        budget: &mut NavigationBudget,
    ) -> Result<PlannedPath, PathError> {
        self.run(map, from, goal, budget, true)
    }

    fn run(
        &mut self, map: &CollisionMap, from: GridPosition, goal: GridPosition,
        budget: &mut NavigationBudget, skip_ray: bool,
    ) -> Result<PlannedPath, PathError> {
        budget.charge(1)?;
        if !map.is_walkable(from) { return Err(PathError::InvalidStart); }
        if !map.is_walkable(goal) { return Err(PathError::InvalidGoal); }
        if from == goal { return Ok(PlannedPath { waypoints: Vec::new() }); }
        // Preserve the allocation-free-search fast path without clearing a
        // previously large scratch table for an unobstructed short request.
        if !skip_ray && map.navigation_segment_is_clear(from, goal, budget)? {
            return Ok(PlannedPath { waypoints: vec![goal] });
        }
        budget.charge((self.nodes.capacity() + self.open.len() + self.chain.len()) as u32)?;
        self.nodes.clear();
        self.open.clear();
        self.chain.clear();
        let mut search = PathSearch::new(from, goal);
        search.scratch = std::mem::take(self);
        search.phase = SearchPhase::Connect; // Straight visibility already checked.
        let result = search.resume(Some(map), budget);
        *self = search.scratch; // Keep allocation capacity, not unfinished state.
        match result {
            Some(result) => result,
            None => {
                budget.charge(budget.remaining()).expect("remaining allowance fits");
                Err(PathError::BudgetExhausted)
            }
        }
    }
}

// Only merge collinear, forward-going segments; no additional visibility scans.
fn append_point(points: &mut Vec<GridPosition>, point: GridPosition) {
    if points.last() == Some(&point) { return; }
    while points.len() >= 2 {
        let a = points[points.len() - 2];
        let b = points[points.len() - 1];
        let (ax, ay) = (i128::from(b.x) - i128::from(a.x), i128::from(b.y) - i128::from(a.y));
        let (bx, by) = (i128::from(point.x) - i128::from(b.x), i128::from(point.y) - i128::from(b.y));
        if ax * by != ay * bx || ax * bx + ay * by <= 0 { break; }
        points.pop();
    }
    points.push(point);
}

#[derive(Clone, Copy)]
enum SearchPhase { Start, Ray, Connect, Search, Trace(u32), Build, Smooth, Finish, SharedSearch, SharedTrace(u32) }

/// Each queued request owns its cursor and bounded sparse scratch. Unlike the
/// synchronous planner, exhausting a slice leaves every unfinished phase intact.
pub(crate) struct PathSearch {
    from: GridPosition,
    goal: GridPosition,
    phase: SearchPhase,
    scan: Option<SegmentScan>,
    scratch: PathPlanner,
    start: u32,
    end: u32,
    points: Vec<GridPosition>,
    smooth:bool,
    smooth_anchor:usize,
    smooth_candidate:usize,
    smooth_points:Vec<GridPosition>,
    shared: Option<std::sync::Arc<std::sync::Mutex<goal::GoalField>>>,
}

impl PathSearch {
    pub fn phase(&self) -> crate::NavigationPhase {
        use crate::NavigationPhase;
        match self.phase {
            SearchPhase::Start => NavigationPhase::Queued,
            SearchPhase::Ray => NavigationPhase::CheckingDirect,
            SearchPhase::Connect => NavigationPhase::Connecting,
            SearchPhase::Search | SearchPhase::SharedSearch => NavigationPhase::Searching,
            SearchPhase::Trace(_) | SearchPhase::SharedTrace(_) => NavigationPhase::Tracing,
            SearchPhase::Build | SearchPhase::Smooth | SearchPhase::Finish => NavigationPhase::BuildingPath,
        }
    }

    pub fn new(from: GridPosition, goal: GridPosition) -> Self {
        Self {
            from, goal, phase: SearchPhase::Start, scan: None,
            scratch: PathPlanner::default(), start: 0, end: 0, points: Vec::new(), shared:None,
            smooth:false,smooth_anchor:0,smooth_candidate:0,smooth_points:Vec::new(),
        }
    }

    pub(crate) fn enable_smoothing(&mut self){self.smooth=true;}
    fn finish_build(&mut self){
        if self.smooth&&self.points.len()>2 {
            self.smooth_anchor=0;self.smooth_candidate=(self.points.len()-1).min(32);
            self.smooth_points.push(self.points[0]);self.scan=None;self.phase=SearchPhase::Smooth;
        }else{self.phase=SearchPhase::Finish;}
    }

    /// None means pending, never unreachable. Use only the original immutable map.
    /// Completion includes prepaid work for converting at most 1024 route legs.
    pub fn resume(
        &mut self, map: Option<&CollisionMap>, budget: &mut NavigationBudget,
    ) -> Option<Result<PlannedPath, PathError>> {
        self.resume_inner(map,budget,None,false)
    }
    pub fn used_shared_route(&self)->bool { self.shared.is_some() }
    pub fn resume_shared(&mut self,map:Option<&CollisionMap>,budget:&mut NavigationBudget,
        fields:&mut SharedGoals, grouped:bool)->Option<Result<PlannedPath,PathError>> {
        self.resume_inner(map,budget,Some(fields),grouped)
    }
    fn resume_inner(&mut self,map:Option<&CollisionMap>,budget:&mut NavigationBudget,
        mut fields:Option<&mut SharedGoals>,grouped:bool)->Option<Result<PlannedPath,PathError>> {
        loop {
            if matches!(self.phase,SearchPhase::SharedSearch){
                let result=self.shared.as_ref().unwrap().lock().expect("single-owner goal search").reach(map.unwrap(),self.start,budget);
                match result {
                    None=>return None,
                    Some(Ok(()))=>{self.points.push(self.from);self.phase=SearchPhase::SharedTrace(self.start);},
                    // A bounded shared frontier may grow beyond an efficient
                    // individual A* search. Preserve that existing fallback.
                    Some(Err(PathError::SearchLimit))=>{self.shared=None;self.phase=SearchPhase::Search;},
                    Some(Err(error))=>return Some(Err(error)),
                }
                continue;
            }
            let cost = match self.phase {
                SearchPhase::Connect => 16,
                // Prepared edges remove eight geometric scans and distance
                // roots. Keep a bounded atomic expansion, with a smaller work
                // allowance for this measured fast path (not a time guarantee).
                SearchPhase::Search => if map.is_some_and(|m| m.navigation_cache_bytes() != 0) { 16 } else { 64 },
                SearchPhase::Build => 4,
                SearchPhase::SharedTrace(_) => 4,
                SearchPhase::Finish => 4 * self.points.len() as u32 + 1,
                _ => 1,
            };
            if !budget.try_charge(cost) { return None; }
            if matches!(self.phase, SearchPhase::Start) {
                if self.from == self.goal {
                    return Some(Ok(PlannedPath { waypoints: Vec::new() }));
                }
                let Some(map) = map else {
                    return Some(Ok(PlannedPath { waypoints: vec![self.goal] }));
                };
                if !map.is_walkable(self.from) { return Some(Err(PathError::InvalidStart)); }
                if !map.is_walkable(self.goal) { return Some(Err(PathError::InvalidGoal)); }
                self.scan = Some(SegmentScan::new(map, self.from, self.goal));
                self.phase = SearchPhase::Ray;
                continue;
            }
            let map = map.expect("map-bound search after Start");
            match self.phase {
                SearchPhase::Start => unreachable!(),
                SearchPhase::Ray => {
                    let scan = self.scan.as_mut().expect("initialized ray");
                    scan.advance(map);
                    match scan.result {
                        Some(true) => return Some(Ok(PlannedPath { waypoints: vec![self.goal] })),
                        Some(false) => { self.scan = None; self.phase = SearchPhase::Connect; }
                        None => {}
                    }
                }
                SearchPhase::Connect => {
                    self.start = map.nav_cell(self.from).expect("validated start");
                    self.end = map.nav_cell(self.goal).expect("validated goal");
                    if !map.segment_is_clear(self.from, map.nav_center(self.start))
                        || !map.segment_is_clear(map.nav_center(self.end), self.goal)
                    {
                        return Some(Err(PathError::Unreachable));
                    }
                    self.scratch.nodes.insert(self.start, Node { parent: None, cost: 0, closed: false });
                    self.scratch.open.push(Reverse((map.nav_estimate(self.start, self.end), 0, self.start)));
                    self.phase = if let Some(fields)=fields.as_deref_mut().filter(|f|grouped||f.contains(self.end)) {
                        self.shared=Some(fields.get(self.end));SearchPhase::SharedSearch
                    } else {SearchPhase::Search};
                }
                SearchPhase::Search => {
                    let Some(Reverse((_, cost, cell))) = self.scratch.open.pop() else {
                        return Some(Err(PathError::Unreachable));
                    };
                    let node = self.scratch.nodes.get_mut(&cell).expect("queued node");
                    if node.closed || node.cost != cost { continue; }
                    if cell == self.end { self.phase = SearchPhase::Trace(cell); continue; }
                    node.closed = true;
                    let (width, height) = map.nav_dimensions();
                    let (x, y) = (i64::from(cell % width), i64::from(cell / width));
                    let cached_edges = map.navigation_cache_bytes() != 0;
                    for (direction, (dx, dy, edge_cost)) in NAV_STEPS.into_iter().enumerate() {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < 0 || ny < 0 || nx >= i64::from(width) || ny >= i64::from(height) { continue; }
                        let next = ny as u32 * width + nx as u32;
                        if cached_edges && !map.nav_edge_is_clear(cell, next, direction) { continue; }
                        let next_cost = cost + map.nav_edge_cost(cell, next, edge_cost);
                        let at_node_limit = self.scratch.nodes.len() >= MAX_PATH_SEARCH_NODES;
                        // Retain a single hash lookup through relax/update instead
                        // of get + contains_key + insert for every improved edge.
                        let entry = self.scratch.nodes.entry(next);
                        if let Entry::Occupied(ref known) = entry {
                            if known.get().closed || next_cost >= known.get().cost { continue; }
                        }
                        if !cached_edges && !map.nav_edge_is_clear(cell, next, direction) { continue; }
                        if (matches!(&entry, Entry::Vacant(_)) && at_node_limit)
                            || self.scratch.open.len() >= MAX_OPEN_ENTRIES
                        {
                            return Some(Err(PathError::SearchLimit));
                        }
                        let updated = Node { parent: Some(cell), cost: next_cost, closed: false };
                        match entry {
                            Entry::Occupied(mut known) => { known.insert(updated); },
                            Entry::Vacant(slot) => { slot.insert(updated); },
                        }
                        self.scratch.open.push(Reverse((next_cost + map.nav_estimate(next, self.end), next_cost, next)));
                    }
                }
                SearchPhase::Trace(cell) => {
                    self.scratch.chain.push(cell);
                    if cell == self.start {
                        self.points.push(self.from);
                        self.phase = SearchPhase::Build;
                    } else {
                        self.phase = SearchPhase::Trace(self.scratch.nodes[&cell].parent.expect("path parent"));
                    }
                }
                SearchPhase::Build => {
                    if let Some(cell) = self.scratch.chain.pop() {
                        append_point(&mut self.points, map.nav_center(cell));
                    } else {
                        append_point(&mut self.points, self.goal);
                        if self.points.len() > MAX_PATH_WAYPOINTS + 1 {
                            return Some(Err(PathError::PathTooLong));
                        }
                        self.finish_build();
                    }
                }
                SearchPhase::Smooth => {
                    // At most 32 candidates per retained corner. Every scan
                    // resumes through the same cell-work budget as A*, so a
                    // long shortcut check cannot block an application frame.
                    let accepted=if self.smooth_candidate==self.smooth_anchor+1{Some(true)}else{
                        if self.scan.is_none(){self.scan=Some(SegmentScan::new(map,self.points[self.smooth_anchor],self.points[self.smooth_candidate]));}
                        let scan=self.scan.as_mut().unwrap();scan.advance(map);scan.result
                    };
                    match accepted {
                        None=>{},
                        Some(false)=>{self.scan=None;self.smooth_candidate-=1;},
                        Some(true)=>{
                            self.scan=None;self.smooth_points.push(self.points[self.smooth_candidate]);self.smooth_anchor=self.smooth_candidate;
                            if self.smooth_anchor+1==self.points.len(){self.points=std::mem::take(&mut self.smooth_points);self.phase=SearchPhase::Finish;}
                            else{self.smooth_candidate=(self.smooth_anchor+32).min(self.points.len()-1);}
                        },
                    }
                }
                SearchPhase::Finish => {
                    self.points.remove(0);
                    return Some(Ok(PlannedPath { waypoints: std::mem::take(&mut self.points) }));
                }
                SearchPhase::SharedTrace(cell)=>{
                    append_point(&mut self.points,map.nav_center(cell));
                    if self.points.len()>MAX_PATH_WAYPOINTS+1{return Some(Err(PathError::PathTooLong));}
                    let next=self.shared.as_ref().unwrap().lock().expect("single-owner goal trace").parent(cell);
                    if let Some(next)=next{self.phase=SearchPhase::SharedTrace(next);}else{
                        append_point(&mut self.points,self.goal);
                        if self.points.len()>MAX_PATH_WAYPOINTS+1{return Some(Err(PathError::PathTooLong));}
                        self.finish_build();
                    }
                }
                SearchPhase::SharedSearch=>unreachable!(),
            }
        }
    }
}
