use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};

fn nz(n:u32)->NonZeroU32 { NonZeroU32::new(n).unwrap() }
fn speed(n:u16)->NonZeroU16 { NonZeroU16::new(n).unwrap() }
fn p(x:i32,y:i32)->GridPosition { GridPosition{x,y} }
fn profile(windup:u16)->AttackProfile {
    AttackProfile{range:64,damage:nz(30),cooldown_ticks:speed(3),windup_ticks:windup}
}
fn actor(w:&mut World, position:GridPosition, hp:u32)->EntityId {
    let id=w.spawn(position).unwrap();w.configure_health(id,hp,nz(hp)).unwrap();id
}
fn pair()->(World,EntityId,EntityId) {
    let mut w=World::default();let a=actor(&mut w,p(0,0),100);let b=actor(&mut w,p(64,0),100);(w,a,b)
}
fn placeholder()->HealthChange {
    let s=HealthState{current:1,maximum:nz(1)};
    HealthChange{entity:EntityId{index:u32::MAX,generation:0},before:s,after:s}
}
fn event_placeholder()->CombatEvent {
    CombatEvent{attacker:placeholder().entity,target:placeholder().entity,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}
}
fn phase(w:&mut World)->(CombatReport,Vec<CombatEvent>,Vec<HealthChange>) {
    let n=w.active_combatants();let mut events=vec![event_placeholder();n];let mut changes=vec![placeholder();n];
    let report=w.resolve_combat(&mut events,&mut changes).unwrap();
    events.truncate(report.events_written);changes.truncate(report.changed_entities);(report,events,changes)
}
fn tick(w:&mut World)->(CombatReport,Vec<CombatEvent>,Vec<HealthChange>) { w.advance_tick().unwrap();phase(w) }
fn health(w:&mut World, commands:&[HealthCommand])->(HealthBatchReport,Vec<Result<HealthOutcome,HealthError>>,Vec<HealthChange>) {
    let mut results=vec![Err(HealthError::Unconfigured);commands.len()];let mut changes=vec![placeholder();commands.len()];
    let report=w.apply_health_commands(commands,&mut results,&mut changes).unwrap();changes.truncate(report.changed_entities);(report,results,changes)
}

#[test]
fn health_extremes_errors_coalescing_and_net_zero_events() {
    let mut w=World::default();let a=actor(&mut w,p(0,0),u32::MAX);
    let commands=[HealthCommand::Damage{entity:a,amount:u32::MAX},HealthCommand::Heal{entity:a,amount:1},
        HealthCommand::Revive{entity:a,current:nz(u32::MAX)},HealthCommand::Heal{entity:a,amount:u32::MAX}];
    let (report,results,changes)=health(&mut w,&commands);
    assert_eq!((report.accepted,report.rejected,report.changed_entities),(3,1,0));
    assert!(results[0].unwrap().depleted());assert_eq!(results[1],Err(HealthError::Depleted));
    assert!(results[2].unwrap().revived());assert!(!results[3].unwrap().changed());assert!(changes.is_empty());
    let (_,_,changes)=health(&mut w,&[HealthCommand::Damage{entity:a,amount:10},HealthCommand::Heal{entity:a,amount:u32::MAX},HealthCommand::SetMaximum{entity:a,maximum:nz(7)}]);
    assert_eq!(changes.len(),1);assert_eq!(changes[0].before.current,u32::MAX);assert_eq!(changes[0].after.current,7);
    assert_eq!(w.health(a).unwrap().maximum,nz(7));
    // A second batch must not retain the first batch's change-buffer index.
    let (_,_,changes)=health(&mut w,&[HealthCommand::Damage{entity:a,amount:2}]);assert_eq!(changes[0].before.current,7);assert_eq!(changes[0].after.current,5);
    assert_eq!(w.tick(),Tick(0));
}

