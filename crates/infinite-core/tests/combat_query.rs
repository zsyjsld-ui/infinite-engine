use infinite_core::*;
use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn p(x:i32,y:i32)->GridPosition{GridPosition{x,y}}
fn circle(center:GridPosition,radius:u32)->CombatQuery{CombatQuery{area:CombatArea::Circle{center,radius},exclude:None,sight_origin:None}}
fn rect(a:GridPosition,b:GridPosition)->CombatQuery{CombatQuery{area:CombatArea::Rect(ViewRect{min:a,max:b}),exclude:None,sight_origin:None}}
fn hit()->CombatQueryHit{CombatQueryHit{entity:EntityId{index:0,generation:0},position:p(0,0)}}
fn oracle(q:CombatQuery,point:GridPosition)->bool{match q.area{CombatArea::Rect(r)=>point.x>=r.min.x&&point.x<=r.max.x&&point.y>=r.min.y&&point.y<=r.max.y,CombatArea::Circle{center,radius}=>{let dx=i128::from(point.x)-i128::from(center.x);let dy=i128::from(point.y)-i128::from(center.y);dx*dx+dy*dy<=i128::from(radius)*i128::from(radius)}}}

#[test]
fn ranges_and_nearest_match_independent_reference_after_motion_and_reuse(){
 let mut w=World::default();let mut ids:Vec<_>=(0..120).map(|i|w.spawn(p((i%12)*17-99,(i/12)*19-91)).unwrap()).collect();let mut f=SpatialFrame::new(n(32));let mut out=Vec::new();
 for tick in 0..20{
  if tick>0{w.despawn(ids[7]).unwrap();ids[7]=w.spawn(p(tick*7-50,11)).unwrap();w.set_position(ids[33],p(tick*9-80,-3)).unwrap();}
  w.advance_tick().unwrap();f.capture(&w).unwrap();let ctx=CombatQueryContext::new(&f,None);
  for q in [circle(p(0,0),0),circle(p(-13,15),70),circle(p(3,-8),130),rect(p(-40,-50),p(71,37))]{
   let q=CombatQuery{exclude:Some(ids[0]),..q};let mut expected:Vec<_>=ids.iter().filter_map(|&entity|{let position=w.position(entity).unwrap();(Some(entity)!=q.exclude&&entity.index%3!=0&&oracle(q,position)).then_some(CombatQueryHit{entity,position})}).collect();expected.sort_by_key(|h|(h.entity.index,h.entity.generation));
   let mut budget=CombatQueryBudget::new(100000);let r=ctx.collect(q,&mut budget,&mut out,|h|h.entity.index%3!=0).unwrap();assert_eq!(out,expected);assert_eq!((r.tick,r.offset,r.count),(w.tick(),0,expected.len()));
   let origin=p(-20,22);let nearest=ctx.nearest(CombatNearestQuery{query:q,origin},&mut budget,|h|h.entity.index%3!=0).unwrap();
   expected.sort_by_key(|h|{let dx=i128::from(h.position.x)-i128::from(origin.x);let dy=i128::from(h.position.y)-i128::from(origin.y);(dx*dx+dy*dy,h.entity.index,h.entity.generation)});assert_eq!(nearest.target,expected.first().copied());
  }
 }
}
#[test]
fn zero_radius_boundaries_ties_and_extreme_integer_distances(){
 let mut w=World::default();let a=w.spawn(p(-3,4)).unwrap();let b=w.spawn(p(3,4)).unwrap();let edge=w.spawn(p(0,5)).unwrap();w.spawn(p(1,5)).unwrap();let mut f=SpatialFrame::new(n(32));f.capture(&w).unwrap();let ctx=CombatQueryContext::new(&f,None);let mut out=Vec::new();let mut budget=CombatQueryBudget::new(100);
 ctx.collect(circle(p(0,0),5),&mut budget,&mut out,|_|true).unwrap();assert_eq!(out.iter().map(|h|h.entity).collect::<Vec<_>>(),vec![a,b,edge]);assert_eq!(ctx.nearest(CombatNearestQuery{query:circle(p(0,0),5),origin:p(0,0)},&mut budget,|_|true).unwrap().target.unwrap().entity,a);
 ctx.collect(circle(p(-3,4),0),&mut budget,&mut out,|_|true).unwrap();assert_eq!(out.len(),1);
 let mut w=World::default();let far=w.spawn(p(i32::MAX,i32::MAX)).unwrap();let close=w.spawn(p(i32::MAX,i32::MIN)).unwrap();let mut f=SpatialFrame::new(n(u32::MAX));f.capture(&w).unwrap();let ctx=CombatQueryContext::new(&f,None);let mut budget=CombatQueryBudget::new(100);
 let q=rect(p(i32::MIN,i32::MIN),p(i32::MAX,i32::MAX));assert_eq!(ctx.nearest(CombatNearestQuery{query:q,origin:p(i32::MIN,i32::MIN)},&mut budget,|_|true).unwrap().target.unwrap().entity,close);
 ctx.collect(circle(p(i32::MIN,i32::MIN),u32::MAX),&mut budget,&mut out,|_|true).unwrap();assert_eq!(out.len(),1);assert_eq!(out[0].entity,close);assert_ne!(out[0].entity,far);
}
#[test]
fn dense_edge_cells_consume_budget_and_do_not_return_partial_targets(){
 let mut w=World::default();w.spawn(p(0,0)).unwrap();for _ in 0..5000{w.spawn(p(90,90)).unwrap();}let mut f=SpatialFrame::new(n(128));f.capture(&w).unwrap();let ctx=CombatQueryContext::new(&f,None);let q=circle(p(0,0),0);let mut out=vec![hit()];let mut budget=CombatQueryBudget::new(10);
 assert_eq!(ctx.collect(q,&mut budget,&mut out,|_|true),Err(CombatQueryError::WorkLimitExceeded));assert!(out.is_empty());assert_eq!(budget.work(),CombatQueryWork{cells:1,candidates:9,sight_steps:0});
 let mut budget=CombatQueryBudget::new(10);assert_eq!(ctx.nearest(CombatNearestQuery{query:q,origin:p(0,0)},&mut budget,|_|true),Err(CombatQueryError::WorkLimitExceeded));
 ctx.collect(q,&mut CombatQueryBudget::new(6000),&mut out,|_|true).unwrap();assert_eq!(out.len(),1);
}
#[test]
fn hit_limits_nearest_without_hit_cap_and_batch_rollback(){
 let mut w=World::default();for _ in 0..4097{w.spawn(p(0,0)).unwrap();}let mut f=SpatialFrame::new(n(16));f.capture(&w).unwrap();let ctx=CombatQueryContext::new(&f,None);let q=circle(p(0,0),1);let mut out=Vec::new();
 assert_eq!(ctx.collect(q,&mut CombatQueryBudget::new(10000),&mut out,|_|true),Err(CombatQueryError::HitLimitExceeded));assert!(out.is_empty());
 assert_eq!(ctx.nearest(CombatNearestQuery{query:q,origin:p(0,0)},&mut CombatQueryBudget::new(10000),|_|true).unwrap().target.unwrap().entity.index,0);
 let mut results=[Err(CombatQueryError::MapRequired);18];ctx.collect_batch(&[q;18],&mut CombatQueryBudget::new(100000),&mut out,&mut results,|_,h|h.entity.index<4096).unwrap();assert_eq!(out.len(),65536);for (i,r)in results[..16].iter().enumerate(){assert_eq!((r.unwrap().offset,r.unwrap().count),(i*4096,4096));}assert_eq!(results[16],Err(CombatQueryError::HitLimitExceeded));assert_eq!(results[17],Err(CombatQueryError::HitLimitExceeded));
}
#[test]
fn batch_structural_checks_and_per_request_failure_preserve_contract(){
 let mut w=World::default();w.spawn(p(0,0)).unwrap();let mut f=SpatialFrame::new(n(16));f.capture(&w).unwrap();let ctx=CombatQueryContext::new(&f,None);let q=circle(p(0,0),0);let bad=rect(p(1,1),p(0,0));let mut out=vec![hit()];let mut r=[Err(CombatQueryError::MapRequired);4];let mut b=CombatQueryBudget::new(4);
 assert_eq!(ctx.collect_batch(&[q;257],&mut b,&mut out,&mut r,|_,_|true),Err(CombatQueryError::TooManyQueries));assert_eq!(ctx.collect_batch(&[q;5],&mut b,&mut out,&mut r,|_,_|true),Err(CombatQueryError::ResultsTooSmall));assert_eq!(out,vec![hit()]);assert_eq!(b.used(),0);assert_eq!(r,[Err(CombatQueryError::MapRequired);4]);
 ctx.collect_batch(&[q,bad,q],&mut b,&mut out,&mut r,|_,_|true).unwrap();assert_eq!(out.len(),2);assert_eq!(r[0].unwrap().offset,0);assert_eq!(r[1],Err(CombatQueryError::Spatial(SyncError::InvalidView)));assert_eq!(r[2].unwrap().offset,1);assert_eq!(r[3],Err(CombatQueryError::MapRequired));assert_eq!(b.remaining(),0);
 let mut r=[Err(CombatQueryError::MapRequired);2];ctx.nearest_batch(&[CombatNearestQuery{query:q,origin:p(0,0)};2],&mut CombatQueryBudget::new(2),&mut r,|_,_|true).unwrap();assert!(r[0].unwrap().target.is_some());assert_eq!(r[1],Err(CombatQueryError::WorkLimitExceeded));
}
#[test]
fn missing_frame_map_large_view_and_empty_batches_are_explicit(){
 let mut f=SpatialFrame::new(n(1));let mut out=vec![hit()];let q=circle(p(0,0),0);assert_eq!(CombatQueryContext::new(&f,None).collect(q,&mut CombatQueryBudget::new(100),&mut out,|_|true),Err(CombatQueryError::Spatial(SyncError::NoFrame)));assert!(out.is_empty());
 f.capture(&World::default()).unwrap();let ctx=CombatQueryContext::new(&f,None);assert_eq!(ctx.line_of_sight(p(0,0),p(0,0),&mut CombatQueryBudget::new(1)),Err(CombatQueryError::MapRequired));assert_eq!(ctx.collect(CombatQuery{sight_origin:Some(p(0,0)),..q},&mut CombatQueryBudget::new(1),&mut out,|_|true),Err(CombatQueryError::MapRequired));
 assert_eq!(ctx.collect(circle(p(0,0),100),&mut CombatQueryBudget::new(100000),&mut out,|_|true),Err(CombatQueryError::Spatial(SyncError::LimitExceeded)));
 out.push(hit());ctx.collect_batch(&[],&mut CombatQueryBudget::new(0),&mut out,&mut [],|_,_|true).unwrap();assert!(out.is_empty());
}
#[test]
fn square_sight_filters_nearest_and_accounts_for_scan_work(){
 let mut map=CollisionMap::new(p(0,0),n(8),n(8),n(16)).unwrap();map.set_blocked(3,3,true).unwrap();let mut w=World::with_map(DEFAULT_TICK_MS,map);let blocked=w.spawn(p(72,56)).unwrap();let visible=w.spawn(p(24,104)).unwrap();let mut f=SpatialFrame::new(n(32));f.capture(&w).unwrap();let ctx=CombatQueryContext::new(&f,w.collision_map());let q=CombatQuery{sight_origin:Some(p(24,56)),..circle(p(24,56),100)};let mut b=CombatQueryBudget::new(200);let mut out=Vec::new();ctx.collect(q,&mut b,&mut out,|_|true).unwrap();assert_eq!(out.iter().map(|h|h.entity).collect::<Vec<_>>(),vec![visible]);assert_ne!(visible,blocked);assert!(b.work().sight_steps>0);
 assert_eq!(ctx.nearest(CombatNearestQuery{query:q,origin:p(24,56)},&mut b,|_|true).unwrap().target.unwrap().entity,visible);
 assert_eq!(ctx.line_of_sight(p(24,56),p(72,56),&mut CombatQueryBudget::new(1)),Err(CombatQueryError::WorkLimitExceeded));assert_eq!(ctx.line_of_sight(p(-1,0),p(0,0),&mut b),Ok(false));
}
#[test]
fn staggered_sight_keeps_closed_edges_and_requires_budget_even_for_endpoints(){
 let mut map=CollisionMap::new_staggered(p(0,0),n(12),n(12),n(768),n(192)).unwrap();map.set_blocked(3,4,true).unwrap();let frame=SpatialFrame::new(n(4096));let ctx=CombatQueryContext::new(&frame,Some(&map));
 for(point,clear)in [(p(2304,768),false),(p(1920,768),false),(p(1919,768),true),(p(2304,864),false),(p(2304,865),true),(p(-384,0),false),(p(-383,0),true)]{let mut b=CombatQueryBudget::new(100);assert_eq!(ctx.line_of_sight(point,point,&mut b),Ok(clear));assert!(b.used()>=1);assert_eq!(ctx.line_of_sight(point,point,&mut CombatQueryBudget::new(b.used())),Ok(clear));assert_eq!(ctx.line_of_sight(point,point,&mut CombatQueryBudget::new(b.used()-1)),Err(CombatQueryError::WorkLimitExceeded));}
 assert_eq!(ctx.line_of_sight(p(768,768),p(3840,768),&mut CombatQueryBudget::new(100)),Ok(false));
 assert_eq!(ctx.line_of_sight(p(0,0),p(0,0),&mut CombatQueryBudget::new(0)),Err(CombatQueryError::WorkLimitExceeded));
}
