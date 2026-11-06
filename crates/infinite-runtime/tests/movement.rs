use std::num::NonZeroU32;
use infinite_core::{CollisionMap, GridPosition as P};
use infinite_runtime::{Admission, Config, ControlToken as T, Input, MovementRuntime, Phase, RuntimeError};
fn n(v:u32)->NonZeroU32 {NonZeroU32::new(v).unwrap()}
fn map()->CollisionMap {CollisionMap::new(P{x:0,y:0},n(128),n(128),n(256)).unwrap()}
fn runtime()->MovementRuntime {MovementRuntime::new(map(),Config {lease_ticks:1000,..Config::default()}).unwrap()}
fn token(id:u64)->T {T{id,epoch:1}}
fn attach(r:&mut MovementRuntime,id:u64,speed:u16) {r.attach(token(id),P{x:512,y:512},speed).unwrap();}

#[test]
fn disconnect_retires_queued_work_but_old_disconnect_cannot_stop_new_owner(){
    let mut r=runtime();attach(&mut r,1,3200);
    r.submit(token(1),1,Input::Goal(P{x:3000,y:512})).unwrap();r.revoke(token(1)).unwrap();
    r.step().unwrap();assert_eq!(r.view(1).unwrap().position,P{x:512,y:512});assert_eq!(r.view(1).unwrap().phase,Phase::Cancelled);
    let fresh=T{id:1,epoch:2};r.renew(fresh).unwrap();r.submit(fresh,1,Input::Direction{x:32767,y:0}).unwrap();
    assert!(matches!(r.revoke(token(1)),Err(RuntimeError::Epoch)));
    r.step().unwrap();assert_eq!(r.view(1).unwrap().position,P{x:672,y:512});assert_eq!(r.view(1).unwrap().speed,3200);
    r.revoke(fresh).unwrap();for _ in 0..4{r.step().unwrap();}assert_eq!(r.view(1).unwrap().position,P{x:672,y:512});
}

#[test]
fn input_flood_has_one_time_budget_and_latest_intent_with_distinct_ack_boundaries(){
    let mut r=runtime();attach(&mut r,1,3200);
    for seq in 1..=1000 {r.submit(token(1),seq,Input::Direction{x:32767,y:0}).unwrap();}
    let pending=r.view(1).unwrap();assert_eq!((pending.received_seq,pending.processed_seq),(1000,0));assert_eq!(pending.position,P{x:512,y:512});
    let report=r.step().unwrap();assert_eq!(report.commands,1);assert_eq!(r.view(1).unwrap().position,P{x:672,y:512});
    assert_eq!(r.view(1).unwrap().processed_seq,1000);assert_eq!(r.tick().0,1);
    r.submit(token(1),1001,Input::Stop).unwrap();r.submit(token(1),1002,Input::Goal(P{x:512,y:1600})).unwrap();r.submit(token(1),1003,Input::Stop).unwrap();
    r.step().unwrap();let v=r.view(1).unwrap();assert_eq!(v.position,P{x:672,y:512});assert_eq!(v.phase,Phase::Cancelled);assert_eq!(v.intent_seq,1003);
}

#[test]
fn repeated_same_direction_retains_fractional_credit_and_rejects_unbounded_vectors(){
    let mut r=runtime();attach(&mut r,1,3);
    for seq in 1..=20 {r.submit(token(1),seq,Input::Direction{x:32767,y:0}).unwrap();r.step().unwrap();}
    assert_eq!(r.view(1).unwrap().position,P{x:515,y:512});
    assert!(matches!(r.submit(token(1),21,Input::Direction{x:32767,y:32767}),Err(RuntimeError::Direction)));
    assert_eq!(r.view(1).unwrap().received_seq,20);
    assert!(matches!(r.submit(token(1),21,Input::Direction{x:i16::MIN,y:0}),Err(RuntimeError::Direction)));
}

#[test]
fn keepalive_does_not_overwrite_queued_goal_and_duplicates_do_not_move_or_refresh_lease(){
    let mut r=runtime();attach(&mut r,1,3200);
    let goal=Input::Goal(P{x:3000,y:512});r.submit(token(1),1,goal).unwrap();r.submit(token(1),2,Input::KeepAlive).unwrap();
    assert_eq!(r.submit(token(1),2,Input::KeepAlive).unwrap(),Admission::Duplicate);
    assert!(matches!(r.submit(token(1),2,Input::Stop),Err(RuntimeError::Sequence)));
    r.step().unwrap();let v=r.view(1).unwrap();assert_eq!(v.intent_seq,1);assert_eq!(v.processed_seq,2);assert_eq!(v.position,P{x:672,y:512});
    assert!(matches!(r.submit(token(1),1,goal),Err(RuntimeError::Sequence)));
}

