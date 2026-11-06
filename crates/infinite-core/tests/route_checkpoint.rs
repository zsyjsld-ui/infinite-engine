use infinite_core::{World,GridPosition as P,RouteCheckpoint};
use std::num::{NonZeroU16,NonZeroU32};
fn speed()->NonZeroU16{NonZeroU16::new(3201).unwrap()}
fn world()->World{World::new(NonZeroU32::new(50).unwrap())}

#[test]
fn exact_cursor_replay_retains_fraction_and_corner_budget(){
 let mut a=world();let mut b=world();let start=P{x:-37,y:61};let x=a.spawn(start).unwrap();let y=b.spawn(start).unwrap();
 let seed=RouteCheckpoint{position:start,origin:start,progress:0,remainder:0,radius:0,waypoints:vec![P{x:65,y:113},P{x:97,y:7},P{x:5000,y:8000}]};
 a.restore_route_checkpoint(x,seed).unwrap();
 for _ in 0..30{
  a.controlled_route(x,16_667,speed(),false).unwrap();let snapshot=a.route_checkpoint(x).unwrap().unwrap();b.restore_route_checkpoint(y,snapshot.clone()).unwrap();
  assert_eq!(b.route_checkpoint(y).unwrap().unwrap(),snapshot);
  a.controlled_route(x,50_000,speed(),false).unwrap();
  for micros in [16_667,16_667,16_666]{b.controlled_route(y,micros,speed(),false).unwrap();}
  assert_eq!(a.position(x).unwrap(),b.position(y).unwrap());assert_eq!(a.route_checkpoint(x).unwrap(),b.route_checkpoint(y).unwrap());
 }
}

#[test]
fn malformed_checkpoint_is_atomic_and_cancel_retires_cursor(){
 let mut w=world();let p=P{x:0,y:0};let e=w.spawn(p).unwrap();w.queue_controlled_navigation(e,P{x:8000,y:100},speed(),0).unwrap();w.plan_pending_navigation();
 let before=w.route_checkpoint(e).unwrap().unwrap();
 for case in 0..5{
  let mut bad=before.clone();match case{0=>bad.progress=u64::MAX,1=>bad.remainder=1_000_000,2=>bad.waypoints.clear(),3=>bad.position.x+=17,_=>bad.waypoints.insert(0,bad.origin)};
  assert!(w.restore_route_checkpoint(e,bad).is_err());assert_eq!(w.route_checkpoint(e).unwrap().unwrap(),before);assert_eq!(w.position(e).unwrap(),p);
 }
 w.stop_moving(e).unwrap();assert_eq!(w.route_checkpoint(e).unwrap(),None);
}

#[test]
fn compact_following_preserves_full_cursor_and_current_corner_tangent(){
 let mut a=world();let mut b=world();let p=P{x:0,y:0};let x=a.spawn(p).unwrap();let y=b.spawn(p).unwrap();
 let seed=RouteCheckpoint{position:p,origin:p,progress:0,remainder:0,radius:0,waypoints:vec![P{x:160,y:0},P{x:160,y:320},P{x:32000,y:16000}]};
 a.restore_route_checkpoint(x,seed.clone()).unwrap();b.restore_route_checkpoint(y,seed).unwrap();
 for micros in [0,16_667,16_667,16_666,200_000,0,70_003,50_000]{
  let full=a.controlled_route(x,micros,speed(),false).unwrap();let lite=b.controlled_route_lite(y,micros,speed()).unwrap();
  assert_eq!(full.position,lite.position);assert!(lite.path.is_empty());assert_eq!(a.route_checkpoint(x).unwrap(),b.route_checkpoint(y).unwrap());
  if let Some(cursor)=b.route_checkpoint(y).unwrap(){assert_eq!(lite.forecast,cursor.waypoints[0]);}
 }
}

#[test]
fn copying_validated_cursor_preserves_exact_route_and_rejects_invalid_source(){
 let mut w=world();let start=P{x:0,y:0};let source=w.spawn(start).unwrap();let target=w.spawn(start).unwrap();
 assert!(w.copy_route_checkpoint(source,target).is_err());
 let seed=RouteCheckpoint{position:start,origin:start,progress:0,remainder:123,radius:0,waypoints:vec![P{x:160,y:0},P{x:160,y:320},P{x:32000,y:16000}]};w.restore_route_checkpoint(source,seed).unwrap();
 for _ in 0..30{w.controlled_route(source,50_000,speed(),false).unwrap();w.copy_route_checkpoint(source,target).unwrap();assert_eq!(w.route_checkpoint(source).unwrap(),w.route_checkpoint(target).unwrap());assert_eq!(w.position(source).unwrap(),w.position(target).unwrap());}
 w.stop_moving(source).unwrap();let before=w.route_checkpoint(target).unwrap();assert!(w.copy_route_checkpoint(source,target).is_err());assert_eq!(w.route_checkpoint(target).unwrap(),before);
}

#[test]
fn suffix_geometry_reuse_cannot_certify_a_changed_origin_or_corner(){
 use infinite_core::CollisionMap;
 let n=|v|NonZeroU32::new(v).unwrap();let mut map=CollisionMap::new(P{x:0,y:0},n(32),n(32),n(256)).unwrap();map.set_blocked(4,2,true).unwrap();
 let mut w=World::with_map(n(50),map);let start=P{x:512,y:512};let e=w.spawn(start).unwrap();let seed=RouteCheckpoint{position:start,origin:start,progress:0,remainder:0,radius:0,waypoints:vec![P{x:512,y:1024},P{x:2048,y:1024},P{x:2048,y:512}]};w.restore_route_checkpoint(e,seed.clone()).unwrap();
 let mut shortcut=seed.clone();shortcut.waypoints=vec![P{x:2048,y:512}];assert!(w.restore_route_checkpoint(e,shortcut).is_err());assert_eq!(w.route_checkpoint(e).unwrap(),Some(seed.clone()));
 let mut invalid=seed.clone();invalid.progress=1;assert!(w.restore_route_checkpoint(e,invalid).is_err());
 let suffix=RouteCheckpoint{position:P{x:512,y:1024},origin:P{x:512,y:1024},progress:0,waypoints:seed.waypoints[1..].to_vec(),..seed};w.restore_route_checkpoint(e,suffix.clone()).unwrap();assert_eq!(w.route_checkpoint(e).unwrap(),Some(suffix));
}
