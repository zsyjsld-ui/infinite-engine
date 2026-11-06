use std::num::NonZeroU32;

use crate::{GridPosition, NavigationBudget, PathError};
mod staggered;
use staggered::StaggeredScan;
mod connected;
use connected::ConnectedScan;

/// At most 2 MiB of occupancy bits per map in this initial implementation.
pub const MAX_COLLISION_CELLS: u64 = 16 * 1024 * 1024;
pub(crate) const NAV_STEPS: [(i64, i64, u64); 8] = [
    (1, 0, 10), (0, 1, 10), (-1, 0, 10), (0, -1, 10),
    (1, 1, 14), (-1, 1, 14), (-1, -1, 14), (1, -1, 14),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    TooManyCells,
    OutsideCoordinateRange,
    CellOutOfBounds,
    InvalidCellGeometry,
}

/// Square or staggered-row occupancy, one bit per cell (one means blocked).
/// Coordinates and cell size use the same 1/16-pixel units as movement.
/// Point collision; rendering projection and resource import belong to the app.
pub struct CollisionMap {
    origin: GridPosition,
    width: u32,
    height: u32,
    cell_size: u32,
    row_height: Option<u32>,
    connected_boundaries: bool,
    blocked: Vec<u8>,
    edge_costs: [u64; 3],
    navigation_edges: Option<Vec<u8>>,
    target_components: Option<Vec<u32>>,
}

/// Resumable supercover: one square crossing or one staggered occupancy cell
/// (with at most two row-corner guards) per advance.
/// Bound to the immutable map used at construction (internal API only).
pub(crate) struct SegmentScan {
    pub result: Option<bool>,
    inner: ScanKind,
}

enum ScanKind { Square(SquareScan), Staggered(StaggeredScan), Connected(ConnectedScan) }

impl SegmentScan {
    pub fn new(map: &CollisionMap, from: GridPosition, to: GridPosition) -> Self {
        if map.connected_boundaries {
            let scan=ConnectedScan::new(map,from,to);
            Self {result:scan.result,inner:ScanKind::Connected(scan)}
        } else if map.row_height.is_some() {
            let scan = StaggeredScan::new(map, from, to);
            Self { result: scan.result, inner: ScanKind::Staggered(scan) }
        } else {
            let scan = SquareScan::new(map, from, to);
            Self { result: scan.result, inner: ScanKind::Square(scan) }
        }
    }
    pub fn advance(&mut self, map: &CollisionMap) {
        self.result = match &mut self.inner {
            ScanKind::Square(scan) => { scan.advance(map); scan.result },
            ScanKind::Staggered(scan) => { scan.advance(map); scan.result },
            ScanKind::Connected(scan) => { scan.advance(map); scan.result },
        };
    }
}

struct SquareScan {
    pub result: Option<bool>,
    x: i64,
    y: i64,
    end_x: i64,
    end_y: i64,
    step_x: i64,
    step_y: i64,
    ax: u64,
    ay: u64,
    next_x: u64,
    next_y: u64,
    along_vertical: bool,
    along_horizontal: bool,
}

impl SquareScan {
    pub fn new(map: &CollisionMap, from: GridPosition, to: GridPosition) -> Self {
        let size = i64::from(map.cell_size);
        let sx = i64::from(from.x) - i64::from(map.origin.x);
        let sy = i64::from(from.y) - i64::from(map.origin.y);
        let ex = i64::from(to.x) - i64::from(map.origin.x);
        let ey = i64::from(to.y) - i64::from(map.origin.y);
        let (x, y, end_x, end_y) = (sx / size, sy / size, ex / size, ey / size);
        let (dx, dy) = (ex - sx, ey - sy);
        let result = if !map.is_walkable(from) || !map.is_walkable(to) {
            Some(false)
        } else if x == end_x && y == end_y {
            Some(true)
        } else {
            None
        };
        Self {
            result, x, y, end_x, end_y, step_x: dx.signum(), step_y: dy.signum(),
            ax: dx.unsigned_abs(), ay: dy.unsigned_abs(),
            next_x: (if dx > 0 { (x + 1) * size - sx } else { sx - x * size }) as u64,
            next_y: (if dy > 0 { (y + 1) * size - sy } else { sy - y * size }) as u64,
            along_vertical: dx == 0 && sx % size == 0,
            along_horizontal: dy == 0 && sy % size == 0,
        }
    }

