use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn p(x:i32,y:i32)->GridPosition{GridPosition{x,y}}
fn map(staggered:bool)->CollisionMap{
    let mut map=if staggered{CollisionMap::new_staggered(p(0,0),n(20),n(14),n(64),n(16)).unwrap()}
        else{CollisionMap::new(p(0,0),n(20),n(14),n(64)).unwrap()};
    for y in 0..11{map.set_blocked(9,y,true).unwrap();}map.prepare_navigation();map
}
fn route(w:&mut World,from:GridPosition,to:GridPosition,speed:u16)->(Vec<GridPosition>,usize){
    let id=w.spawn(from).unwrap();w.queue_navigation(id,to,NonZeroU16::new(speed).unwrap()).unwrap();
    let mut trace=Vec::new();let mut cached=0;
    for _ in 0..2000{
        w.advance_tick().unwrap();cached+=w.last_navigation_progress().cached;
        assert!(w.last_navigation_budget().used()<=w.navigation_config().work_per_tick);
        let state=w.entity_state(id).unwrap();
        assert!(!matches!(state.navigation,NavigationStatus::Failed{..}));assert_ne!(state.movement,MovementStatus::Blocked);
        if !matches!(state.navigation,NavigationStatus::Pending{..}){trace.push(state.position);}
        if state.movement==MovementStatus::Arrived{assert_eq!(state.position,to);w.despawn(id).unwrap();return(trace,cached);}
    }
    panic!("route did not finish")
}
#[test]
fn exact_cached_routes_preserve_every_movement_step_at_different_speeds(){
    for staggered in [false,true]{
        let from=p(160,if staggered{48}else{224});let to=p(1120,from.y);
        let mut shared=World::with_map(DEFAULT_TICK_MS,map(staggered));
        let(_,cached)=route(&mut shared,from,to,2240);assert_eq!(cached,0);
        for speed in [640,2240,10000]{
            let expected=route(&mut World::with_map(DEFAULT_TICK_MS,map(staggered)),from,to,speed).0;
            let(actual,cached)=route(&mut shared,from,to,speed);assert_eq!(cached,1);assert_eq!(actual,expected);
        }
    }
}
#[test]
fn endpoints_are_exact_and_fifo_storage_is_bounded(){
    let mut w=World::default();
    assert_eq!(route(&mut w,p(0,0),p(100,0),640).1,0);
    assert_eq!(route(&mut w,p(1,0),p(100,0),640).1,0);
    assert_eq!(route(&mut w,p(0,0),p(101,0),640).1,0);
    assert_eq!(route(&mut w,p(0,0),p(100,0),640).1,1);
    for x in 102..119{assert_eq!(route(&mut w,p(0,0),p(x,0),640).1,0);}
    assert_eq!(route(&mut w,p(0,0),p(100,0),640).1,0,"oldest route was evicted");
    assert_eq!(route(&mut World::default(),p(0,0),p(100,0),640).1,0,"cache is world-local");
}
#[test]
fn duplicate_batch_reuses_search_and_cancellation_is_independent(){
    let mut w=World::with_map(DEFAULT_TICK_MS,map(false));let mut ids=Vec::new();
    for _ in 0..32{let id=w.spawn(p(160,224)).unwrap();w.queue_navigation(id,p(1120,224),NonZeroU16::new(2240).unwrap()).unwrap();ids.push(id);}
    assert!(w.cancel_navigation(ids[0]).unwrap());w.despawn(ids[1]).unwrap();w.set_position(ids[2],p(224,224)).unwrap();
    let mut cached=0;for _ in 0..50{w.advance_tick().unwrap();cached+=w.last_navigation_progress().cached;assert!(w.last_navigation_budget().used()<=65536);}
    assert!(cached>=28);for &id in &ids[3..]{assert_eq!(w.position(id).unwrap(),p(1120,224));}
    assert_eq!(w.position(ids[0]).unwrap(),p(160,224));assert_eq!(w.position(ids[2]).unwrap(),p(224,224));
}
#[test]
fn cache_does_not_bypass_timeout_or_shared_budget(){
    let mut w=World::with_map(DEFAULT_TICK_MS,map(false));route(&mut w,p(160,224),p(1120,224),2240);
    // Exhaust the remaining shared allowance through a separate synchronous caller.
    exhaust(&mut w);
    let id=w.spawn(p(160,224)).unwrap();w.queue_navigation(id,p(1120,224),NonZeroU16::new(2240).unwrap()).unwrap();
    w.advance_tick().unwrap();assert_eq!(w.last_navigation_progress().cached,0);
    assert!(matches!(w.entity_state(id).unwrap().navigation,NavigationStatus::Pending{..}));
    w.advance_tick().unwrap();assert_eq!(w.last_navigation_progress().cached,1);
    let config=NavigationConfig{max_wait_ticks:Some(n(1)),..NavigationConfig::default()};
    let mut w=World::with_map_and_navigation_config(DEFAULT_TICK_MS,map(false),config).unwrap();
    route(&mut w,p(160,224),p(1120,224),2240);
    exhaust(&mut w);
    let id=w.spawn(p(160,224)).unwrap();w.queue_navigation(id,p(1120,224),NonZeroU16::new(2240).unwrap()).unwrap();
    w.advance_tick().unwrap();w.advance_tick().unwrap();assert_eq!(w.last_navigation_progress().timed_out,1);assert_eq!(w.last_navigation_progress().cached,0);
    assert!(matches!(w.entity_state(id).unwrap().navigation,NavigationStatus::Cancelled{reason:NavigationCancelReason::TimedOut,..}));
}
fn exhaust(w:&mut World){
    let probe=w.spawn(p(160,224)).unwrap();
    for _ in 0..1000{
        w.stop_moving(probe).unwrap();
        if w.navigate_to(probe,p(1120,224),NonZeroU16::new(2240).unwrap()).is_err(){break;}
    }
    assert_eq!(w.navigation_budget().remaining(),0);w.despawn(probe).unwrap();
}
