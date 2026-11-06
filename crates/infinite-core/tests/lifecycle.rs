use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn actor(w:&mut World,profile:LifecycleProfile)->EntityId{let e=w.spawn(p(0)).unwrap();w.configure_health(e,100,n(100)).unwrap();w.register_lifecycle(e,profile).unwrap();e}
fn policy(corpse:u32,respawn:Option<u32>)->LifecycleProfile{LifecycleProfile{corpse_ticks:corpse,respawn_after:respawn}}
fn advance(w:&mut World,ticks:usize){for _ in 0..ticks{w.advance_tick().unwrap();}}
fn run(w:&mut World,limit:usize)->(LifecycleReport,Vec<LifecycleEvent>){let mut events=Vec::new();let r=w.resolve_lifecycle(limit,&mut events).unwrap();assert!(r.visited<=limit);assert_eq!(r.processed,events.len());assert!(r.blocked.is_none());(r,events)}
fn ready(events:&[LifecycleEvent])->RespawnTicket{events.iter().find_map(|e|match e.kind{LifecycleEventKind::RespawnReady(t)=>Some(t),_=>None}).unwrap()}
fn revive(w:&mut World,e:EntityId){let h=w.health(e).unwrap();let mut result=[Err(HealthError::Unconfigured)];w.apply_health_commands(&[HealthCommand::Revive{entity:e,current:n(100)}],&mut result,&mut[HealthChange{entity:e,before:h,after:h}]).unwrap();assert!(result[0].is_ok());}

#[test]
fn due_only_corpse_visibility_and_explicit_respawn_reset(){
 let mut w=World::default();let e=actor(&mut w,policy(3,Some(5)));w.configure_resource(e,7,30).unwrap();
 assert_eq!(run(&mut w,4096).0.visited,0);assert_eq!(w.lifecycle_activity().next_due,None);
 w.apply_damage(e,100).unwrap();assert_eq!(w.lifecycle_state(e).unwrap().phase,LifecyclePhase::NewlyDead);
 assert_eq!(run(&mut w,0).0.overdue,Some(Tick(0)));let (_,events)=run(&mut w,1);assert!(matches!(events[0].kind,LifecycleEventKind::DeathSettled{..}));assert_eq!(w.lifecycle_activity().next_due,Some(Tick(3)));
 advance(&mut w,2);assert_eq!(run(&mut w,4096).0.visited,0);assert!(w.lifecycle_state(e).unwrap().visible);
 advance(&mut w,1);let (_,events)=run(&mut w,1);assert_eq!(events[0].kind,LifecycleEventKind::CorpseHidden);assert_eq!(events[0].scheduled_at,Tick(3));assert!(!w.lifecycle_state(e).unwrap().visible);
 advance(&mut w,2);let (_,events)=run(&mut w,1);let ticket=ready(&events);assert_eq!(w.lifecycle_activity().awaiting_placement,1);assert_eq!(w.health(e).unwrap().current,0);
 let outcome=w.complete_respawn(ticket,p(50)).unwrap();assert_eq!(outcome.health.after.current,100);assert_eq!(outcome.resource.unwrap().after.current,30);assert_eq!(w.position(e).unwrap(),p(50));assert_eq!(w.lifecycle_state(e).unwrap().phase,LifecyclePhase::Alive);assert!(w.lifecycle_state(e).unwrap().visible);assert_eq!(w.lifecycle_activity().awaiting_placement,0);assert_eq!(w.complete_respawn(ticket,p(90)),Err(LifecycleError::StaleTicket));
}