#[test]
fn health_preflight_and_entity_lifecycle_are_atomic() {
    let (mut w,a,b)=pair();let unconfigured=w.spawn(p(100,0)).unwrap();
    assert_eq!(w.health(unconfigured),Err(HealthError::Unconfigured));
    assert_eq!(w.configure_health(unconfigured,11,nz(10)),Err(HealthError::AboveMaximum));assert_eq!(w.health(unconfigured),Err(HealthError::Unconfigured));
    assert_eq!(w.configure_health(a,10,nz(10)),Err(HealthError::AlreadyConfigured));
    let cmd=HealthCommand::Damage{entity:a,amount:1};let mut results=[Err(HealthError::NotDepleted);1];let mut changes=[placeholder();1];
    assert_eq!(w.apply_health_commands(&[cmd],&mut [],&mut changes),Err(HealthBatchError::ResultsBufferTooSmall));
    assert_eq!(w.apply_health_commands(&[cmd],&mut results,&mut []),Err(HealthBatchError::ChangesBufferTooSmall));
    assert_eq!(w.apply_health_commands(&vec![cmd;MAX_HEALTH_COMMANDS+1],&mut results,&mut changes),Err(HealthBatchError::TooManyCommands));
    assert_eq!(results,[Err(HealthError::NotDepleted)]);assert_eq!(changes,[placeholder()]);assert_eq!(w.health(a).unwrap().current,100);
    w.despawn(a).unwrap();let replacement=w.spawn(p(0,0)).unwrap();assert_eq!(replacement.index,a.index);assert_ne!(replacement.generation,a.generation);
    assert_eq!(w.health(a),Err(HealthError::InvalidEntity));assert_eq!(w.health(replacement),Err(HealthError::Unconfigured));assert_eq!(w.health(b).unwrap().current,100);
}

#[test]
fn cooldown_and_phase_guard_survive_stop_restart_and_retarget() {
    let (mut w,a,b)=pair();let c=actor(&mut w,p(0,64),100);w.start_attack(a,b,profile(0)).unwrap();
    assert_eq!(tick(&mut w).0.hits,1);assert_eq!(w.attack_state(a).unwrap().next_ready,Tick(4));
    let mut events=[event_placeholder();1];let mut changes=[placeholder();1];
    assert_eq!(w.resolve_combat(&mut events,&mut changes),Err(CombatPhaseError::AlreadyResolved));assert_eq!(events,[event_placeholder()]);
    w.stop_attack(a).unwrap();w.start_attack(a,c,profile(0)).unwrap();
    assert_eq!(tick(&mut w).0.hits,0);assert_eq!(tick(&mut w).0.hits,0);assert_eq!(tick(&mut w).0.hits,1);
    assert_eq!(w.health(b).unwrap().current,70);assert_eq!(w.health(c).unwrap().current,70);
}

#[test]
fn phase_capacity_failure_does_not_consume_tick_or_mutate_state() {
    let (mut w,a,b)=pair();w.start_attack(a,b,profile(2)).unwrap();
    let mut events=[event_placeholder();1];let mut changes=[placeholder();1];
    assert_eq!(w.resolve_combat(&mut [],&mut changes),Err(CombatPhaseError::EventsBufferTooSmall));
    assert_eq!(w.resolve_combat(&mut events,&mut []),Err(CombatPhaseError::ChangesBufferTooSmall));
    assert_eq!(events,[event_placeholder()]);assert_eq!(changes,[placeholder()]);assert!(w.attack_state(a).unwrap().windup.is_none());
    let report=w.resolve_combat(&mut events,&mut changes).unwrap();assert_eq!(report.windups_started,1);assert_eq!(w.health(b).unwrap().current,100);
}

#[test]
fn sequential_lethal_damage_prevents_retaliation_and_coalesces_targets() {
    let (mut w,a,b)=pair();let c=actor(&mut w,p(0,0),100);
    health(&mut w,&[HealthCommand::Damage{entity:b,amount:50}]);
    w.start_attack(a,b,profile(0)).unwrap();w.start_attack(c,b,profile(0)).unwrap();w.start_attack(b,a,profile(0)).unwrap();
    let (report,events,changes)=tick(&mut w);
    assert_eq!((report.hits,report.ended,report.remaining_active),(2,2,1));
    assert_eq!(changes.len(),1);assert_eq!((changes[0].before.current,changes[0].after.current),(50,0));
    assert_eq!(w.health(a).unwrap().current,100);assert!(events.iter().any(|e|e.kind==CombatEventKind::Ended(CombatEndReason::AttackerDepleted)));
    assert_eq!(tick(&mut w).0.remaining_active,0);
}

