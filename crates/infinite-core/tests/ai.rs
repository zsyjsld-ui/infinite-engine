use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn p(x:i32,y:i32)->GridPosition{GridPosition{x,y}}
fn entity(w:&mut World,pos:GridPosition)->EntityId{let id=w.spawn(pos).unwrap();w.configure_health(id,100,n(100)).unwrap();id}
fn profile()->AiProfile{AiProfile{aggro_radius:100,leash_radius:300,bonus_radius:200,attack:AttackProfile{range:10,damage:n(7),cooldown_ticks:t(3),windup_ticks:0},attack_break_range:30,chase:ChaseConfig{speed:t(320),..ChaseConfig::default()}}}
fn monster(w:&mut World,pos:GridPosition)->EntityId{let id=entity(w,pos);w.register_ai_actor(id,profile()).unwrap();id}
fn player(w:&mut World,pos:GridPosition)->EntityId{let id=entity(w,pos);w.register_ai_target(id,true).unwrap();id}
fn resolve(w:&mut World,ai:&mut AiSystem)->Vec<AiEvent>{let mut events=Vec::new();let r=ai.resolve(w,4096,&mut CombatQueryBudget::new(1000000),&mut events).unwrap();assert_eq!(r.remaining,0);assert!(r.deferred.is_none());events}
fn next(w:&mut World,ai:&mut AiSystem)->Vec<AiEvent>{w.advance_tick().unwrap();resolve(w,ai)}
fn phase(w:&mut World)->(CombatReport,Vec<CombatEvent>){let health=HealthState{current:1,maximum:n(1)};let dummy=EntityId{index:u32::MAX,generation:0};let mut events=vec![CombatEvent{attacker:dummy,target:dummy,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};w.active_combatants()];let mut changes=vec![HealthChange{entity:dummy,before:health,after:health};events.len()];let r=w.resolve_combat(&mut events,&mut changes).unwrap();events.truncate(r.events_written);(r,events)}

#[test]
fn nearest_ties_use_registration_order_and_equal_threat_keeps_target(){
 let mut w=World::default();let m=monster(&mut w,p(0,0));let a=entity(&mut w,p(50,0));let b=entity(&mut w,p(-50,0));w.register_ai_target(b,true).unwrap();w.register_ai_target(a,true).unwrap();let mut ai=AiSystem::new(n(32));resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(b));
 w.add_ai_damage_threat(m,a,10).unwrap();w.add_ai_damage_threat(m,b,10).unwrap();resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(b));w.add_ai_damage_threat(m,a,1).unwrap();let events=resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(a));assert!(events.iter().any(|e|e.reason==AiReason::Switched));assert_eq!(w.ai_threats(m).unwrap().iter().find(|e|e.target==a).unwrap().damage,11);
}

#[test]
fn far_damage_wake_has_no_leash_until_entry_and_break_is_strict(){
 let mut w=World::default();let m=monster(&mut w,p(0,0));let a=player(&mut w,p(1000,0));let mut ai=AiSystem::new(n(32));resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,None);
 w.grant_shield(ShieldSpec{source:m,target:m,duration:None,profile:ShieldProfile::Absorb{capacity:n(30)}}).unwrap();let first=w.apply_damage(m,5).unwrap();w.add_ai_damage_threat(m,a,first.health.before.current-first.health.after.current).unwrap();resolve(&mut w,&mut ai);assert!(w.ai_threats(m).unwrap().is_empty());assert_eq!(w.ai_state(m).unwrap().target,None);
 let hit=w.apply_damage(m,35).unwrap();w.add_ai_damage_threat(m,a,hit.health.before.current-hit.health.after.current).unwrap();resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(a));assert!(!w.ai_state(m).unwrap().entered_aggro);
 w.set_position(a,p(2000,0)).unwrap();next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(a));w.set_position(a,p(100,0)).unwrap();next(&mut w,&mut ai);assert!(w.ai_state(m).unwrap().entered_aggro);w.set_position(a,p(300,0)).unwrap();next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(a));w.set_ai_control_until(m,Tick(100)).unwrap();w.set_position(a,p(301,0)).unwrap();let events=next(&mut w,&mut ai);assert_eq!(events[0].reason,AiReason::Leash);assert_eq!(w.ai_state(m).unwrap().target,None);assert_eq!(w.position(m).unwrap(),p(0,0));assert_eq!(w.health(m).unwrap().current,90);assert_eq!(w.active_combatants(),0);
}