#[test]
fn respawn_before_or_at_corpse_deadline_has_priority_and_failed_placement_retries(){
 for delay in [0,3]{let mut w=World::default();let e=actor(&mut w,policy(3,Some(delay)));w.apply_damage(e,100).unwrap();let (_,mut events)=run(&mut w,8);if delay>0{advance(&mut w,delay as usize);events.extend(run(&mut w,8).1);}assert!(!events.iter().any(|e|e.kind==LifecycleEventKind::CorpseHidden));let ticket=ready(&events);assert!(w.lifecycle_state(e).unwrap().visible);
 w.defer_respawn(ticket,n(1)).unwrap();assert_eq!(w.complete_respawn(ticket,p(0)),Err(LifecycleError::StaleTicket));assert!(w.lifecycle_state(e).unwrap().visible);assert_eq!(run(&mut w,8).0.visited,0);
 advance(&mut w,1);let (_,events)=run(&mut w,1);let next=ready(&events);assert_eq!(events[0].scheduled_at,w.tick());w.complete_respawn(next,p(0)).unwrap();}
}

#[test]
fn catchup_keeps_original_deadlines_order_and_equal_deadline_acceptance_order(){
 let mut w=World::default();let a=actor(&mut w,policy(2,Some(10)));let b=actor(&mut w,policy(3,Some(6)));let c=actor(&mut w,policy(3,Some(6)));
 for e in [a,b,c]{w.apply_damage(e,100).unwrap();}let (_,initial)=run(&mut w,10);assert_eq!(initial.iter().map(|e|e.entity).collect::<Vec<_>>(),vec![a,b,c]);advance(&mut w,12);
 let (_,events)=run(&mut w,10);assert_eq!(events.iter().map(|e|(e.scheduled_at,e.entity)).collect::<Vec<_>>(),vec![(Tick(2),a),(Tick(3),b),(Tick(3),c),(Tick(6),b),(Tick(6),c),(Tick(10),a)]);
 for e in events{if let LifecycleEventKind::RespawnReady(ticket)=e.kind{w.complete_respawn(ticket,p(0)).unwrap();}}assert_eq!(w.lifecycle_activity().queued,0);
}

#[test]
fn external_revive_unregister_and_reused_dense_rows_cancel_old_life_tickets(){
 let mut w=World::default();let gone=actor(&mut w,policy(0,None));let a=actor(&mut w,policy(0,Some(0)));let b=actor(&mut w,policy(100,None));
 for e in [gone,a,b]{w.apply_damage(e,100).unwrap();}let (_,events)=run(&mut w,4096);let ticket=ready(&events);assert_eq!(ticket.entity,a);assert!(w.health(gone).is_err());assert_eq!(w.lifecycle_state(b).unwrap().phase,LifecyclePhase::Corpse);
 revive(&mut w,a);assert_eq!(w.complete_respawn(ticket,p(0)),Err(LifecycleError::StaleTicket));w.apply_damage(a,100).unwrap();let (_,events)=run(&mut w,4096);let next=ready(&events);assert_ne!(ticket,next);
 w.unregister_lifecycle(a).unwrap();w.register_lifecycle(a,policy(0,Some(0))).unwrap();let (_,events)=run(&mut w,4096);let third=ready(&events);assert_ne!(next,third);assert_eq!(w.complete_respawn(next,p(0)),Err(LifecycleError::StaleTicket));w.complete_respawn(third,p(0)).unwrap();
 w.despawn(b).unwrap();let reused=actor(&mut w,policy(0,None));assert_ne!(reused,b);advance(&mut w,100);assert!(run(&mut w,4096).1.is_empty());assert_eq!(w.health(reused).unwrap().current,100);
}

#[test]
fn bounded_removal_storm_never_scans_idle_actors_or_loses_dense_swap_members(){
 let mut w=World::default();let survivor=actor(&mut w,policy(1,None));let entities:Vec<_>=(0..1000).map(|_|actor(&mut w,policy(0,None))).collect();for &e in &entities{w.apply_damage(e,100).unwrap();}
 let mut removed=Vec::new();let mut visits=0;while w.lifecycle_activity().queued>0{let (r,events)=run(&mut w,7);visits+=r.visited;removed.extend(events.iter().filter(|e|e.kind==LifecycleEventKind::Removed).map(|e|e.entity));}
 assert_eq!(removed,entities);assert_eq!(visits,2000);assert_eq!(w.lifecycle_activity().registered,1);assert_eq!(w.health(survivor).unwrap().current,100);assert_eq!(run(&mut w,4096).0.visited,0);
}