#[test]
fn distance_matches_wide_integer_reference_at_boundaries() {
    let mut seed=17u64;
    let mut samples=vec![(p(0,0),p(3,4),5),(p(0,0),p(3,4),4),(p(i32::MIN,i32::MIN),p(i32::MAX,i32::MAX),u32::MAX),
        (p(i32::MIN,0),p(i32::MAX,0),u32::MAX),(p(-64,-64),p(-64,-64),0)];
    for _ in 0..256 { seed=seed.wrapping_mul(6364136223846793005).wrapping_add(1);let a=p(seed as i32,(seed>>32) as i32);
        seed=seed.wrapping_mul(6364136223846793005).wrapping_add(1);samples.push((a,p(seed as i32,(seed>>32) as i32),(seed>>16) as u32)); }
    for (a,b,r) in samples {let dx=i128::from(a.x)-i128::from(b.x);let dy=i128::from(a.y)-i128::from(b.y);
        assert_eq!(within_attack_range(a,b,r),dx*dx+dy*dy<=i128::from(r)*i128::from(r));}
}

#[test]
fn windup_repeat_and_late_resolution_preserve_timing() {
    let (mut w,a,b)=pair();let attack=profile(2);w.start_attack(a,b,attack).unwrap();
    let (report,events,_)=tick(&mut w);assert_eq!(report.windups_started,1);assert!(matches!(events[0].kind,CombatEventKind::WindupStarted{..}));
    let original=w.attack_state(a).unwrap();assert_eq!(original.windup.unwrap().hit_at,Tick(3));assert_eq!(original.next_ready,Tick(6));
    w.start_attack(a,b,attack).unwrap();assert_eq!(w.attack_state(a).unwrap(),original);
    assert_eq!(tick(&mut w).0.winding_up,1);assert_eq!(w.health(b).unwrap().current,100);
    for _ in 0..5 {w.advance_tick().unwrap();} // Resolve late at tick 7, no catch-up burst.
    assert_eq!(phase(&mut w).0.hits,1);assert_eq!(w.health(b).unwrap().current,70);assert_eq!(w.attack_state(a).unwrap().next_ready,Tick(10));
    assert_eq!(tick(&mut w).0.hits,0);
}

#[test]
fn windup_range_cancel_and_profile_change_keep_reserved_cooldown() {
    let (mut w,a,b)=pair();w.start_attack(a,b,profile(2)).unwrap();tick(&mut w);
    w.set_position(b,p(65,0)).unwrap();let (report,events,_)=tick(&mut w);assert_eq!(report.windups_cancelled,1);assert!(matches!(events[0].kind,CombatEventKind::WindupCancelled{..}));
    w.set_position(b,p(64,0)).unwrap();w.start_attack(a,b,profile(0)).unwrap();
    while w.tick().0<5 {assert_eq!(tick(&mut w).0.hits,0);}
    assert_eq!(tick(&mut w).0.hits,1);assert_eq!(w.health(b).unwrap().current,70);
    // A later active windup is cancelled by changing damage, not silently edited.
    w.start_attack(a,b,profile(2)).unwrap();while w.tick().0<9 {tick(&mut w);}
    assert!(w.attack_state(a).unwrap().windup.is_some());let ready=w.attack_state(a).unwrap().next_ready;
    let mut changed=profile(2);changed.damage=nz(99);w.start_attack(a,b,changed).unwrap();
    assert!(w.attack_state(a).unwrap().windup.is_none());assert_eq!(w.attack_state(a).unwrap().next_ready,ready);
}

#[test]
fn target_reuse_and_dense_row_moves_do_not_redirect_pending_hits() {
    let mut w=World::default();let filler=actor(&mut w,p(-100,0),100);let b=actor(&mut w,p(64,0),100);let a=actor(&mut w,p(0,0),100);
    w.start_attack(a,b,profile(2)).unwrap();tick(&mut w);w.despawn(filler).unwrap(); // Attacker changes dense row.
    assert_eq!(tick(&mut w).0.winding_up,1);assert_eq!(tick(&mut w).0.hits,1);assert_eq!(w.health(b).unwrap().current,70);
    while w.tick().0<6 {tick(&mut w);}assert!(w.attack_state(a).unwrap().windup.is_some());
    w.despawn(b).unwrap();let replacement=actor(&mut w,p(64,0),100);assert_eq!(replacement.index,b.index);
    let (_,events,_)=tick(&mut w);assert_eq!(events[0].kind,CombatEventKind::Ended(CombatEndReason::TargetGone));
    assert_eq!(w.health(replacement).unwrap().current,100);assert_eq!(w.active_combatants(),0);
}

