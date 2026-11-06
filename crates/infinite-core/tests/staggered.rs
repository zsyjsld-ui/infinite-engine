use infinite_core::{CollisionMap, GridPosition as P, MapError, PathPlanner, NavigationBudget, World, DEFAULT_TICK_MS, NavigationStatus, MovementStatus};
use std::num::{NonZeroU32,NonZeroU16};
fn n(v:u32)->NonZeroU32 { NonZeroU32::new(v).unwrap() }
fn map()->CollisionMap {CollisionMap::new_staggered(P{x:0,y:0},n(12),n(12),n(768),n(192)).unwrap()}

#[test]
fn staggered_closed_boundaries_and_signed_origin() {
    let mut m=map();m.set_blocked(3,4,true).unwrap();
    assert!(!m.is_walkable(P{x:2304,y:768}));
    assert!(!m.is_walkable(P{x:1920,y:768}));
    assert!(m.is_walkable(P{x:1919,y:768}));
    assert!(!m.is_walkable(P{x:2304,y:864}));
    assert!(m.is_walkable(P{x:2304,y:865}));
    assert!(!m.is_walkable(P{x:-384,y:0}));
    assert!(m.is_walkable(P{x:-383,y:0}));
    let shifted=CollisionMap::new_staggered(P{x:-5000,y:-4000},n(12),n(12),n(768),n(192)).unwrap();
    assert!(shifted.is_walkable(P{x:-4232,y:-3808}));
    assert!(matches!(CollisionMap::new_staggered(P{x:0,y:0},n(2),n(2),n(3),n(2)),Err(MapError::InvalidCellGeometry)));
    assert!(matches!(CollisionMap::new_staggered(P{x:i32::MAX,y:0},n(2),n(2),n(768),n(192)),Err(MapError::OutsideCoordinateRange)));
}

#[test]
fn staggered_segment_symmetry_and_no_tunnelling() {
    let mut m=map();m.set_blocked(3,4,true).unwrap();
    let a=P{x:768,y:768};let b=P{x:3840,y:768};
    assert!(!m.segment_is_clear(a,b));assert!(!m.segment_is_clear(b,a));
    for x in (-500..5000).step_by(151) {for y in (-100..2100).step_by(79) {
        let p=P{x,y};let q=P{x:4500-x,y:1900-y};
        assert_eq!(m.segment_is_clear(p,q),m.segment_is_clear(q,p),"{p:?} {q:?}");
        assert_eq!(m.segment_is_clear(p,p),m.is_walkable(p));
    }}
}

#[test]
fn staggered_plan_and_movement_share_collision() {
    let mut m=map();for y in 2..9 {m.set_blocked(4,y,true).unwrap();}
    let start=P{x:1536,y:960};let goal=P{x:5376,y:960};
    let route=PathPlanner::default().plan_with_budget(&m,start,goal,&mut NavigationBudget::new(u32::MAX)).unwrap();
    let mut previous=start;
    for &point in route.waypoints(){assert!(m.segment_is_clear(previous,point));previous=point;}
    assert_eq!(previous,goal);
    let mut world=World::with_map(DEFAULT_TICK_MS,m);
    let id=world.spawn(start).unwrap();
    world.queue_navigation(id,goal,NonZeroU16::new(2240).unwrap()).unwrap();
    for _ in 0..400 {
        world.advance_tick().unwrap();let state=world.entity_state(id).unwrap();
        assert!(!matches!(state.navigation,NavigationStatus::Failed{..}));
        assert_ne!(state.movement,MovementStatus::Blocked);
        if state.movement==MovementStatus::Arrived {assert_eq!(state.position,goal);return;}
    }
    panic!("did not arrive");
}

#[test]
fn staggered_disconnected_and_invalid_goal() {
    let mut m=map();for x in 0..12 {m.set_blocked(x,5,true).unwrap();}
    let mut planner=PathPlanner::default();
    assert!(matches!(planner.plan_with_budget(&m,P{x:1536,y:384},P{x:1536,y:1536},&mut NavigationBudget::new(u32::MAX)),Err(infinite_core::PathError::Unreachable)));
    assert!(matches!(planner.plan(&m,P{x:1536,y:384},P{x:1920,y:960}),Err(infinite_core::PathError::InvalidGoal)));
}

