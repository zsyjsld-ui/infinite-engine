use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn p(x:i32,y:i32)->GridPosition{GridPosition{x,y}}
fn map(staggered:bool)->CollisionMap{
    let mut m=if staggered{CollisionMap::new_staggered(p(0,0),n(20),n(28),n(64),n(16)).unwrap()}
        else{CollisionMap::new(p(0,0),n(20),n(28),n(64)).unwrap()};
    for y in 0..22{m.set_blocked(9,y,true).unwrap();}m.prepare_navigation();m
}
#[test]
fn different_starts_share_goal_and_every_small_movement_segment_is_clear(){
    fn send<T:Send>(){}send::<World>(); // Preserve moving a world to a worker thread.
    for staggered in [false,true]{
        let mut w=World::with_map(DEFAULT_TICK_MS,map(staggered));let goal=p(1120,if staggered{48}else{224});let mut ids=Vec::new();
        for i in 0..16{let row=2+i/4;let from=p((i%4+1)*64+if staggered{(row%2)*32}else{32},row*if staggered{16}else{64}+if staggered{0}else{32});let id=w.spawn(from).unwrap();w.queue_navigation(id,goal,NonZeroU16::new(80).unwrap()).unwrap();ids.push(id);}
        let mut shared=0;for _ in 0..2000{
            let before:Vec<_>=ids.iter().map(|&id|w.position(id).unwrap()).collect();w.advance_tick().unwrap();shared+=w.last_navigation_progress().shared;
            assert!(w.last_navigation_budget().used()<=65536);
            for(&id,from)in ids.iter().zip(before){let state=w.entity_state(id).unwrap();assert!(!matches!(state.navigation,NavigationStatus::Failed{..}));assert_ne!(state.movement,MovementStatus::Blocked);assert!(w.collision_map().unwrap().segment_is_clear(from,state.position));let dx=i64::from(from.x-state.position.x);let dy=i64::from(from.y-state.position.y);assert!(dx*dx+dy*dy<=36);}
            if ids.iter().all(|&id|w.position(id).unwrap()==goal){break;}
        }
        assert!(shared>=15);assert!(ids.iter().all(|&id|w.position(id).unwrap()==goal));
    }
}
#[test]
fn exhausted_shared_frontier_falls_back_to_individual_astar(){
    let mut m=CollisionMap::new(p(0,0),n(200),n(200),n(64)).unwrap();m.set_blocked(100,0,true).unwrap();m.prepare_navigation();
    let mut w=World::with_map(DEFAULT_TICK_MS,m);let mut ids=Vec::new();
    for x in [32,96]{let id=w.spawn(p(x,32)).unwrap();w.queue_navigation(id,p(12768,32),NonZeroU16::new(2240).unwrap()).unwrap();ids.push(id);}
    let mut shared=0;for _ in 0..200{w.advance_tick().unwrap();shared+=w.last_navigation_progress().shared;assert!(w.last_navigation_budget().used()<=65536);}
    assert_eq!(shared,0,"large reverse frontier must fall back before these starts");
    for id in ids{assert_eq!(w.position(id).unwrap(),p(12768,32));}
}
#[test]
fn eviction_and_individual_cancellation_do_not_invalidate_pending_shared_searches(){
    let nav=NavigationConfig{work_per_tick:MIN_NAVIGATION_WORK_SLICE,work_per_slice:MIN_NAVIGATION_WORK_SLICE,queued_work_reserve:MIN_NAVIGATION_WORK_SLICE,..NavigationConfig::default()};
    let mut w=World::with_map_and_navigation_config(DEFAULT_TICK_MS,map(false),nav).unwrap();let mut pairs=Vec::new();
    for g in [224,352,480]{for x in [160,224]{let id=w.spawn(p(x,224)).unwrap();w.queue_navigation(id,p(1120,g),NonZeroU16::new(2240).unwrap()).unwrap();pairs.push((id,p(1120,g)));}}
    w.advance_tick().unwrap();assert!(w.cancel_navigation(pairs[0].0).unwrap());let stopped=w.position(pairs[0].0).unwrap();
    for _ in 0..200{w.advance_tick().unwrap();assert!(w.last_navigation_budget().used()<=nav.work_per_tick);}
    assert_eq!(w.position(pairs[0].0).unwrap(),stopped);for &(id,goal)in &pairs[1..]{assert_eq!(w.position(id).unwrap(),goal);}
}

#[test]
fn shared_unreachable_goal_reports_failure_without_crossing_the_wall(){
    let mut m=CollisionMap::new(p(0,0),n(20),n(28),n(64)).unwrap();
    for y in 0..28{m.set_blocked(9,y,true).unwrap();}m.prepare_navigation();
    let mut w=World::with_map(DEFAULT_TICK_MS,m);let mut ids=Vec::new();
    for x in [96,160,224,288]{let at=p(x,224);let id=w.spawn(at).unwrap();w.queue_navigation(id,p(1120,224),NonZeroU16::new(2240).unwrap()).unwrap();ids.push((id,at));}
    for _ in 0..20{w.advance_tick().unwrap();assert!(w.last_navigation_budget().used()<=65536);}
    for(id,at)in ids{assert_eq!(w.position(id).unwrap(),at);assert!(matches!(w.entity_state(id).unwrap().navigation,NavigationStatus::Failed{error:PathError::Unreachable,..}));}
}
