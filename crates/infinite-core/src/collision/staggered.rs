use super::{CollisionMap, MapError, MAX_COLLISION_CELLS};
use crate::GridPosition;
use std::num::NonZeroU32;

impl CollisionMap {
    /// Origin is the centre of cell (0,0). Odd rows shift right by half a column.
    /// Columns/rows must have even sizes in position units so all boundaries
    /// remain integral. Closed boundary contact with a blocked/outside cell fails.
    pub fn new_staggered(
        origin: GridPosition, width: NonZeroU32, height: NonZeroU32,
        column_width: NonZeroU32, row_height: NonZeroU32,
    ) -> Result<Self, MapError> {
        let (width, height, cell_size, row_height) = (width.get(),height.get(),column_width.get(),row_height.get());
        if cell_size % 2 != 0 || row_height % 2 != 0 { return Err(MapError::InvalidCellGeometry); }
        let cells = u64::from(width) * u64::from(height);
        if cells > MAX_COLLISION_CELLS { return Err(MapError::TooManyCells); }
        let left = i64::from(origin.x) - i64::from(cell_size / 2);
        let top = i64::from(origin.y) - i64::from(row_height / 2);
        let right = i128::from(origin.x) + i128::from(width - 1) * i128::from(cell_size)
            + i128::from(cell_size / 2) * if height > 1 { 2 } else { 1 };
        let bottom = i128::from(origin.y) + i128::from(height - 1) * i128::from(row_height) + i128::from(row_height / 2);
        if left < i64::from(i32::MIN) || top < i64::from(i32::MIN)
            || right > i128::from(i32::MAX) || bottom > i128::from(i32::MAX) {
            return Err(MapError::OutsideCoordinateRange);
        }
        let rounded_length = |dx: u128| {
            let squared = dx * dx + u128::from(row_height) * u128::from(row_height);
            let floor = squared.isqrt();
            (floor + u128::from(floor * floor != squared)) as u64
        };
        let edge_costs = [u64::from(cell_size), rounded_length(u128::from(cell_size / 2)), rounded_length(u128::from(cell_size / 2) * 3)];
        Ok(Self { origin,width,height,cell_size,row_height:Some(row_height),connected_boundaries:false,blocked:vec![0;((cells+7)/8) as usize], edge_costs, navigation_edges:None,target_components:None })
    }

    /// Open cells may share a boundary with solid cells. Crossing still needs
    /// an open path with a positive-width shared edge; diagonal cracks are solid.
    pub fn new_staggered_connected(origin:GridPosition,width:NonZeroU32,height:NonZeroU32,column_width:NonZeroU32,row_height:NonZeroU32)->Result<Self,MapError> {
        let mut map=Self::new_staggered(origin,width,height,column_width,row_height)?;
        map.connected_boundaries=true;Ok(map)
    }

    pub(crate) fn nav_estimate(&self, from: u32, to: u32) -> u64 {
        if self.row_height.is_some() {
            self.center_distance_squared(from,to).isqrt() as u64
        } else {
            let dx = u64::from((from % self.width).abs_diff(to % self.width));
            let dy = u64::from((from / self.width).abs_diff(to / self.width));
            14 * dx.min(dy) + 10 * dx.abs_diff(dy)
        }
    }
    pub(crate) fn nav_edge_cost(&self, from:u32, to:u32, square_cost:u64) -> u64 {
        if self.row_height.is_none() { return square_cost; }
        let (a,b) = (self.nav_center(from),self.nav_center(to));
        if a.y == b.y { self.edge_costs[0] }
        else if a.x.abs_diff(b.x) == self.cell_size / 2 { self.edge_costs[1] }
        else { self.edge_costs[2] }
    }
    fn center_distance_squared(&self, from:u32, to:u32) -> u128 {
        let (a,b) = (self.nav_center(from),self.nav_center(to));
        let dx = i128::from(a.x)-i128::from(b.x);
        let dy = i128::from(a.y)-i128::from(b.y);
        (dx*dx+dy*dy) as u128
    }
    pub(crate) fn nav_edge_is_clear(&self, from:u32, to:u32, direction:usize) -> bool {
        if let Some(edges) = &self.navigation_edges { return edges[from as usize] & (1 << direction) != 0; }
        self.nav_edge_is_clear_uncached(from, to)
    }
    pub(super) fn nav_edge_is_clear_uncached(&self, from:u32, to:u32) -> bool {
        if self.connected_boundaries {
            let (x,y,nx,ny)=(from%self.width,from/self.width,to%self.width,to/self.width);
            if !self.cell_is_open(i64::from(x),i64::from(y))||!self.cell_is_open(i64::from(nx),i64::from(ny)){return false}
            // Centre-neighbour edges have only three shapes. Horizontal and
            // short diagonal edges cross one positive-width shared side. A long
            // diagonal crosses the two intermediate cells at 1/3 and 2/3.
            // This stencil is exact only for direct index neighbours; arbitrary
            // connectors and movement retain the rational union scan.
            if x.abs_diff(nx)<=1&&y.abs_diff(ny)<=1 {
                let (a,b)=(self.nav_center(from),self.nav_center(to));
                if y==ny||a.x.abs_diff(b.x)==self.cell_size/2{return true}
                return self.cell_is_open(i64::from(nx),i64::from(y))&&self.cell_is_open(i64::from(x),i64::from(ny));
            }
        }
        if self.row_height.is_some() && !self.connected_boundaries {
            let (x,y,nx,ny) = (from%self.width,from/self.width,to%self.width,to/self.width);
            if x!=nx && y!=ny && (!self.cell_is_open(i64::from(nx),i64::from(y))
                || !self.cell_is_open(i64::from(x),i64::from(ny))) { return false; }
        }
        self.segment_is_clear(self.nav_center(from),self.nav_center(to))
    }