#[test]
fn square_map_behavior_still_works() {
    let mut m=CollisionMap::new(P{x:0,y:0},n(8),n(8),n(192)).unwrap();
    m.set_blocked(3,3,true).unwrap();
    assert!(!m.segment_is_clear(P{x:96,y:96},P{x:1440,y:1440}));
    let start=P{x:96,y:96};let goal=P{x:1440,y:1440};
    let path=PathPlanner::default().plan(&m,start,goal).unwrap();
    let mut previous=start;for &p in path.waypoints(){assert!(m.segment_is_clear(previous,p));previous=p;}
    assert_eq!(previous,goal);
}

#[test]
fn staggered_long_ray_resumes_with_bounded_tick_work() {
    let m=CollisionMap::new_staggered(P{x:0,y:0},n(20000),n(3),n(768),n(192)).unwrap();
    let config=infinite_core::NavigationConfig {work_per_tick:8192,work_per_slice:4101,queued_work_reserve:4101,queue_capacity:4,max_wait_ticks:None};
    let mut world=World::with_map_and_navigation_config(DEFAULT_TICK_MS,m,config).unwrap();
    let id=world.spawn(P{x:1152,y:192}).unwrap();
    world.queue_navigation(id,P{x:19998*768+384,y:192},NonZeroU16::new(2240).unwrap()).unwrap();
    world.advance_tick().unwrap();
    assert!(matches!(world.entity_state(id).unwrap().navigation,NavigationStatus::Pending{..}));
    assert!(world.last_navigation_budget().used()<=8192);
    for _ in 0..5 {
        world.advance_tick().unwrap();
        assert!(world.last_navigation_budget().used()<=8192);
        if matches!(world.entity_state(id).unwrap().navigation,NavigationStatus::Ready{..}){return;}
    }
    panic!("ray failed to resume");
}

#[test]
fn prepared_edges_preserve_routes_and_invalidate_on_occupancy_edit() {
    let mut m=map();for y in 2..9 {m.set_blocked(4,y,true).unwrap();}
    let a=P{x:1536,y:960};let b=P{x:5376,y:960};
    let mut planner=PathPlanner::default();
    let before=planner.plan_with_budget(&m,a,b,&mut NavigationBudget::new(u32::MAX)).unwrap().waypoints().to_vec();
    m.prepare_navigation();assert_eq!(m.navigation_cache_bytes(),144);
    let after=planner.plan_with_budget(&m,a,b,&mut NavigationBudget::new(u32::MAX)).unwrap().waypoints().to_vec();
    assert_eq!(before,after);
    m.set_blocked(4,4,true).unwrap();assert_eq!(m.navigation_cache_bytes(),144); // unchanged cell
    for y in 0..12 {m.set_blocked(4,y,true).unwrap();}
    assert_eq!(m.navigation_cache_bytes(),0);
    assert!(matches!(planner.plan_with_budget(&m,a,b,&mut NavigationBudget::new(u32::MAX)),Err(infinite_core::PathError::Unreachable)));
    m.prepare_navigation();
    assert!(matches!(planner.plan_with_budget(&m,a,b,&mut NavigationBudget::new(u32::MAX)),Err(infinite_core::PathError::Unreachable)));
}

#[test]
fn prepared_square_edges_preserve_obstacles() {
    let mut m=CollisionMap::new(P{x:0,y:0},n(8),n(8),n(192)).unwrap();
    for y in 1..7 {m.set_blocked(3,y,true).unwrap();}
    let mut planner=PathPlanner::default();let a=P{x:96,y:96};let b=P{x:1440,y:1440};
    let before=planner.plan_with_budget(&m,a,b,&mut NavigationBudget::new(u32::MAX)).unwrap().waypoints().to_vec();
    m.prepare_navigation();assert_eq!(m.navigation_cache_bytes(),64);
    let after=planner.plan_with_budget(&m,a,b,&mut NavigationBudget::new(u32::MAX)).unwrap().waypoints().to_vec();assert_eq!(before,after);
    let mut previous=a;for p in after {assert!(m.segment_is_clear(previous,p));previous=p;}
}
