use infinite_core::*;use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn entity(w:&mut World,hp:u32)->EntityId{let e=w.spawn(p(0)).unwrap();w.configure_health(e,hp,n(100)).unwrap();e}
#[test]
fn empty_map_can_skip_but_live_work_and_external_deadlines_are_rechecked(){
 let mut w=World::default();assert_eq!(w.map_activity(MapWork::default()),MapActivity{continuous:false,pending:false,next_due:None});w.skip_idle_to(Tick(100),MapWork::default()).unwrap();assert_eq!(w.tick(),Tick(100));assert_eq!(w.skip_idle_to(Tick(99),MapWork::default()),Err(MapSleepError::Past));
 assert_eq!(w.skip_idle_to(Tick(101),MapWork{pending:true,..MapWork::default()}),Err(MapSleepError::Pending));assert_eq!(w.skip_idle_to(Tick(110),MapWork{next_due:Some(Tick(105)),..MapWork::default()}),Err(MapSleepError::Deadline{due:Tick(105)}));
 let e=entity(&mut w,100);w.move_to(e,p(100),t(1)).unwrap();assert_eq!(w.skip_idle_to(Tick(101),MapWork::default()),Err(MapSleepError::Continuous));w.stop_moving(e).unwrap();w.skip_idle_to(Tick(105),MapWork::default()).unwrap();assert_eq!(w.position(e).unwrap(),p(0));
 let target=entity(&mut w,100);w.start_attack(e,target,AttackProfile{range:1,damage:n(1),cooldown_ticks:t(1),windup_ticks:0}).unwrap();assert_eq!(w.skip_idle_to(Tick(106),MapWork::default()),Err(MapSleepError::Continuous));
}

#[test]
fn population_and_lifecycle_waits_prevent_silent_sleep(){
 let mut w=World::default();w.resize_population(1).unwrap();assert_eq!(w.skip_idle_to(Tick(1),MapWork::default()),Err(MapSleepError::Deadline{due:Tick(0)}));w.skip_idle_to(Tick(5),MapWork{spawn_not_before:Some(Tick(5)),..MapWork::default()}).unwrap();
 let e=entity(&mut w,100);w.allocate_spawn_group(1,None,&[e],10).unwrap();w.register_lifecycle(e,LifecycleProfile{corpse_ticks:3,respawn_after:Some(5)}).unwrap();w.apply_damage(e,100).unwrap();assert_eq!(w.skip_idle_to(Tick(6),MapWork::default()),Err(MapSleepError::Deadline{due:Tick(5)}));
 w.resolve_lifecycle(10,&mut Vec::new()).unwrap();assert_eq!(w.map_activity(MapWork::default()).next_due,Some(Tick(8)));assert_eq!(w.skip_idle_to(Tick(9),MapWork::default()),Err(MapSleepError::Deadline{due:Tick(8)}));w.skip_idle_to(Tick(8),MapWork::default()).unwrap();w.resolve_lifecycle(10,&mut Vec::new()).unwrap();w.skip_idle_to(Tick(10),MapWork::default()).unwrap();w.resolve_lifecycle(10,&mut Vec::new()).unwrap();assert_eq!(w.skip_idle_to(Tick(11),MapWork::default()),Err(MapSleepError::Pending));
}

#[test]
fn queued_navigation_and_undrained_cancellation_keep_map_awake(){
 let map=CollisionMap::new(p(0),n(20),n(20),n(16)).unwrap();let mut w=World::with_map(DEFAULT_TICK_MS,map);let e=entity(&mut w,100);
 w.queue_navigation(e,p(100),t(20)).unwrap();assert_eq!(w.skip_idle_to(Tick(1),MapWork::default()),Err(MapSleepError::Continuous));w.cancel_navigation(e).unwrap();assert_eq!(w.skip_idle_to(Tick(1),MapWork::default()),Err(MapSleepError::Pending));assert!(w.pop_navigation_cancellation().is_some());w.skip_idle_to(Tick(1),MapWork::default()).unwrap();
}

