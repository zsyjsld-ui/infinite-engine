//! Exact integer capsule ranking, without square roots or floating point.
use crate::{GridPosition,ViewRect,CombatQueryError,CombatQueryHit};
/// Bound each axis of one sweep to keep every squared/cross product below
/// 2^106 in u128, even at opposite i32 positions and with a u32 radius.
pub const MAX_SEGMENT_AXIS:u32=1_048_576;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SegmentSweep {pub from:GridPosition,pub to:GridPosition,pub radius:u32,pub strict_ends:bool}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CombatSegmentQuery {pub sweep:SegmentSweep,pub exclude:Option<crate::EntityId>,pub sight_origin:Option<GridPosition>}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CombatSegmentHit {
 pub hit:CombatQueryHit,
 /// Clamped dot product along this sweep (common denominator length squared).
 pub along:u64,
 /// Squared perpendicular distance times length squared, or ordinary squared
 /// distance for a zero-length sweep. Only compare hits from the SAME sweep.
 pub distance_scaled:u128,
}
pub(crate) struct SegmentGeometry {sweep:SegmentSweep,dx:i128,dy:i128,len:u128}
impl SegmentGeometry {
 pub(crate) fn new(sweep:SegmentSweep)->Result<Self,CombatQueryError>{let dx=i128::from(sweep.to.x)-i128::from(sweep.from.x);let dy=i128::from(sweep.to.y)-i128::from(sweep.from.y);if dx.unsigned_abs()>u128::from(MAX_SEGMENT_AXIS)||dy.unsigned_abs()>u128::from(MAX_SEGMENT_AXIS){return Err(CombatQueryError::InvalidSegment);}Ok(Self{sweep,dx,dy,len:(dx*dx+dy*dy)as u128})}
 pub(crate) fn bounds(&self)->ViewRect{let s=self.sweep;let r=i64::from(s.radius);let clamp=|v:i64|v.clamp(i32::MIN as i64,i32::MAX as i64)as i32;ViewRect{min:GridPosition{x:clamp(i64::from(s.from.x.min(s.to.x))-r),y:clamp(i64::from(s.from.y.min(s.to.y))-r)},max:GridPosition{x:clamp(i64::from(s.from.x.max(s.to.x))+r),y:clamp(i64::from(s.from.y.max(s.to.y))+r)}}}
 pub(crate) fn rank(&self,position:GridPosition)->Option<(u64,u128)>{let x=i128::from(position.x)-i128::from(self.sweep.from.x);let y=i128::from(position.y)-i128::from(self.sweep.from.y);let dot=x*self.dx+y*self.dy;if self.sweep.strict_ends&&(dot<0||dot>self.len as i128){return None;}
  let radius=u128::from(self.sweep.radius).pow(2);if self.len==0{let distance=(x*x+y*y)as u128;return(distance<=radius).then_some((0,distance));}
  let along=dot.clamp(0,self.len as i128)as u64;let distance=if dot<=0{(x*x+y*y)as u128*self.len}else if dot>=self.len as i128{let x=x-self.dx;let y=y-self.dy;(x*x+y*y)as u128*self.len}else{let cross=x*self.dy-y*self.dx;cross.unsigned_abs().pow(2)};(distance<=radius*self.len).then_some((along,distance))
 }
}
