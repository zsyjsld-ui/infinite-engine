//! Optional union-of-open-cells boundary policy for staggered occupancy.
//! Exact rational clipping; touching a solid cell is allowed only through a
//! connected open side. Zero-width diagonal connections never create a passage.
use super::CollisionMap;
use crate::GridPosition;
use std::collections::{HashSet, VecDeque};

#[derive(Clone, Copy, Debug)]
struct Fraction { n:i64, d:i64 }
impl PartialEq for Fraction {fn eq(&self,other:&Self)->bool{self.cmp(other)==std::cmp::Ordering::Equal}}
impl Eq for Fraction {}
impl Ord for Fraction {
    fn cmp(&self,other:&Self)->std::cmp::Ordering {
        (i128::from(self.n)*i128::from(other.d)).cmp(&(i128::from(other.n)*i128::from(self.d)))
    }
}
impl PartialOrd for Fraction { fn partial_cmp(&self,other:&Self)->Option<std::cmp::Ordering>{Some(self.cmp(other))} }
const ZERO:Fraction=Fraction{n:0,d:1};
const ONE:Fraction=Fraction{n:1,d:1};
#[derive(Clone,Copy)]
struct Cell { index:u32, lo:Fraction, hi:Fraction }

impl CollisionMap {
    pub(super) fn raw_staggered_cell(&self,p:GridPosition)->Option<u32> {
        let rh=i64::from(self.row_height?);let cw=i64::from(self.cell_size);
        let y=(i64::from(p.y)-i64::from(self.origin.y)+rh/2).div_euclid(rh);
        let x=(i64::from(p.x)-i64::from(self.origin.x)-y.rem_euclid(2)*cw/2+cw/2).div_euclid(cw);
        (x>=0&&y>=0&&x<i64::from(self.width)&&y<i64::from(self.height)).then_some((y*i64::from(self.width)+x)as u32)
    }
    fn connected_interval(&self,index:u32,a:GridPosition,b:GridPosition)->Option<Cell> {
        let x=index%self.width;let y=index/self.width;
        if !self.cell_is_open(i64::from(x),i64::from(y)){return None}
        let centre=self.nav_center(index);
        let mut lo=ZERO;let mut hi=ONE;
        for (start,end,center,half) in [
            (a.x,b.x,centre.x,self.cell_size/2),
            (a.y,b.y,centre.y,self.row_height.unwrap()/2),
        ] {
            let s=i64::from(start);let delta=i64::from(end)-s;
            let low=i64::from(center)-i64::from(half);let high=i64::from(center)+i64::from(half);
            if delta==0 {if s<low||s>high{return None}continue}
            let (l,h)=if delta>0{(low-s,high-s)}else{(s-high,s-low)};
            let d=delta.abs();lo=lo.max(Fraction{n:l,d});hi=hi.min(Fraction{n:h,d});
            if lo>hi{return None}
        }
        Some(Cell{index,lo,hi})
    }
    fn neighbours(&self,index:u32)->impl Iterator<Item=u32>+'_ {
        let x=i64::from(index%self.width);let y=i64::from(index/self.width);
        (-1..=1).flat_map(move|dy|(-1..=1).filter_map(move|dx|{
            let nx=x+dx;let ny=y+dy;
            (nx>=0&&ny>=0&&nx<i64::from(self.width)&&ny<i64::from(self.height))
                .then_some((ny*i64::from(self.width)+nx)as u32)
        }))
    }
    pub(super) fn connected_cell(&self,p:GridPosition)->Option<u32> {
        let home=self.raw_staggered_cell(p)?;
        self.neighbours(home).filter(|&i|self.connected_interval(i,p,p).is_some())
            .min_by_key(|&i|{let c=self.nav_center(i);let dx=i128::from(c.x)-i128::from(p.x);let dy=i128::from(c.y)-i128::from(p.y);(dx*dx+dy*dy,i)})
    }
}

pub(super) struct ConnectedScan {
    pub result:Option<bool>,
    from:GridPosition,to:GridPosition,
    queue:VecDeque<Cell>,seen:HashSet<u32>,
}
impl ConnectedScan {
    pub fn new(map:&CollisionMap,from:GridPosition,to:GridPosition)->Self {
        let mut out=Self{result:None,from,to,queue:VecDeque::new(),seen:HashSet::new()};
        let Some(home)=map.raw_staggered_cell(from)else{out.result=Some(false);return out};
        if map.raw_staggered_cell(to).is_none(){out.result=Some(false);return out}
        // Common short steps never allocate: a single open convex cell covers both ends.
        for index in map.neighbours(home) {
            if map.connected_interval(index,from,to).is_some_and(|c|c.lo==ZERO&&c.hi==ONE){out.result=Some(true);return out}
        }
        for index in map.neighbours(home) {
            if let Some(cell)=map.connected_interval(index,from,to).filter(|c|c.lo==ZERO){out.queue.push_back(cell);out.seen.insert(index);}
        }
        if out.queue.is_empty(){out.result=Some(false)}
        out
    }
    /// One charged visit examines at most nine neighbours, regardless of map size.
    pub fn advance(&mut self,map:&CollisionMap) {
        if self.result.is_some(){return}
        let Some(cell)=self.queue.pop_front()else{self.result=Some(false);return};
        let a=map.nav_center(cell.index);
        for index in map.neighbours(cell.index) {
            if self.seen.contains(&index){continue}
            let b=map.nav_center(index);
            let w=i64::from(map.cell_size)-(i64::from(a.x)-i64::from(b.x)).abs();
            let h=i64::from(map.row_height.unwrap())-(i64::from(a.y)-i64::from(b.y)).abs();
            if w<0||h<0||w.max(h)==0{continue}
            let Some(next)=map.connected_interval(index,self.from,self.to)else{continue};
            if cell.lo.max(next.lo)>cell.hi.min(next.hi){continue}
            if next.hi==ONE {self.result=Some(true);return}
            self.seen.insert(index);self.queue.push_back(next);
        }
        if self.queue.is_empty(){self.result=Some(false)}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;
    #[test]
    fn neighbour_stencil_matches_rational_oracle_for_all_local_patterns(){
        let n=|v|NonZeroU32::new(v).unwrap();
        for row in [2,3]{for mask in 0u16..512{
            let mut map=CollisionMap::new_staggered_connected(GridPosition{x:-192,y:-48},n(6),n(6),n(48),n(12)).unwrap();
            for y in 0..3{for x in 0..3{map.set_blocked(x+1,y+row-1,mask&(1<<(y*3+x))!=0).unwrap();}}
            let source=row*6+2;
            for next in map.neighbours(source){
                assert_eq!(map.nav_edge_is_clear_uncached(source,next),map.segment_is_clear(map.nav_center(source),map.nav_center(next)),"row={row},mask={mask},next={next}");
            }
        }}
    }
}
