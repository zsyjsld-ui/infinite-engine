use std::num::{NonZeroU16, NonZeroU32};

use crate::{CollisionMap, GridPosition, PlannedPath};

pub const POSITION_UNITS_PER_PIXEL: u32 = 16;
pub const DEFAULT_TICK_MS: NonZeroU32 = NonZeroU32::new(50).unwrap();

/// Current intent or the last completed movement outcome, not an event queue.
/// Numeric values are an internal representation, not a network protocol.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MovementStatus {
    #[default]
    Idle = 0,
    Moving = 1,
    Arrived = 2,
    Stopped = 3,
    Blocked = 4,
}

#[derive(Clone)]
struct Leg {
    target: GridPosition,
    length: u64,
}

/// A cached segment plus optional remaining route. All leg lengths are prepared
/// when intent changes, not recomputed each tick. Direct motion has no route allocation.
#[derive(Clone)]
pub(crate) struct Movement {
    origin: GridPosition,
    segment_target: GridPosition,
    // Final application goal, not the current intermediate waypoint.
    pub target: GridPosition,
    pub speed: NonZeroU16,
    pub planned: bool,
    length: u64,
    progress: u64,
    // Reverse order permits O(1) removal without shifting the remaining route.
    pending: Vec<Leg>,
    // Fractional movement credit, in 1/1000 position unit, always below 1000.
    pub remainder: u16,
}

struct ControlledProgress {
    position: GridPosition, origin: GridPosition, target: GridPosition,
    length: u64, progress: u64, pending: usize, arrived: bool,
}

/// Read-only route presentation before display correction. Heading is the
/// current certified segment, never the quantized displacement of one frame.
pub struct ControlledPresentation {
    pub position:GridPosition,pub heading_from:GridPosition,pub heading_to:GridPosition,pub arrived:bool,
}

fn projected(origin: GridPosition, target: GridPosition, progress: u64, length: u64) -> GridPosition {
    if progress == length { return target; }
    let axis = |a:i32,b:i32| (i128::from(a)+(i128::from(b)-i128::from(a))*i128::from(progress)/i128::from(length)) as i32;
    GridPosition{x:axis(origin.x,target.x),y:axis(origin.y,target.y)}
}

fn within(point:GridPosition, goal:GridPosition, radius:u32)->bool {
    let x=i128::from(point.x)-i128::from(goal.x);let y=i128::from(point.y)-i128::from(goal.y);
    x*x+y*y<=i128::from(radius)*i128::from(radius)
}

// First representable point inside the arrival disk on this certified leg.
// Closest-point projection avoids the potentially overflowing quadratic
// discriminant for opposite i32 endpoints. No floating-point roots are used.
fn radius_entry(origin:GridPosition,target:GridPosition,length:u64,begin:u64,end:u64,goal:GridPosition,radius:u32)->Option<u64>{
    if radius==0 {return (target==goal&&end==length).then_some(end);}
    let test=|p|within(projected(origin,target,p,length),goal,radius);
    if test(begin){return Some(begin);}
    let dx=i128::from(target.x)-i128::from(origin.x);let dy=i128::from(target.y)-i128::from(origin.y);
    let gx=i128::from(goal.x)-i128::from(origin.x);let gy=i128::from(goal.y)-i128::from(origin.y);
    let closest=((gx*dx+gy*dy)*i128::from(length)/(dx*dx+dy*dy)).clamp(i128::from(begin),i128::from(end)) as u64;
    let mut high=None;
    // Quantization may move the closest lattice sample by one diagonal unit.
    for p in closest.saturating_sub(2).max(begin)..=closest.saturating_add(2).min(end){if test(p){high=Some(p);break;}}
    let mut high=high.or_else(||test(end).then_some(end))?;let mut low=begin;
    while high-low>1 {let middle=low+(high-low)/2;if test(middle){high=middle}else{low=middle}}
    Some(high)
}