    // A point touches at most two rows and two columns per row. This is the
    // closed-segment rule specialized to zero length; all arithmetic fits i64.
    pub(super) fn staggered_point_is_open(&self, point: GridPosition) -> bool {
        let (x,y) = (i64::from(point.x)-i64::from(self.origin.x), i64::from(point.y)-i64::from(self.origin.y));
        let rh = i64::from(self.row_height.unwrap());
        let cw = i64::from(self.cell_size);
        let first_row = -(-(y-rh/2)).div_euclid(rh);
        let last_row = (y+rh/2).div_euclid(rh);
        for row in first_row..=last_row {
            let offset = row.rem_euclid(2) * (cw/2);
            let first = -(-(x-offset-cw/2)).div_euclid(cw);
            let last = (x-offset+cw/2).div_euclid(cw);
            for column in first..=last { if !self.cell_is_open(column,row) { return false; } }
        }
        true
    }
}

/// Exact row-strip clipping with rational X coordinates (i128 intermediates).
/// Each advance visits ONE occupancy cell, plus at most two corner guards.
/// No row-wide loops hidden inside a navigation work unit; scan is resumable.
pub(super) struct StaggeredScan {
    pub result: Option<bool>,
    sx:i128, sy:i128, dx:i128, dy:i128, den:i128,
    row:i64, last_row:i64, column:i64, last_column:i64,
}
fn ceil_div(n:i128,d:i128)->i128 { -(-n).div_euclid(d) }
impl StaggeredScan {
    pub fn new(map:&CollisionMap, from:GridPosition, to:GridPosition)->Self {
        let (sx,sy)=(i128::from(from.x)-i128::from(map.origin.x),i128::from(from.y)-i128::from(map.origin.y));
        let (ex,ey)=(i128::from(to.x)-i128::from(map.origin.x),i128::from(to.y)-i128::from(map.origin.y));
        let rh=i128::from(map.row_height.unwrap());
        let row=ceil_div(sy.min(ey)-rh/2,rh) as i64;
        let last_row=(sy.max(ey)+rh/2).div_euclid(rh) as i64;
        let mut scan=Self {result:None,sx,sy,dx:ex-sx,dy:ey-sy,den:(ey-sy).abs().max(1),row,last_row,column:0,last_column:0};
        if row<0 || last_row>=i64::from(map.height) { scan.result=Some(false); }
        else { scan.prepare_row(map); }
        scan
    }
    fn prepare_row(&mut self,map:&CollisionMap) {
        let rh=i128::from(map.row_height.unwrap());
        let low=i128::from(self.row)*rh-rh/2;
        let high=low+rh;
        let (x0,x1)=if self.dy==0 { (self.sx,self.sx+self.dx) } else {
            let a=(low-self.sy)*self.dy.signum();
            let b=(high-self.sy)*self.dy.signum();
            let lo=a.min(b).max(0); let hi=a.max(b).min(self.den);
            (self.sx*self.den+self.dx*lo,self.sx*self.den+self.dx*hi)
        };
        let cw=i128::from(map.cell_size);
        let shift=i128::from(self.row.rem_euclid(2))*(cw/2)*self.den;
        self.column=ceil_div(x0.min(x1)-shift-(cw/2)*self.den,cw*self.den) as i64;
        self.last_column=(x0.max(x1)-shift+(cw/2)*self.den).div_euclid(cw*self.den) as i64;
        if self.column<0 || self.last_column>=i64::from(map.width) { self.result=Some(false); }
    }
    pub fn advance(&mut self,map:&CollisionMap) {
        if self.result.is_some() { return; }
        if !map.cell_is_open(self.column,self.row) { self.result=Some(false);return; }
        if self.column<self.last_column { self.column+=1;return; }
        if self.row==self.last_row { self.result=Some(true);return; }
        if self.dy!=0 {
            let rh=i128::from(map.row_height.unwrap());
            let t=(i128::from(self.row)*rh+rh/2-self.sy)*self.dy.signum();
            if (0..=self.den).contains(&t) {
                let x=self.sx*self.den+self.dx*t;
                let cw=i128::from(map.cell_size);let half=cw/2*self.den;
                let above=(x-i128::from(self.row.rem_euclid(2))*half+half).div_euclid(cw*self.den) as i64;
                let below=(x-i128::from((self.row+1).rem_euclid(2))*half+half).div_euclid(cw*self.den) as i64;
                if above!=below && (!map.cell_is_open(above,self.row+1)||!map.cell_is_open(below,self.row)) {
                    self.result=Some(false);return;
                }
            }
        }
        self.row+=1;self.prepare_row(map);
    }
}
