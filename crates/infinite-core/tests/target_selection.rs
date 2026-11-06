use infinite_core::{CollisionMap,GridPosition as P};use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
#[test]
fn nearest_reachable_component_wins_over_closer_disconnected_cell(){
 let mut m=CollisionMap::new(P{x:0,y:0},n(12),n(12),n(100)).unwrap();for y in 0..12{m.set_blocked(6,y,true).unwrap();}
 m.prepare_target_selection();let s=P{x:150,y:550};let target=P{x:699,y:550};assert_eq!(m.select_nearby_target(s,target,2),Some(P{x:550,y:550}));
 assert_eq!(m.select_nearby_target(s,P{x:850,y:550},2),Some(P{x:850,y:550}),"legal target still needs ordinary navigation reachability");
 assert_eq!(m.select_nearby_target(s,target,0),None);
}
#[test]
fn ties_bounds_and_occupancy_invalidation_are_deterministic(){
 let mut m=CollisionMap::new(P{x:0,y:0},n(5),n(5),n(100)).unwrap();m.set_blocked(2,2,true).unwrap();m.prepare_target_selection();
 assert_eq!(m.select_nearby_target(P{x:50,y:50},P{x:250,y:250},1),Some(P{x:250,y:150}));
 assert_eq!(m.select_nearby_target(P{x:50,y:50},P{x:i32::MAX,y:i32::MIN},8),None);
 assert_eq!(m.select_nearby_target(P{x:50,y:50},P{x:250,y:250},33),None);
 m.set_blocked(2,1,true).unwrap();assert_eq!(m.select_nearby_target(P{x:50,y:50},P{x:250,y:250},1),None);
 m.prepare_target_selection();assert_eq!(m.select_nearby_target(P{x:50,y:50},P{x:250,y:250},1),Some(P{x:150,y:250}));
}