#[test]
fn sleeping_ai_is_idle_but_same_cell_target_change_and_dirty_attributes_wake_map(){
 let mut w=World::default();let m=entity(&mut w,100);let target=entity(&mut w,100);w.set_position(target,p(151)).unwrap();w.set_position(m,p(100)).unwrap();w.register_ai_actor(m,AiProfile{aggro_radius:50,leash_radius:100,bonus_radius:50,attack:AttackProfile{range:1,damage:n(1),cooldown_ticks:t(2),windup_ticks:0},attack_break_range:1,chase:ChaseConfig::default()}).unwrap();w.register_ai_target(target,true).unwrap();let mut ai=AiSystem::new(n(32));ai.resolve(&mut w,4096,&mut CombatQueryBudget::new(10000),&mut Vec::new()).unwrap();w.skip_idle_to(Tick(20),MapWork::default()).unwrap();
 w.set_position(target,p(150)).unwrap();assert_eq!(w.skip_idle_to(Tick(21),MapWork::default()),Err(MapSleepError::Pending));ai.resolve(&mut w,4096,&mut CombatQueryBudget::new(10000),&mut Vec::new()).unwrap();assert_eq!(w.skip_idle_to(Tick(21),MapWork::default()),Err(MapSleepError::Continuous));
 let mut empty=World::default();let a=entity(&mut empty,100);empty.configure_attributes(a,CombatAttributes{attack:1,break_power:0,defense:0,maximum_health:n(100),maximum_resource:0,level:t(1)}).unwrap();assert_eq!(empty.skip_idle_to(Tick(1),MapWork::default()),Err(MapSleepError::Pending));empty.resolve_attributes(&mut Vec::new());empty.skip_idle_to(Tick(1),MapWork::default()).unwrap();
}

#[test]
fn deadline_jumps_match_each_tick_for_effects_shields_casts_and_corpses(){
 fn simulate(jump:bool)->(Vec<String>,u32){
  let mut w=World::default();let source=entity(&mut w,100);let target=entity(&mut w,50);let corpse=entity(&mut w,100);
  w.configure_resource(source,10,10).unwrap();w.learn_skill(source,SkillSpec{id:SkillId(1),resource_cost:1,cooldown_ticks:10,windup_ticks:8,recovery:None,ingress_grace_ticks:0}).unwrap();w.start_cast(CastRequest{caster:source,request_id:1,skill:SkillId(1),target:CastTarget::Entity(target)}).unwrap();
  w.apply_periodic(PeriodicEffect{source,target,key:1,kind:PeriodicKind::Heal,amount:n(7),first_delay:3,interval:t(4),pulses:t(3),refresh:PeriodicRefresh::Replace}).unwrap();w.grant_shield(ShieldSpec{source,target:source,duration:Some(t(6)),profile:ShieldProfile::Absorb{capacity:n(5)}}).unwrap();w.register_lifecycle(corpse,LifecycleProfile{corpse_ticks:4,respawn_after:Some(10)}).unwrap();w.apply_damage(corpse,100).unwrap();let mut all=Vec::new();
  loop{let mut life=Vec::new();w.resolve_lifecycle(4096,&mut life).unwrap();for e in life{all.push(format!("life:{e:?}"));if let LifecycleEventKind::RespawnReady(ticket)=e.kind{w.complete_respawn(ticket,p(0)).unwrap();}}
   let mut effects=Vec::new();w.resolve_effects(4096,&mut effects,&mut Vec::new()).unwrap();all.extend(effects.iter().map(|e|format!("effect:{e:?}")));let mut shields=Vec::new();w.resolve_shields(4096,&mut shields).unwrap();all.extend(shields.iter().map(|e|format!("shield:{e:?}")));let mut casts=Vec::new();w.resolve_casts(4096,&mut casts).unwrap();all.extend(casts.iter().map(|e|format!("cast:{e:?}")));
   if w.tick()>=Tick(12){break;}if jump{let next=w.map_activity(MapWork::default()).next_due.unwrap_or(Tick(12));w.skip_idle_to(next,MapWork::default()).unwrap();}else{w.advance_tick().unwrap();}
  }(all,w.health(target).unwrap().current)
 }let ordinary=simulate(false);assert_eq!(ordinary.1,71);assert_eq!(simulate(true),ordinary);
}