    pub fn advance(&mut self, map: &CollisionMap) {
        if self.result.is_some() { return; }
        let need_x = self.x != self.end_x;
        let need_y = self.y != self.end_y;
        let cross_x = need_x && (!need_y || u128::from(self.next_x) * u128::from(self.ay)
            <= u128::from(self.next_y) * u128::from(self.ax));
        let cross_y = need_y && (!need_x || u128::from(self.next_y) * u128::from(self.ax)
            <= u128::from(self.next_x) * u128::from(self.ay));
        if cross_x && cross_y
            && (!map.cell_is_open(self.x + self.step_x, self.y)
                || !map.cell_is_open(self.x, self.y + self.step_y))
        {
            self.result = Some(false);
            return;
        }
        if cross_x { self.x += self.step_x; self.next_x += u64::from(map.cell_size); }
        if cross_y { self.y += self.step_y; self.next_y += u64::from(map.cell_size); }
        if !map.touch_is_open(self.x, self.y, self.along_vertical, self.along_horizontal) {
            self.result = Some(false);
        } else if self.x == self.end_x && self.y == self.end_y {
            self.result = Some(true);
        }
    }
}

impl CollisionMap {
    /// Create an initially open map. Left/top bounds are inclusive and
    /// right/bottom bounds exclusive; all representable positions must fit i32.
    pub fn new(
        origin: GridPosition,
        width: NonZeroU32,
        height: NonZeroU32,
        cell_size: NonZeroU32,
    ) -> Result<Self, MapError> {
        let (width, height, cell_size) = (width.get(), height.get(), cell_size.get());
        let cells = u64::from(width) * u64::from(height);
        if cells > MAX_COLLISION_CELLS {
            return Err(MapError::TooManyCells);
        }
        let available_x = (i64::from(i32::MAX) + 1 - i64::from(origin.x)) as u64;
        let available_y = (i64::from(i32::MAX) + 1 - i64::from(origin.y)) as u64;
        if u64::from(width) * u64::from(cell_size) > available_x
            || u64::from(height) * u64::from(cell_size) > available_y
        {
            return Err(MapError::OutsideCoordinateRange);
        }
        Ok(Self {
            origin, width, height, cell_size, row_height: None, connected_boundaries:false,
            blocked: vec![0; ((cells + 7) / 8) as usize],
            edge_costs: [10, 14, 14], navigation_edges: None,target_components:None,
        })
    }

    /// Configure cells before transferring the map into a World.
    pub fn set_blocked(&mut self, x: u32, y: u32, blocked: bool) -> Result<(), MapError> {
        if x >= self.width || y >= self.height {
            return Err(MapError::CellOutOfBounds);
        }
        let index = y as usize * self.width as usize + x as usize;
        let mask = 1u8 << (index & 7);
        if (self.blocked[index >> 3] & mask != 0) != blocked {
            // A configured-map edit invalidates every cached edge. Runtime maps
            // are immutable; never allow a stale connection to bypass collision.
            self.navigation_edges = None;
            self.target_components = None;
        }
        if blocked {
            self.blocked[index >> 3] |= mask;
        } else {
            self.blocked[index >> 3] &= !mask;
        }
        Ok(())
    }

    /// Internal blocked-cell edges and corners are solid, including tangency.
    pub fn is_walkable(&self, point: GridPosition) -> bool {
        if self.connected_boundaries { return self.connected_cell(point).is_some(); }
        if self.row_height.is_some() { return self.staggered_point_is_open(point); }
        let Some((x, y)) = self.local(point) else { return false };
        let size = i64::from(self.cell_size);
        self.touch_is_open(x / size, y / size, x % size == 0, y % size == 0)
    }

    /// Visit every cell touched by the closed segment using integer boundary
    /// fractions. No fixed-distance sampling, floating point, or allocation.
    pub fn segment_is_clear(&self, from: GridPosition, to: GridPosition) -> bool {
        self.scan_segment(from, to, || Ok(())).expect("unlimited scan")
    }