fn grid_world(wall_to:u32, timeout:Option<NonZeroU32>)->World {
    let mut map=CollisionMap::new(p(0,0),nz(20),nz(14),nz(64)).unwrap();
    for y in 0..wall_to {map.set_blocked(9,y,true).unwrap();}
    World::with_map_and_navigation_config(DEFAULT_TICK_MS,map,NavigationConfig {work_per_tick:8192,work_per_slice:MIN_NAVIGATION_WORK_SLICE,
        queued_work_reserve:MIN_NAVIGATION_WORK_SLICE,queue_capacity:4,max_wait_ticks:timeout}).unwrap()
}
fn chase()->ChaseConfig {ChaseConfig{speed:speed(640),repath_ticks:speed(3),retry_ticks:speed(4),retarget_distance:nz(16)}}

#[test]
fn chase_routes_around_wall_and_stops_in_range_before_cooldown() {
    let mut w=grid_world(11,None);let a=actor(&mut w,p(160,224),100);let b=actor(&mut w,p(1120,224),100);
    let mut attack=profile(0);attack.range=96;attack.cooldown_ticks=speed(20);w.start_chase_attack(a,b,attack,chase()).unwrap();
    let mut went_around=false;let mut hit=false;
    for _ in 0..120 {let (report,_,_)=tick(&mut w);let pos=w.position(a).unwrap();went_around|=pos.y>=704;
        assert!(w.collision_map().unwrap().is_walkable(pos));assert!(w.last_navigation_progress().work_used<=8192);
        assert!(w.pending_navigation_count()<=4);assert!(report.chase_requests<=32);
        if report.hits>0 {assert!(within_attack_range(pos,w.position(b).unwrap(),96));assert_eq!(w.moving_count(),0);assert_eq!(w.pending_navigation_count(),0);hit=true;break;}}
    assert!(went_around&&hit,"must detour and actually hit");let pos=w.position(a).unwrap();
    for _ in 0..3 {assert_eq!(tick(&mut w).0.hits,0);assert_eq!(w.position(a).unwrap(),pos);}
}

#[test]
fn chase_failure_backs_off_and_identical_commands_do_not_reset_retry() {
    let mut w=grid_world(14,None);let a=actor(&mut w,p(160,224),100);let b=actor(&mut w,p(1120,224),100);let attack=profile(0);
    w.start_chase_attack(a,b,attack,chase()).unwrap();phase(&mut w);
    let failed=loop {tick(&mut w);let s=w.attack_state(a).unwrap().chase.unwrap();if matches!(s.status,ChaseStatus::Retrying(ChaseFailure::Path(PathError::Unreachable))){break s;}
        assert!(w.tick().0<20,"small disconnected grid must terminate");};
    assert_eq!(w.pending_navigation_count(),0);assert_eq!(w.moving_count(),0);
    while w.tick().0+1<failed.next_request.0 {w.start_chase_attack(a,b,attack,chase()).unwrap();assert_eq!(tick(&mut w).0.chase_requests,0);}
    assert_eq!(tick(&mut w).0.chase_requests,1);assert_eq!(w.health(b).unwrap().current,100);
}

#[test]
fn chase_admission_rotates_and_queue_full_retries_are_bounded() {
    let nav=NavigationConfig{queue_capacity:1,..NavigationConfig::default()};let mut w=World::with_navigation_config(DEFAULT_TICK_MS,nav).unwrap();
    let target=actor(&mut w,p(10000,0),100);let mut ids=Vec::new();let config=ChaseConfig{retry_ticks:speed(1),..chase()};
    for _ in 0..33 {let a=actor(&mut w,p(0,0),100);w.start_chase_attack(a,target,profile(0),config).unwrap();ids.push(a);}
    let (report,_,_)=phase(&mut w);assert_eq!((report.chase_requests,report.chase_deferred),(32,1));assert_eq!(w.pending_navigation_count(),1);
    assert_eq!(w.attack_state(ids[32]).unwrap().chase.unwrap().status,ChaseStatus::Throttled);
    let (report,_,_)=tick(&mut w);assert!(report.chase_requests<=32);assert_eq!(w.pending_navigation_count(),1);
    assert_eq!(w.attack_state(ids[32]).unwrap().chase.unwrap().status,ChaseStatus::Waiting);
    assert!(w.last_navigation_progress().work_used<=nav.work_per_tick);
}

