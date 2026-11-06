use infinite_core::{CollisionMap, GridPosition as P, NavigationBudget, PathPlanner};
use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn map()->CollisionMap{CollisionMap::new_staggered_connected(P{x:0,y:0},n(8),n(8),n(48),n(12)).unwrap()}

#[test]
fn open_side_allows_tangency_but_not_solid_interior(){
    let mut m=map();m.set_blocked(3,4,true).unwrap();
    let a=P{x:122,y:42};let b=P{x:166,y:42};
    assert!(m.segment_is_clear(a,b));
    assert!(!m.segment_is_clear(P{x:122,y:43},P{x:166,y:43}));
    assert!(m.is_walkable(P{x:144,y:42}));
    assert!(!m.is_walkable(P{x:144,y:43}));
    assert!(!m.is_walkable(P{x:0,y:-7}));
    assert!(m.is_walkable(P{x:0,y:-6}));
    assert!(!m.is_walkable(P{x:0,y:90}));
}

#[test]
fn exact_boundaries_preserve_symmetry_and_point_rules(){
    let mut m=map();for y in 1..7{if y!=4{m.set_blocked(3,y,true).unwrap();}}
    for x in (-24..380).step_by(12){for y in (-6..98).step_by(6){
        let a=P{x,y};let b=P{x:360-x,y:84-y};
        assert_eq!(m.segment_is_clear(a,a),m.is_walkable(a),"{a:?}");
        assert_eq!(m.segment_is_clear(a,b),m.segment_is_clear(b,a),"{a:?} {b:?}");
    }}
}

#[test]
fn disconnected_rows_and_budget_cannot_tunnel(){
    let mut m=map();for x in 0..8{m.set_blocked(x,4,true).unwrap();}
    assert!(!m.segment_is_clear(P{x:96,y:24},P{x:96,y:72}));
    let mut planner=PathPlanner::default();
    assert!(planner.plan_with_budget(&m,P{x:96,y:24},P{x:96,y:72},&mut NavigationBudget::new(1)).is_err());
}

#[test]
fn prepared_navigation_uses_same_connected_collision(){
    let mut m=map();for y in 1..7{m.set_blocked(3,y,true).unwrap();}
    let a=P{x:72,y:30};let b=P{x:264,y:30};
    let mut planner=PathPlanner::default();
    let first=planner.plan_with_budget(&m,a,b,&mut NavigationBudget::new(u32::MAX)).unwrap();
    let expected=first.waypoints().to_vec();
    m.prepare_navigation();
    let route=planner.plan_with_budget(&m,a,b,&mut NavigationBudget::new(u32::MAX)).unwrap();
    assert_eq!(route.waypoints(),expected);
    let mut prior=a;for &next in route.waypoints(){assert!(m.segment_is_clear(prior,next));prior=next;}
    assert_eq!(prior,b);
}