    pub(crate) fn navigation_segment_is_clear(
        &self, from: GridPosition, to: GridPosition, budget: &mut NavigationBudget,
    ) -> Result<bool, PathError> {
        self.scan_segment(from, to, || budget.charge(1))
    }

    // One unit covers endpoint checks; each crossing checks a bounded number
    // of cells. Ordinary movement supplies a statically no-op charger.
    fn scan_segment(
        &self, from: GridPosition, to: GridPosition,
        mut charge: impl FnMut() -> Result<(), PathError>,
    ) -> Result<bool, PathError> {
        charge()?;
        let mut scan = SegmentScan::new(self, from, to);
        loop {
            if let Some(clear) = scan.result { return Ok(clear); }
            charge()?;
            scan.advance(self);
        }
    }

    pub(crate) fn nav_dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Optional load-time preparation for immutable maps: one byte per cell,
    /// one bit per directed neighbour. Uses the SAME exact collision predicate
    /// as uncached search. Not part of a simulation tick; O(cells * 8) work.
    /// Occupancy edits invalidate this table; uncached navigation remains valid.
    pub fn prepare_navigation(&mut self) {
        let mut edges = vec![0u8; self.width as usize * self.height as usize];
        for y in 0..self.height {
            for x in 0..self.width {
                if !self.cell_is_open(i64::from(x), i64::from(y)) { continue; }
                let cell = y * self.width + x;
                for (direction, &(dx, dy, _)) in NAV_STEPS.iter().enumerate() {
                    let (nx, ny) = (i64::from(x) + dx, i64::from(y) + dy);
                    if !self.cell_is_open(nx, ny) { continue; }
                    let next = ny as u32 * self.width + nx as u32;
                    if self.nav_edge_is_clear_uncached(cell, next) {
                        edges[cell as usize] |= 1 << direction;
                    }
                }
            }
        }
        self.navigation_edges = Some(edges);
    }

    pub fn navigation_cache_bytes(&self) -> usize {
        self.navigation_edges.as_ref().map_or(0, Vec::len)+self.target_components.as_ref().map_or(0,|v|v.len()*4)
    }

    /// Optional immutable-map preparation for nearest reachable click targets.
    /// Uses the exact same centre edges as navigation, not geometric proximity.
    /// Load-time O(cells+edges), four extra bytes/cell; edits invalidate it.
    pub fn prepare_target_selection(&mut self){
        if self.target_components.is_some(){return;}if self.navigation_edges.is_none(){self.prepare_navigation();}
        let edges=self.navigation_edges.as_ref().unwrap();let mut labels=vec![u32::MAX;edges.len()];let mut queue=Vec::new();
        for cell in 0..edges.len(){
            if labels[cell]!=u32::MAX||!self.cell_is_open((cell as u32%self.width)as i64,(cell as u32/self.width)as i64){continue;}
            queue.clear();queue.push(cell as u32);labels[cell]=cell as u32;let mut read=0;
            while read<queue.len(){let from=queue[read];read+=1;
                for(direction,&(dx,dy,_))in NAV_STEPS.iter().enumerate(){if edges[from as usize]&(1<<direction)==0{continue;}
                    let next=(i64::from(from)+dy*i64::from(self.width)+dx)as usize;
                    if labels[next]==u32::MAX{labels[next]=cell as u32;queue.push(next as u32);}
                }
            }
        }self.target_components=Some(labels);
    }

