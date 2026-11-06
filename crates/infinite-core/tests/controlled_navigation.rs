use infinite_core::{CollisionMap,GridPosition as P,MovementStatus,NavigationStatus,PathError,World};
use std::num::{NonZeroU16,NonZeroU32};
fn speed(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn open()->World{World::new(n(50))}

#[test]
fn application_clock_radius_and_preview_do_not_double_integrate(){
 let mut w=open();let e=w.spawn(P{x:0,y:0}).unwrap();
 w.queue_controlled_navigation(e,P{x:1000,y:0},speed(3200),16).unwrap();w.plan_pending_navigation();
 for _ in 0..10{w.advance_tick().unwrap();}assert_eq!(w.position(e).unwrap(),P{x:0,y:0});
 for _ in 0..10{let p=w.controlled_route(e,50_000,speed(3200),true).unwrap();assert_eq!(p.position,P{x:160,y:0});}
 assert_eq!(w.position(e).unwrap(),P{x:0,y:0});
 for _ in 0..7{w.controlled_route(e,50_000,speed(3200),false).unwrap();}
 assert_eq!(w.position(e).unwrap(),P{x:984,y:0});assert_eq!(w.movement_status(e).unwrap(),MovementStatus::Arrived);
}

#[test]
fn fractional_microsecond_slices_preserve_distance_credit(){
 let mut a=open();let mut b=open();let start=P{x:-31,y:17};let goal=P{x:10000,y:4000};
 let x=a.spawn(start).unwrap();let y=b.spawn(start).unwrap();
 for (w,e) in [(&mut a,x),(&mut b,y)]{w.queue_controlled_navigation(e,goal,speed(3201),16).unwrap();w.plan_pending_navigation();}
 for _ in 0..50{
  a.controlled_route(x,50_000,speed(3201),false).unwrap();
  for _ in 0..20{b.controlled_route(y,2500,speed(3201),false).unwrap();}
  assert_eq!(a.position(x).unwrap(),b.position(y).unwrap());
 }
}

#[test]
fn cancellation_relocation_and_regular_movement_return_ownership(){
 let mut w=open();let e=w.spawn(P{x:0,y:0}).unwrap();
 w.queue_controlled_navigation(e,P{x:1000,y:0},speed(1000),16).unwrap();w.plan_pending_navigation();
 w.controlled_route(e,50_000,speed(1000),false).unwrap();w.stop_moving(e).unwrap();
 assert!(w.controlled_route(e,50_000,speed(1000),false).is_err());w.advance_tick().unwrap();assert_eq!(w.position(e).unwrap().x,50);
 w.queue_navigation(e,P{x:1000,y:0},speed(1000)).unwrap();w.advance_tick().unwrap();assert_eq!(w.position(e).unwrap().x,100);
 w.queue_controlled_navigation(e,P{x:2000,y:0},speed(1000),0).unwrap();w.plan_pending_navigation();
 w.set_position(e,P{x:300,y:0}).unwrap();assert!(w.controlled_route(e,50_000,speed(1000),false).is_err());
 w.navigate_to(e,P{x:400,y:0},speed(1000)).unwrap();w.advance_tick().unwrap();assert_eq!(w.position(e).unwrap().x,350);
 w.despawn(e).unwrap();assert!(w.controlled_route(e,50_000,speed(1000),false).is_err());
}

#[test]
fn shared_budget_wait_is_not_unreachable_and_search_is_resumable(){
 let mut w=open();let e=w.spawn(P{x:0,y:0}).unwrap();w.restrict_navigation_work(0);
 w.queue_controlled_navigation(e,P{x:1000,y:0},speed(1000),16).unwrap();w.plan_pending_navigation();
 assert!(matches!(w.entity_state(e).unwrap().navigation,NavigationStatus::Pending{..}));
 for _ in 0..10{w.controlled_route(e,200_000,speed(1000),false).unwrap();assert_eq!(w.position(e).unwrap().x,0);w.plan_pending_navigation();}
 w.advance_tick().unwrap();w.advance_tick().unwrap();
 assert!(matches!(w.entity_state(e).unwrap().navigation,NavigationStatus::Ready{..}));
 w.controlled_route(e,50_000,speed(1000),false).unwrap();assert_eq!(w.position(e).unwrap().x,50);
}

#[test]
fn connected_detour_obeys_each_actual_leg_and_stops_inside_radius(){
 let mut map=CollisionMap::new_staggered_connected(P{x:0,y:0},n(12),n(12),n(48),n(12)).unwrap();
 for y in 2..9{map.set_blocked(4,y,true).unwrap();}map.prepare_navigation();
 let mut w=World::with_map(n(50),map);let goal=P{x:336,y:48};let e=w.spawn(P{x:96,y:48}).unwrap();
 w.queue_controlled_navigation(e,goal,speed(300),4).unwrap();
 let mut previous=w.position(e).unwrap();
 for _ in 0..200{
  w.advance_tick().unwrap();let sample=w.controlled_route(e,20_000,speed(300),false).unwrap();
  assert!(w.collision_map().unwrap().segment_is_clear(previous,sample.position));previous=sample.position;
  if w.movement_status(e).unwrap()==MovementStatus::Arrived{break;}
 }
 assert_eq!(w.movement_status(e).unwrap(),MovementStatus::Arrived);
 let dx=i64::from(previous.x-goal.x);let dy=i64::from(previous.y-goal.y);assert!(dx*dx+dy*dy<=16);
}

#[test]
fn disconnected_result_does_not_move_or_fall_back(){
 let mut map=CollisionMap::new_staggered_connected(P{x:0,y:0},n(8),n(8),n(48),n(12)).unwrap();for x in 0..8{map.set_blocked(x,4,true).unwrap();}map.prepare_navigation();
 let mut w=World::with_map(n(50),map);let start=P{x:96,y:24};let e=w.spawn(start).unwrap();
 w.queue_controlled_navigation(e,P{x:96,y:72},speed(1000),0).unwrap();
 for _ in 0..20{w.advance_tick().unwrap();w.controlled_route(e,200_000,speed(1000),false).unwrap();}
 assert!(matches!(w.entity_state(e).unwrap().navigation,NavigationStatus::Failed{error:PathError::Unreachable,..}));assert_eq!(w.position(e).unwrap(),start);
}
