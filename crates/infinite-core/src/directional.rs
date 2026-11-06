//! Integer directional displacement and bounded local wall sliding.
use crate::GridPosition;
pub const DIRECTION_SCALE:i64=32767;
pub const MAX_DIRECTION_MICROS:u32=200_000;
const DENOMINATOR:i64=DIRECTION_SCALE*1_000_000;
const QUERIES:u16=256;
const SKIN:i64=4; // 1/4 pixel in 1/16 pixel units.
const PROBE:i64=128;
const ROTATIONS:[(i64,i64);15]=[(32609,3212),(32137,6393),(31356,9512),(30273,12539),(28898,15446),(27245,18204),(25329,20787),(23170,23170),(20787,25329),(18204,27245),(15446,28898),(12539,30273),(9512,31356),(6393,32137),(3212,32609)];
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct DirectionRemainder {x:i64,y:i64}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum DirectionError {InvalidDirection,InvalidDuration,CoordinateOverflow,DeltaTooLarge}
impl DirectionRemainder {
    /// Exact fractional carry for an authority/replica checkpoint. This is not
    /// an input command and never authorizes a client to change server carry.
    pub fn parts(self)->(i64,i64){(self.x,self.y)}
    pub fn from_parts(x:i64,y:i64)->Result<Self,DirectionError>{
        if x<=-DENOMINATOR||x>=DENOMINATOR||y<=-DENOMINATOR||y>=DENOMINATOR{return Err(DirectionError::InvalidDirection);}
        Ok(Self{x,y})
    }
    /// Trusted server elapsed time only. The application owns lease/cast gates.
    /// Fractional units are carried; rejected input does not change the carry.
    pub fn displacement(&mut self,x:i32,y:i32,speed:u16,micros:u32)->Result<GridPosition,DirectionError>{
        if i64::from(x).abs()>DIRECTION_SCALE||i64::from(y).abs()>DIRECTION_SCALE||i64::from(x)*i64::from(x)+i64::from(y)*i64::from(y)>DIRECTION_SCALE*DIRECTION_SCALE{return Err(DirectionError::InvalidDirection)}
        if micros>MAX_DIRECTION_MICROS{return Err(DirectionError::InvalidDuration)}
        let px=i64::from(x)*i64::from(speed)*i64::from(micros)+self.x;
        let py=i64::from(y)*i64::from(speed)*i64::from(micros)+self.y;
        let delta=GridPosition{x:(px/DENOMINATOR)as i32,y:(py/DENOMINATOR)as i32};
        self.x=px%DENOMINATOR;self.y=py%DENOMINATOR;Ok(delta)
    }
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct IntegerSlide {pub position:GridPosition,pub segments:[Option<GridPosition>;2],pub queries:u16}
fn norm(x:i64,y:i64)->i64{((i128::from(x)*i128::from(x)+i128::from(y)*i128::from(y))as u128).isqrt()as i64}
fn ceil_norm(x:i64,y:i64)->i64{let n=norm(x,y);n+i64::from(i128::from(n)*i128::from(n)<i128::from(x)*i128::from(x)+i128::from(y)*i128::from(y))}
fn offset(p:GridPosition,x:i64,y:i64)->Option<GridPosition>{Some(GridPosition{x:i32::try_from(i64::from(p.x)+x).ok()?,y:i32::try_from(i64::from(p.y)+y).ok()?})}
struct Oracle<F>{allowed:F,queries:u16}
impl<F:FnMut(GridPosition,GridPosition)->bool> Oracle<F>{
    fn accepts(&mut self,a:GridPosition,b:Option<GridPosition>)->bool{
        let Some(b)=b else{return false};if self.queries>=QUERIES{return false}self.queries+=1;(self.allowed)(a,b)
    }
    fn clip(&mut self,a:GridPosition,b:GridPosition)->GridPosition{
        let dx=i64::from(b.x)-i64::from(a.x);let dy=i64::from(b.y)-i64::from(a.y);let length=ceil_norm(dx,dy);
        if length==0{return a}
        let(mut low,mut high)=(0i64,65536i64);
        for _ in 0..16{if self.queries>=QUERIES{break}let middle=(low+high)/2;
            if self.accepts(a,offset(a,dx*middle/65536,dy*middle/65536)){low=middle}else{high=middle}}
        let ratio=(low-(SKIN*65536+length-1)/length).max(0);
        let point=offset(a,dx*ratio/65536,dy*ratio/65536);
        if ratio>0&&self.accepts(a,point){point.unwrap()}else{a}
    }
    fn candidate(&mut self,base:GridPosition,remaining:(i64,i64),direction:(i64,i64),original:(i64,i64),budget:i64)->Option<(GridPosition,i128)>{
        let (mut ux,mut uy)=direction;let length=ceil_norm(ux,uy);if length==0{return None}
        ux=ux*DIRECTION_SCALE/length;uy=uy*DIRECTION_SCALE/length;
        let projection=((remaining.0*ux+remaining.1*uy)/DIRECTION_SCALE).min(budget);
        if projection<=0||!self.accepts(base,offset(base,ux*PROBE/DIRECTION_SCALE,uy*PROBE/DIRECTION_SCALE)){return None}
        let desired=offset(base,ux*projection/DIRECTION_SCALE,uy*projection/DIRECTION_SCALE)?;
        let point=if self.accepts(base,Some(desired)){desired}else{self.clip(base,desired)};
        let(dx,dy)=(i64::from(point.x)-i64::from(base.x),i64::from(point.y)-i64::from(base.y));
        let score=i128::from(dx)*i128::from(original.0)+i128::from(dy)*i128::from(original.1);
        (score>0&&ceil_norm(dx,dy)<=budget).then_some((point,score))
    }
}
fn better(old:Option<(GridPosition,i128)>,new:Option<(GridPosition,i128)>)->Option<(GridPosition,i128)>{
    match(old,new){(Some(a),Some(b))=>Some(if b.1>a.1{b}else{a}),(a,None)=>a,(None,b)=>b}
}
/// Every published segment passes the same collision oracle as navigation.
/// Tangent motion is a projection of remaining input, never full-speed rerouting.
/// Ambiguous equal left/right progress stops at contact. Work is bounded.
pub fn slide_integer<F:FnMut(GridPosition,GridPosition)->bool>(from:GridPosition,delta:GridPosition,allowed:F)->Result<IntegerSlide,DirectionError>{
    let(dx,dy)=(i64::from(delta.x),i64::from(delta.y));
    if dx.abs()>65536||dy.abs()>65536{return Err(DirectionError::DeltaTooLarge)}
    let target=offset(from,dx,dy).ok_or(DirectionError::CoordinateOverflow)?;
    let mut oracle=Oracle{allowed,queries:0};let mut segments=[None,None];
    if (dx==0&&dy==0)||!oracle.accepts(from,Some(from)){return Ok(IntegerSlide{position:from,segments,queries:oracle.queries})}
    if oracle.accepts(from,Some(target)){return Ok(IntegerSlide{position:target,segments:[Some(target),None],queries:oracle.queries})}
    let base=oracle.clip(from,target);if base!=from{segments[0]=Some(base)}
    let remaining=(i64::from(target.x)-i64::from(base.x),i64::from(target.y)-i64::from(base.y));
    let budget=(norm(dx,dy)-ceil_norm(i64::from(base.x)-i64::from(from.x),i64::from(base.y)-i64::from(from.y))).max(0);
    let mut slide=None;
    for d in [(remaining.0.signum()*DIRECTION_SCALE,0),(0,remaining.1.signum()*DIRECTION_SCALE)]{
        if d==(0,0){continue}if oracle.accepts(base,offset(base,-d.0*PROBE/DIRECTION_SCALE,-d.1*PROBE/DIRECTION_SCALE)){
            slide=better(slide,oracle.candidate(base,remaining,d,(dx,dy),budget));
        }
    }
    if slide.is_none(){
        let(mut left,mut right)=(None,None);
        for (cos,sin) in ROTATIONS {
            if oracle.queries>=QUERIES{break}
            left=better(left,oracle.candidate(base,remaining,(dx*cos-dy*sin,dx*sin+dy*cos),(dx,dy),budget));
            right=better(right,oracle.candidate(base,remaining,(dx*cos+dy*sin,-dx*sin+dy*cos),(dx,dy),budget));
        }
        slide=if matches!((left,right),(Some(a),Some(b))if a.1==b.1){None}else{better(left,right)};
    }
    if let Some((point,_))=slide{segments[usize::from(segments[0].is_some())]=Some(point)}
    Ok(IntegerSlide{position:segments[1].or(segments[0]).unwrap_or(from),segments,queries:oracle.queries})
}