    /// Preserve a legal exact target. For a blocked target, select the closest
    /// reachable centre inside an application-chosen cell-radius (maximum32).
    /// Equal distances use row-major order. Requires prepare_target_selection
    /// for blocked targets; it does not run an unbounded search during input.
    pub fn select_nearby_target(&self,start:GridPosition,target:GridPosition,radius:u8)->Option<GridPosition>{
        if !self.is_walkable(start)||radius>32{return None;}if self.is_walkable(target){return Some(target);}
        let labels=self.target_components.as_ref()?;let source=self.nav_cell(start)?;
        if !self.segment_is_clear(start,self.nav_center(source)){return None;}
        let label=labels[source as usize];if label==u32::MAX{return None;}
        let size=i64::from(self.cell_size);let ox=i64::from(target.x)-i64::from(self.origin.x);let oy=i64::from(target.y)-i64::from(self.origin.y);
        let(gx,gy)=if let Some(row)=self.row_height{let gy=(oy+i64::from(row/2)).div_euclid(i64::from(row));((ox-gy.rem_euclid(2)*(size/2)+size/2).div_euclid(size),gy)}else{(ox.div_euclid(size),oy.div_euclid(size))};
        let r=i64::from(radius);let mut best:Option<(u128,u32,GridPosition)>=None;
        for y in (gy-r).max(0)..=(gy+r).min(i64::from(self.height)-1){for x in (gx-r).max(0)..=(gx+r).min(i64::from(self.width)-1){
            let cell=(y*i64::from(self.width)+x)as u32;if labels[cell as usize]!=label{continue;}
            let point=self.nav_center(cell);let dx=i128::from(point.x)-i128::from(target.x);let dy=i128::from(point.y)-i128::from(target.y);let distance=(dx*dx+dy*dy)as u128;
            if best.is_none_or(|b|(distance,cell)<(b.0,b.1)){best=Some((distance,cell,point));}
        }}best.map(|b|b.2)
    }

    pub(crate) fn nav_cell(&self, point: GridPosition) -> Option<u32> {
        if self.connected_boundaries { return self.connected_cell(point); }
        if let Some(row_height) = self.row_height {
            let y = (i64::from(point.y) - i64::from(self.origin.y) + i64::from(row_height / 2))
                .div_euclid(i64::from(row_height));
            let x = (i64::from(point.x) - i64::from(self.origin.x)
                - (y.rem_euclid(2) * i64::from(self.cell_size / 2)) + i64::from(self.cell_size / 2))
                .div_euclid(i64::from(self.cell_size));
            return (x >= 0 && y >= 0 && x < i64::from(self.width) && y < i64::from(self.height))
                .then_some((y * i64::from(self.width) + x) as u32);
        }
        let (x, y) = self.local(point)?;
        let size = i64::from(self.cell_size);
        Some((y / size) as u32 * self.width + (x / size) as u32)
    }

    pub(crate) fn nav_center(&self, cell: u32) -> GridPosition {
        let size = i64::from(self.cell_size);
        if let Some(row_height) = self.row_height {
            let row = cell / self.width;
            return GridPosition {
                x: (i64::from(self.origin.x) + i64::from(cell % self.width) * size + i64::from(row % 2) * (size / 2)) as i32,
                y: (i64::from(self.origin.y) + i64::from(row) * i64::from(row_height)) as i32,
            };
        }
        GridPosition {
            x: (i64::from(self.origin.x) + i64::from(cell % self.width) * size + size / 2) as i32,
            y: (i64::from(self.origin.y) + i64::from(cell / self.width) * size + size / 2) as i32,
        }
    }

    fn local(&self, point: GridPosition) -> Option<(i64, i64)> {
        let x = i64::from(point.x) - i64::from(self.origin.x);
        let y = i64::from(point.y) - i64::from(self.origin.y);
        let size = i64::from(self.cell_size);
        if x < 0 || y < 0 || x / size >= i64::from(self.width) || y / size >= i64::from(self.height) {
            None
        } else {
            Some((x, y))
        }
    }

    fn cell_is_open(&self, x: i64, y: i64) -> bool {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return false;
        }
        let index = y as usize * self.width as usize + x as usize;
        self.blocked[index >> 3] & (1u8 << (index & 7)) == 0
    }

    fn touch_is_open(&self, x: i64, y: i64, vertical: bool, horizontal: bool) -> bool {
        self.cell_is_open(x, y)
            && (!vertical || x == 0 || self.cell_is_open(x - 1, y))
            && (!horizontal || y == 0 || self.cell_is_open(x, y - 1))
            && (!vertical || !horizontal || x == 0 || y == 0 || self.cell_is_open(x - 1, y - 1))
    }
}