#[test]
fn new_damage_target_resets_old_leash_before_validation_and_bonus_expires(){
 let mut w=World::default();let m=monster(&mut w,p(0,0));let a=player(&mut w,p(50,0));let b=player(&mut w,p(150,0));let mut ai=AiSystem::new(n(32));resolve(&mut w,&mut ai);w.add_ai_damage_threat(m,a,2).unwrap();resolve(&mut w,&mut ai);w.set_position(a,p(500,0)).unwrap();w.add_ai_damage_threat(m,b,3).unwrap();let events=next(&mut w,&mut ai);assert!(!events.iter().any(|e|e.reason==AiReason::Leash));assert_eq!(w.ai_state(m).unwrap().target,Some(b));assert!(!w.ai_state(m).unwrap().entered_aggro);
 let c=player(&mut w,p(160,0));w.set_ai_bonus_threat(m,c,100,Tick(3)).unwrap();next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(c));w.set_position(a,p(60,0)).unwrap();next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(a));
}

#[test]
fn chase_loss_falls_back_far_but_attack_loss_idles_and_eligibility_is_live(){
 for distance in [5,50]{let mut w=World::default();let m=monster(&mut w,p(0,0));let a=player(&mut w,p(distance,0));let b=player(&mut w,p(1000,0));let mut ai=AiSystem::new(n(32));resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(a));w.set_ai_target_eligible(a,false).unwrap();next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,if distance==5{None}else{Some(b)});if distance==5{assert_eq!(w.active_combatants(),0);}else{assert!(!w.ai_state(m).unwrap().entered_aggro);}}
}

#[test]
fn attack_hysteresis_and_control_preserve_cooldown_and_report_windup_cancel(){
 let mut w=World::default();let m=entity(&mut w,p(0,0));let mut cfg=profile();cfg.attack.windup_ticks=2;w.register_ai_actor(m,cfg).unwrap();let a=player(&mut w,p(5,0));let mut ai=AiSystem::new(n(32));resolve(&mut w,&mut ai);let (r,_)=phase(&mut w);assert_eq!(r.windups_started,1);let original=w.attack_state(m).unwrap().windup.unwrap();let ready=w.attack_state(m).unwrap().next_ready;
 w.set_position(a,p(25,0)).unwrap();next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().mode,AiMode::Attacking);assert_eq!(w.attack_state(m).unwrap().windup,Some(original));w.set_ai_control_until(m,Tick(4)).unwrap();let events=resolve(&mut w,&mut ai);assert_eq!(events[0].cancelled_windup,Some(original));assert_eq!(w.active_combatants(),0);assert_eq!(w.attack_state(m).unwrap().next_ready,ready);
 next(&mut w,&mut ai);next(&mut w,&mut ai);assert_eq!(w.active_combatants(),0);next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().mode,AiMode::Attacking);assert!(w.attack_state(m).unwrap().intent.is_some());w.set_position(a,p(31,0)).unwrap();next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().mode,AiMode::Chasing);assert!(w.attack_state(m).unwrap().chase.is_some());
}

#[test]
fn budget_failure_never_commits_partial_choice_and_resume_is_fair(){
 let mut w=World::default();let monsters:Vec<_>=(0..3).map(|_|monster(&mut w,p(0,0))).collect();let a=player(&mut w,p(50,0));let b=player(&mut w,p(40,0));let mut ai=AiSystem::new(n(32));let mut events=Vec::new();let r=ai.resolve(&mut w,3,&mut CombatQueryBudget::new(1),&mut events).unwrap();assert_eq!(r.processed,0);assert_eq!(r.remaining,3);assert_eq!(r.deferred,Some(CombatQueryError::WorkLimitExceeded));assert!(events.is_empty());assert!(monsters.iter().all(|&m|w.ai_state(m).unwrap().target.is_none()));
 for left in (0..3).rev(){let r=ai.resolve(&mut w,1,&mut CombatQueryBudget::new(2),&mut events).unwrap();assert_eq!(r.processed,1);assert_eq!(r.remaining,left);}
 assert!(monsters.iter().all(|&m|w.ai_state(m).unwrap().target==Some(b)));let r=ai.resolve(&mut w,3,&mut CombatQueryBudget::new(0),&mut events).unwrap();assert_eq!(r.processed,0);assert_eq!(r.remaining,0);assert_eq!(w.health(a).unwrap().current,100);
}

#[test]
fn death_revive_and_dense_reuse_do_not_keep_threat_from_old_life(){
 let mut w=World::default();let removed=monster(&mut w,p(-100,0));let m=monster(&mut w,p(0,0));let a=player(&mut w,p(500,0));let b=player(&mut w,p(20,0));w.add_ai_damage_threat(m,a,20).unwrap();w.despawn(removed).unwrap();let mut ai=AiSystem::new(n(32));resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(a));w.apply_damage(m,100).unwrap();assert!(w.ai_threats(m).unwrap().is_empty());
 let h=w.health(m).unwrap();let mut results=[Err(HealthError::Unconfigured)];let mut changes=[HealthChange{entity:m,before:h,after:h}];w.apply_health_commands(&[HealthCommand::Revive{entity:m,current:n(100)}],&mut results,&mut changes).unwrap();next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(b));w.despawn(b).unwrap();let replacement=player(&mut w,p(10,0));assert_ne!(b,replacement);next(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(replacement));assert!(w.ai_threats(m).unwrap().is_empty());
}