#[test]
fn queue_capacity_pressure_retries_next_tick_without_failure_backoff() {
    let nav=NavigationConfig{queue_capacity:1,..NavigationConfig::default()};
    let mut w=World::with_navigation_config(DEFAULT_TICK_MS,nav).unwrap();
    let b=actor(&mut w,p(10000,0),100);let a=actor(&mut w,p(0,0),100);let c=actor(&mut w,p(1,0),100);
    for id in [a,c]{w.start_chase_attack(id,b,profile(0),ChaseConfig::default()).unwrap();}
    let (r,_,_)=phase(&mut w);assert_eq!(r.chase_requests,2);
    let waiting=w.attack_state(c).unwrap().chase.unwrap();assert_eq!(waiting.status,ChaseStatus::Throttled);
    assert_eq!(waiting.next_request,Tick(1));assert_eq!(waiting.config.retry_ticks.get(),10);
    let(r,_,_)=tick(&mut w);assert_eq!(r.chase_requests,1);
    assert_eq!(w.attack_state(c).unwrap().chase.unwrap().status,ChaseStatus::Waiting);
    assert_eq!(w.pending_navigation_count(),1);
}

#[test]
fn entering_range_cancels_pending_chase_even_while_attack_cools_down() {
    let (mut w,a,b)=pair();let mut attack=profile(0);attack.cooldown_ticks=speed(20);
    w.start_chase_attack(a,b,attack,chase()).unwrap();assert_eq!(tick(&mut w).0.hits,1);
    let ready=w.attack_state(a).unwrap().next_ready;w.set_position(b,p(500,0)).unwrap();assert_eq!(tick(&mut w).0.chase_requests,1);
    assert_eq!(w.pending_navigation_count(),1);w.set_position(b,p(64,0)).unwrap();tick(&mut w);
    assert_eq!(w.pending_navigation_count(),0);assert_eq!(w.moving_count(),0);assert_eq!(w.attack_state(a).unwrap().chase.unwrap().status,ChaseStatus::InRange);
    assert_eq!(w.attack_state(a).unwrap().next_ready,ready);assert_eq!(w.health(b).unwrap().current,70);
}

#[test]
fn successful_manual_movement_owns_route_and_cancels_windup() {
    let (mut w,a,b)=pair();w.start_chase_attack(a,b,profile(2),chase()).unwrap();tick(&mut w);
    let ready=w.attack_state(a).unwrap().next_ready;
    w.move_to(a,p(-1000,0),speed(320)).unwrap();assert!(w.attack_state(a).unwrap().windup.is_none());
    assert_eq!(w.attack_state(a).unwrap().chase.unwrap().status,ChaseStatus::Interrupted);
    for _ in 0..8 {assert_eq!(tick(&mut w).0.hits,0);}
    assert_eq!(w.entity_state(a).unwrap().target,Some(p(-1000,0)));assert_eq!(w.attack_state(a).unwrap().next_ready,ready);
    w.stop_attack(a).unwrap();let old=w.position(a).unwrap();w.advance_tick().unwrap();assert!(w.position(a).unwrap().x<old.x);
    assert_eq!(w.health(b).unwrap().current,100);
}