#[test]
fn zero_speed_and_disabled_time_never_accumulate_movement(){
    let mut r=runtime();attach(&mut r,1,0);r.submit(token(1),1,Input::Goal(P{x:3000,y:512})).unwrap();
    for _ in 0..10 {r.step().unwrap();}assert_eq!(r.view(1).unwrap().position,P{x:512,y:512});assert_eq!(r.view(1).unwrap().phase,Phase::Paused);
    r.set_motion_policy(1,3200,true).unwrap();r.step().unwrap();assert_eq!(r.view(1).unwrap().position,P{x:672,y:512});
    r.set_motion_policy(1,3200,false).unwrap();for _ in 0..10 {r.step().unwrap();}assert_eq!(r.view(1).unwrap().position,P{x:672,y:512});
    r.set_motion_policy(1,3200,true).unwrap();r.step().unwrap();assert_eq!(r.view(1).unwrap().position,P{x:832,y:512});
}

#[test]
fn cancelled_and_expired_control_cannot_restart_from_old_epoch(){
    let mut r=MovementRuntime::new(map(),Config{lease_ticks:2,..Config::default()}).unwrap();attach(&mut r,1,3200);
    r.submit(token(1),1,Input::Direction{x:32767,y:0}).unwrap();r.step().unwrap();r.step().unwrap();
    assert_eq!(r.submit(token(1),1,Input::Direction{x:32767,y:0}).unwrap(),Admission::Duplicate);
    let p=r.view(1).unwrap().position;assert_eq!(r.step().unwrap().expired,1);assert_eq!(r.view(1).unwrap().position,p);
    r.renew(T{id:1,epoch:2}).unwrap();assert!(matches!(r.submit(token(1),2,Input::Direction{x:32767,y:0}),Err(RuntimeError::Epoch)));
    r.step().unwrap();assert_eq!(r.view(1).unwrap().position,p);assert_eq!(r.view(1).unwrap().received_seq,0);
}

#[test]
fn command_fairness_is_bounded_and_renew_does_not_duplicate_queue_membership(){
    let mut r=MovementRuntime::new(map(),Config{commands_per_tick:1,lease_ticks:1000,..Config::default()}).unwrap();
    for id in 1..=3 {attach(&mut r,id,3200);r.submit(token(id),1,Input::Direction{x:32767,y:0}).unwrap();}
    assert_eq!(r.step().unwrap().pending,2);r.submit(token(1),2,Input::KeepAlive).unwrap();
    r.renew(T{id:2,epoch:2}).unwrap();r.submit(T{id:2,epoch:2},1,Input::Direction{x:0,y:32767}).unwrap();
    assert_eq!(r.step().unwrap().commands,1);assert_eq!(r.view(2).unwrap().position,P{x:512,y:672});
    r.step().unwrap();assert_eq!(r.view(3).unwrap().processed_seq,1);assert_eq!(r.view(1).unwrap().processed_seq,1);
    r.step().unwrap();assert_eq!(r.view(1).unwrap().processed_seq,2);
}

#[test]
fn excess_navigation_requests_wait_instead_of_failing_world_and_budget_is_global(){
    let mut r=runtime();for id in 1..=64 {attach(&mut r,id,3200);r.submit(token(id),1,Input::Goal(P{x:12000,y:12000})).unwrap();}
    let mut moved=vec![false;64];for _ in 0..30 {let report=r.step().unwrap();assert!(report.navigation_work<=8192);for id in 1..=64 {moved[(id-1)as usize]|=r.view(id).unwrap().position!=P{x:512,y:512};}}
    assert!(moved.iter().all(|&v|v));
}

#[test]
fn blocked_goal_cancels_prior_route_and_wall_blocks_manual_motion(){
    let mut m=map();for y in 0..128 {m.set_blocked(4,y,true).unwrap();}
    let mut r=MovementRuntime::new(m,Config{lease_ticks:1000,..Config::default()}).unwrap();attach(&mut r,1,3200);
    r.submit(token(1),1,Input::Goal(P{x:512,y:2000})).unwrap();r.step().unwrap();let p=r.view(1).unwrap().position;
    r.submit(token(1),2,Input::Goal(P{x:1100,y:512})).unwrap();r.step().unwrap();assert_eq!(r.view(1).unwrap().phase,Phase::Failed);assert_eq!(r.view(1).unwrap().position,p);
    r.submit(token(1),3,Input::Direction{x:32767,y:0}).unwrap();for _ in 0..20 {r.step().unwrap();}
    let v=r.view(1).unwrap();assert!(v.position.x<1024);assert_eq!(v.phase,Phase::Blocked);assert!(r.world().collision_map().unwrap().is_walkable(v.position));
}

#[test]
fn arrival_radius_and_stop_do_not_invent_extra_steps(){
    let mut r=runtime();attach(&mut r,1,3200);r.submit(token(1),1,Input::Goal(P{x:1512,y:512})).unwrap();
    for _ in 0..10 {r.step().unwrap();}let v=r.view(1).unwrap();assert_eq!(v.phase,Phase::Arrived);assert_eq!(v.position,P{x:1432,y:512});
    r.submit(token(1),2,Input::Stop).unwrap();r.step().unwrap();assert_eq!(r.view(1).unwrap().position,v.position);assert_eq!(r.view(1).unwrap().phase,Phase::Cancelled);
}