#[test]
fn respawn_preflight_rejects_effects_dirty_attributes_and_blocked_position_atomically(){
 let map=CollisionMap::new(p(0),n(10),n(10),n(16)).unwrap();let mut w=World::with_map(DEFAULT_TICK_MS,map);let e=actor(&mut w,policy(0,Some(0)));let source=w.spawn(p(16)).unwrap();w.configure_health(source,100,n(100)).unwrap();
 let effect=w.apply_periodic(PeriodicEffect{source,target:e,key:1,kind:PeriodicKind::Heal,amount:n(1),first_delay:10,interval:t(1),pulses:t(2),refresh:PeriodicRefresh::Replace}).unwrap().id;
 w.apply_damage(e,100).unwrap();let ticket=ready(&run(&mut w,8).1);assert_eq!(w.complete_respawn(ticket,p(32)),Err(LifecycleError::ResetPending));assert_eq!(w.position(e).unwrap(),p(0));assert_eq!(w.health(e).unwrap().current,0);w.cancel_effect(effect).unwrap();
 w.configure_attributes(e,CombatAttributes{attack:1,break_power:0,defense:0,maximum_health:n(120),maximum_resource:0,level:t(1)}).unwrap();assert_eq!(w.complete_respawn(ticket,p(32)),Err(LifecycleError::AttributesPending));w.resolve_attributes(&mut Vec::new());
 assert_eq!(w.complete_respawn(ticket,p(-100)),Err(LifecycleError::World(WorldError::BlockedPosition)));assert_eq!(w.position(e).unwrap(),p(0));assert_eq!(w.health(e).unwrap().current,0);
 assert_eq!(w.complete_respawn(ticket,p(32)).unwrap().health.after.current,120);
}

#[test]
fn death_cleanup_reports_windup_and_cast_then_respawn_resets_cooldown_not_dedup(){
 let mut w=World::default();let e=actor(&mut w,policy(0,Some(0)));let target=actor(&mut w,policy(10,None));w.configure_resource(e,100,100).unwrap();
 let spec=SkillSpec{id:SkillId(1),resource_cost:10,cooldown_ticks:100,windup_ticks:50,recovery:Some(SkillRecovery{group:0,ticks:100,ingress_grace_ticks:0}),ingress_grace_ticks:0};w.learn_skill(e,spec).unwrap();let request=CastRequest{caster:e,request_id:8,skill:SkillId(1),target:CastTarget::Entity(target)};w.start_cast(request).unwrap();
 w.start_attack(e,target,AttackProfile{range:10,damage:n(1),cooldown_ticks:t(100),windup_ticks:10}).unwrap();let health=w.health(target).unwrap();let mut combat=[CombatEvent{attacker:e,target,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}];w.resolve_combat(&mut combat,&mut[HealthChange{entity:target,before:health,after:health}]).unwrap();assert!(w.attack_state(e).unwrap().windup.is_some());
 w.apply_damage(e,100).unwrap();let (_,events)=run(&mut w,8);assert!(matches!(events[0].kind,LifecycleEventKind::DeathSettled{cancelled_windup:Some(_),cancelled_cast:Some(SkillEvent::Cancelled{reason:CastCancelReason::CasterDepleted,..})}));assert_eq!(w.active_casts(),0);assert_eq!(w.active_combatants(),0);let ticket=ready(&events);w.complete_respawn(ticket,p(0)).unwrap();
 assert_eq!(w.attack_state(e).unwrap().next_ready,Tick(0));assert_eq!(w.skill_state(e,SkillId(1)).unwrap().cooldown_ready,Tick(0));assert_eq!(w.skill_state(e,SkillId(1)).unwrap().recovery_ready,Tick(0));assert!(w.start_cast(request).unwrap().duplicate);assert_eq!(w.resource(e).unwrap().current,100);assert!(!w.start_cast(CastRequest{request_id:9,..request}).unwrap().duplicate);
}