impl Movement {
    fn controlled_progress(&self,from:GridPosition,mut budget:u64,radius:u32,map:Option<&CollisionMap>)->Result<ControlledProgress,()> {
        let mut state=ControlledProgress{position:from,origin:self.origin,target:self.segment_target,length:self.length,
            progress:self.progress,pending:self.pending.len(),arrived:within(from,self.target,radius)};
        if state.arrived||budget==0{return Ok(state);}
        loop {
            let end=state.progress+budget.min(state.length-state.progress);
            let entry=radius_entry(state.origin,state.target,state.length,state.progress,end,self.target,radius);
            let next_progress=entry.unwrap_or(end);
            let next=projected(state.origin,state.target,next_progress,state.length);
            if next!=state.position&&map.is_some_and(|m|!m.segment_is_clear(state.position,next)){return Err(());}
            budget-=next_progress-state.progress;state.progress=next_progress;state.position=next;
            if entry.is_some(){state.arrived=true;return Ok(state);}
            if state.progress<state.length||budget==0{return Ok(state);}
            if state.pending==0{state.arrived=true;return Ok(state);}
            state.pending-=1;let leg=&self.pending[state.pending];
            state.origin=state.position;state.target=leg.target;state.length=leg.length;state.progress=0;
        }
    }
    /// Externally scheduled motion uses the same certified route and collision
    /// map, but gets one bounded microsecond allowance from its application.
    /// Preview is allocation-free and does not consume position or fractional credit.
    pub(crate) fn controlled_step(&mut self,from:GridPosition,micros:u32,speed:NonZeroU16,radius:u32,
        remainder:&mut u32,map:Option<&CollisionMap>)->Result<(GridPosition,bool),()> {
        let credit=u64::from(speed.get())*u64::from(micros)+u64::from(*remainder);
        let state=self.controlled_progress(from,credit/1_000_000,radius,map)?;
        self.origin=state.origin;self.segment_target=state.target;self.length=state.length;self.progress=state.progress;
        self.pending.truncate(state.pending);self.speed=speed;*remainder=(credit%1_000_000)as u32;
        Ok((state.position,state.arrived))
    }
    pub(crate) fn controlled_preview(&self,from:GridPosition,micros:u32,speed:NonZeroU16,radius:u32,remainder:u32,map:Option<&CollisionMap>)->Result<GridPosition,()> {
        let credit=u64::from(speed.get())*u64::from(micros)+u64::from(remainder);
        self.controlled_progress(from,credit/1_000_000,radius,map).map(|p|p.position)
    }
    pub(crate) fn controlled_presentation(&self,from:GridPosition,micros:u32,speed:NonZeroU16,radius:u32,remainder:u32,map:Option<&CollisionMap>)->Result<ControlledPresentation,()> {
        let credit=u64::from(speed.get())*u64::from(micros)+u64::from(remainder);
        let s=self.controlled_progress(from,credit/1_000_000,radius,map)?;
        Ok(ControlledPresentation{position:s.position,heading_from:s.origin,heading_to:s.target,arrived:s.arrived})
    }
    pub(crate) fn segment_target(&self)->GridPosition{self.segment_target}
    pub(crate) fn remaining_waypoints(&self,maximum:usize)->Vec<GridPosition>{
        std::iter::once(self.segment_target).chain(self.pending.iter().rev().map(|leg|leg.target)).take(maximum).collect()
    }
    pub fn new(
        origin: GridPosition,
        target: GridPosition,
        speed: NonZeroU16,
        remainder: u16,
    ) -> Option<Self> {
        if origin == target {
            return None;
        }
        Some(Self {
            origin, segment_target: target, target, speed, planned: false,
            length: distance(origin, target), progress: 0, pending: Vec::new(), remainder,
        })
    }

    pub fn from_path(
        origin: GridPosition, path: PlannedPath, speed: NonZeroU16, remainder: u16,
    ) -> Option<Self> {
        Self::from_waypoints(origin,path.into_waypoints(),speed,remainder)
    }
    pub(crate) fn from_waypoints(origin:GridPosition,points:Vec<GridPosition>,speed:NonZeroU16,remainder:u16)->Option<Self>{
        let mut pending = Vec::with_capacity(points.len());
        let mut previous = origin;
        for target in points {
            if target != previous {
                pending.push(Leg { target, length: distance(previous, target) });
                previous = target;
            }
        }
        pending.reverse();
        let first = pending.pop()?;
        Some(Self {
            origin, segment_target: first.target, target: previous, speed, planned: true,
            length: first.length, progress: 0, pending, remainder,
        })
    }