#[test]
fn actual_ai_chase_uses_navigation_budget_and_hits_after_arrival(){
 let map=CollisionMap::new(p(-32,-32),n(20),n(20),n(16)).unwrap();let mut w=World::with_map(DEFAULT_TICK_MS,map);let m=monster(&mut w,p(0,0));let a=player(&mut w,p(80,0));let mut ai=AiSystem::new(n(32));let mut hit=false;
 for _ in 0..20{w.step(&[],&mut [],&[],&mut []).unwrap();resolve(&mut w,&mut ai);let(r,_)=phase(&mut w);assert!(r.chase_requests<=MAX_CHASE_REQUESTS_PER_TICK);if r.hits>0{hit=true;break;}}
 assert!(hit);assert_eq!(w.health(a).unwrap().current,93);assert!(within_attack_range(w.position(m).unwrap(),w.position(a).unwrap(),profile().attack_break_range));
}

#[test]
fn target_registration_order_survives_despawn_and_spatial_candidate_order(){
 let mut w=World::default();let m=monster(&mut w,p(0,0));let first=entity(&mut w,p(70,0));let second=entity(&mut w,p(-70,0));w.register_ai_target(second,true).unwrap();w.register_ai_target(first,true).unwrap();let gone=player(&mut w,p(400,0));for i in 0..126{player(&mut w,p(500+i*20,500));}w.despawn(gone).unwrap();let mut ai=AiSystem::new(n(64));resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(second));
}

#[test]
fn same_tick_eligibility_and_control_stop_old_intents_even_when_reselection_is_blocked(){
 let mut w=World::default();let m=monster(&mut w,p(0,0));let a=player(&mut w,p(50,0));let b=player(&mut w,p(60,0));let mut ai=AiSystem::new(n(32));resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(a));w.add_ai_damage_threat(m,b,5).unwrap();w.set_ai_target_eligible(a,false).unwrap();let mut events=Vec::new();let r=ai.resolve(&mut w,1,&mut CombatQueryBudget::new(0),&mut events).unwrap();assert!(r.deferred.is_some());assert_eq!(w.active_combatants(),0);assert_eq!(events[0].reason,AiReason::TargetLost);resolve(&mut w,&mut ai);assert_eq!(w.ai_state(m).unwrap().target,Some(b));
 w.set_ai_control_until(m,Tick(5)).unwrap();w.add_ai_damage_threat(m,b,1).unwrap();let r=ai.resolve(&mut w,1,&mut CombatQueryBudget::new(0),&mut events).unwrap();assert!(r.deferred.is_some());assert!(w.ai_state(m).unwrap().controlled);assert_eq!(w.active_combatants(),0);resolve(&mut w,&mut ai);assert!(w.ai_state(m).unwrap().controlled);
}

#[test]
fn total_threat_capacity_is_atomic_and_death_despawn_prune_release_accounting(){
 let mut w=World::default();let targets:Vec<_>=(0..MAX_AI_TARGETS).map(|_|player(&mut w,p(0,0))).collect();let mut actors=Vec::new();
 for _ in 0..MAX_AI_TOTAL_THREATS/MAX_AI_THREATS{let actor=monster(&mut w,p(0,0));actors.push(actor);for &target in &targets{w.add_ai_damage_threat(actor,target,1).unwrap();}}
 assert_eq!(w.ai_total_threats(),MAX_AI_TOTAL_THREATS);let extra=monster(&mut w,p(0,0));assert_eq!(w.add_ai_damage_threat(extra,targets[0],1),Err(AiError::ThreatCapacity));assert!(w.ai_threats(extra).unwrap().is_empty());
 w.add_ai_damage_threat(actors[0],targets[0],1).unwrap();assert_eq!(w.ai_total_threats(),MAX_AI_TOTAL_THREATS);w.despawn(actors[0]).unwrap();assert_eq!(w.ai_total_threats(),MAX_AI_TOTAL_THREATS-MAX_AI_THREATS);
 w.add_ai_damage_threat(extra,targets[0],1).unwrap();w.despawn(targets[0]).unwrap();assert_eq!(w.prune_ai_threats(extra).unwrap(),1);assert!(w.ai_threats(extra).unwrap().is_empty());w.apply_damage(actors[1],100).unwrap();assert_eq!(w.ai_total_threats(),MAX_AI_TOTAL_THREATS-MAX_AI_THREATS*2);
}