#[test]
fn rejected_navigation_preserves_chase_and_public_queue_yields_control() {
    let mut w=grid_world(11,None);let a=actor(&mut w,p(160,224),100);let b=actor(&mut w,p(1120,224),100);
    w.start_chase_attack(a,b,profile(0),chase()).unwrap();phase(&mut w);let old=w.attack_state(a).unwrap();
    for command in [MovementCommand::MoveTo{entity:a,target:p(608,224),speed:speed(640)},
        MovementCommand::NavigateTo{entity:a,target:p(608,224),speed:speed(640)},MovementCommand::QueueNavigation{entity:a,target:p(608,224),speed:speed(640)}] {
        let mut result=[Ok(())];w.apply_movement_commands(&[command],&mut result).unwrap();assert!(result[0].is_err());assert_eq!(w.attack_state(a).unwrap(),old);
    }
    w.queue_navigation(a,p(224,224),speed(640)).unwrap();assert_eq!(w.attack_state(a).unwrap().chase.unwrap().status,ChaseStatus::Interrupted);
    w.stop_attack(a).unwrap();assert_eq!(w.pending_navigation_count(),1); // Manual request survives.
    w.advance_tick().unwrap();assert!(w.position(a).unwrap().x>160);
}

#[test]
fn pending_chase_keeps_search_origin_and_uses_shared_budget_timeout() {
    let mut map=CollisionMap::new(p(0,0),nz(100),nz(100),nz(64)).unwrap();
    for y in 0..100 {map.set_blocked(50,y,true).unwrap();}
    let nav=NavigationConfig{work_per_tick:MIN_NAVIGATION_WORK_SLICE,work_per_slice:MIN_NAVIGATION_WORK_SLICE,
        queued_work_reserve:MIN_NAVIGATION_WORK_SLICE,queue_capacity:2,max_wait_ticks:Some(nz(1))};
    let mut w=World::with_map_and_navigation_config(DEFAULT_TICK_MS,map,nav).unwrap();
    let a=actor(&mut w,p(32,32),100);let b=actor(&mut w,p(6368,32),100);let probe=w.spawn(p(96,32)).unwrap();
    // A synchronous caller consumes the same allowance; chase must not obtain
    // a private budget in the following advance_tick.
    assert_eq!(w.navigate_to(probe,p(6368,32),speed(640)),Err(WorldError::Navigation(PathError::BudgetExhausted)));
    assert_eq!(w.navigation_budget().remaining(),0);
    w.start_chase_attack(a,b,profile(0),chase()).unwrap();phase(&mut w);
    let original=w.navigation_wait(a).unwrap().unwrap();w.set_position(b,p(6368,96)).unwrap();
    assert_eq!(tick(&mut w).0.chase_requests,0);
    let waiting=w.navigation_wait(a).unwrap().unwrap();assert_eq!(waiting.enqueued_at,original.enqueued_at);assert_eq!(waiting.work_used,0);
    assert_eq!(w.entity_state(a).unwrap().navigation,NavigationStatus::Pending{target:p(6368,32)});
    assert_eq!(tick(&mut w).0.chase_requests,0);assert_eq!(w.last_navigation_progress().timed_out,1);
    assert_eq!(w.attack_state(a).unwrap().chase.unwrap().status,ChaseStatus::Retrying(ChaseFailure::Cancelled(NavigationCancelReason::TimedOut)));
    assert_eq!(w.pending_navigation_count(),0);assert_eq!(w.moving_count(),0);assert_eq!(w.health(b).unwrap().current,100);
}

#[test]
fn moving_target_repath_is_throttled_and_cancel_does_not_resume_stale_route() {
    let (mut w,a,b)=pair();w.set_position(b,p(1024,0)).unwrap();w.start_chase_attack(a,b,profile(0),chase()).unwrap();
    phase(&mut w);tick(&mut w);assert_eq!(w.attack_state(a).unwrap().chase.unwrap().status,ChaseStatus::Following);
    w.set_position(b,p(1024,64)).unwrap();assert_eq!(tick(&mut w).0.chase_requests,0);
    assert_eq!(w.attack_state(a).unwrap().chase.unwrap().last_goal,Some(p(1024,0)));
    assert_eq!(tick(&mut w).0.chase_requests,1);assert_eq!(w.attack_state(a).unwrap().chase.unwrap().last_goal,Some(p(1024,64)));
    assert!(w.cancel_navigation(a).unwrap());assert_eq!(w.attack_state(a).unwrap().chase.unwrap().status,ChaseStatus::Interrupted);
    let position=w.position(a).unwrap();assert_eq!(tick(&mut w).0.chase_requests,0);assert_eq!(w.position(a).unwrap(),position);
    assert_eq!(w.moving_count(),0);
}