    /// One distance budget for the entire tick, even across several corners.
    /// Check each actual subsegment, never the shortcut chord across a turn.
    /// On Err the world discards this movement and retains its pre-tick position.
    pub fn advance(
        &mut self, tick_ms: NonZeroU32, from: GridPosition, map: Option<&CollisionMap>,
    ) -> Result<GridPosition, ()> {
        let credit = u64::from(self.speed.get()) * u64::from(tick_ms.get())
            + u64::from(self.remainder);
        self.remainder = (credit % 1000) as u16;
        let mut budget = credit / 1000;
        // World already validated the current point; its map is immutable.
        // Keep fractional credit, but no projection/collision work is needed
        // when this tick cannot consume even one position unit.
        if budget == 0 { return Ok(from); }
        let mut position = from;
        loop {
            let consumed = budget.min(self.length - self.progress);
            self.progress += consumed;
            budget -= consumed;
            let next = self.segment_position();
            if next != position && map.is_some_and(|map| !map.segment_is_clear(position, next)) { return Err(()); }
            position = next;
            if self.progress < self.length { return Ok(position); }
            let Some(leg) = self.pending.pop() else { return Ok(position) };
            self.origin = position;
            self.segment_target = leg.target;
            self.length = leg.length;
            self.progress = 0;
            if budget == 0 { return Ok(position); }
        }
    }

    fn segment_position(&self) -> GridPosition {
        if self.progress == self.length { return self.segment_target; }
        // Project cumulative progress from the original endpoints; rounding does
        // not accumulate every tick. Ordinary map segments fit the fast i64
        // multiply/divide; extreme coordinates keep the exact i128 fallback.
        // A segment between i32 endpoints has length/progress below 2^33.
        let project = |from: i32, to: i32| {
            let delta = i64::from(to) - i64::from(from);
            let offset = match delta.checked_mul(self.progress as i64) {
                Some(product) => product / self.length as i64,
                None => (i128::from(delta) * i128::from(self.progress) / i128::from(self.length)) as i64,
            };
            (i64::from(from) + offset) as i32
        };
        GridPosition {
            x: project(self.origin.x, self.segment_target.x),
            y: project(self.origin.y, self.segment_target.y),
        }
    }

    pub fn arrived(&self) -> bool {
        self.progress == self.length && self.pending.is_empty()
    }

    pub(crate) fn cursor(&self)->(GridPosition,u64,Vec<GridPosition>){
        (self.origin,self.progress,self.remaining_waypoints(crate::MAX_PATH_WAYPOINTS))
    }
    /// The world map is immutable. A suffix with the exact certified segment
    /// origin can reuse geometry validation, but must still validate progress.
    pub(crate) fn certifies_suffix(&self,origin:GridPosition,points:&[GridPosition])->bool{
        let count=1+self.pending.len();if points.is_empty()||points.len()>count{return false;}
        let skip=count-points.len();let point_at=|i:usize|if i==0{self.segment_target}else{self.pending[self.pending.len()-i].target};
        if origin!=if skip==0{self.origin}else{point_at(skip-1)}{return false;}
        points.iter().enumerate().all(|(i,&p)|p==point_at(skip+i))
    }
    pub(crate) fn restore_progress(&mut self,progress:u64,position:GridPosition)->Result<(),()>{
        if progress>self.length||projected(self.origin,self.segment_target,progress,self.length)!=position{return Err(());}
        self.progress=progress;Ok(())
    }
}

fn distance(origin: GridPosition, target: GridPosition) -> u64 {
    let dx = i128::from(target.x) - i128::from(origin.x);
    let dy = i128::from(target.y) - i128::from(origin.y);
    let squared = (dx * dx + dy * dy) as u128;
    let floor = squared.isqrt();
    (floor + u128::from(floor * floor < squared)) as u64
}