#[test]
fn respawn_clears_ai_control_and_threat_and_death_halts_movement(){
 let mut w=World::default();let e=actor(&mut w,policy(0,Some(0)));let target=actor(&mut w,policy(1,None));w.register_ai_target(target,true).unwrap();
 w.register_ai_actor(e,AiProfile{aggro_radius:100,leash_radius:300,bonus_radius:100,attack:AttackProfile{range:1,damage:n(1),cooldown_ticks:t(2),windup_ticks:0},attack_break_range:1,chase:ChaseConfig::default()}).unwrap();w.set_ai_control_until(e,Tick(100)).unwrap();w.add_ai_damage_threat(e,target,20).unwrap();w.move_to(e,p(100),t(20)).unwrap();
 w.apply_damage(e,100).unwrap();let ticket=ready(&run(&mut w,8).1);assert_eq!(w.movement_target(e).unwrap(),None);w.complete_respawn(ticket,p(10)).unwrap();assert!(w.ai_threats(e).unwrap().is_empty());let mut ai=AiSystem::new(n(32));let r=ai.resolve(&mut w,4096,&mut CombatQueryBudget::new(10000),&mut Vec::new()).unwrap();assert_eq!(r.remaining,0);assert!(!w.ai_state(e).unwrap().controlled);assert_eq!(w.ai_state(e).unwrap().target,Some(target));
}

#[test]
fn dense_current_tick_cohorts_match_one_item_heap_order_with_zero_delay_and_catchup(){
 fn simulate(limit:usize,late:bool)->Vec<LifecycleEvent>{
  let mut w=World::default();let entities:Vec<_>=(0..512).map(|i|actor(&mut w,policy(if i%3==0{0}else{2},if i%5==0{None}else{Some(4)}))).collect();
  for e in entities{w.apply_damage(e,100).unwrap();}if late{advance(&mut w,8);}let mut all=Vec::new();
  for _ in 0..6{loop{let (r,events)=run(&mut w,limit);for e in &events{if let LifecycleEventKind::RespawnReady(ticket)=e.kind{w.complete_respawn(ticket,p(0)).unwrap();}}all.extend(events);if r.overdue.is_none()&&r.unscheduled==0{break;}}w.advance_tick().unwrap();}
  all
 }
 for late in [false,true]{assert_eq!(simulate(4096,late),simulate(1,late));}
}

#[test]
fn resumed_cohort_invalidates_on_external_revive_despawn_and_new_death(){
 fn simulate(limit:usize)->Vec<LifecycleEvent>{
  let mut w=World::default();let entities:Vec<_>=(0..512).map(|_|actor(&mut w,policy(3,Some(5)))).collect();for &e in &entities{w.apply_damage(e,100).unwrap();}
  let mut all=Vec::new();while all.len()<128{let (_,events)=run(&mut w,limit.min(128-all.len()));all.extend(events);}
  revive(&mut w,entities[400]);w.despawn(entities[401]).unwrap();let replacement=actor(&mut w,policy(3,Some(5)));w.apply_damage(replacement,100).unwrap();
  for _ in 0..6{loop{let (r,events)=run(&mut w,limit);for e in &events{if let LifecycleEventKind::RespawnReady(ticket)=e.kind{w.complete_respawn(ticket,p(0)).unwrap();}}all.extend(events);if r.overdue.is_none()&&r.unscheduled==0{break;}}w.advance_tick().unwrap();}
  assert_eq!(w.health(entities[400]).unwrap().current,100);assert_eq!(w.health(replacement).unwrap().current,100);all
 }
 assert_eq!(simulate(128),simulate(1));
}
