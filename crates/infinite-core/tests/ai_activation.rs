use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn p(x:i32,y:i32)->GridPosition{GridPosition{x,y}}
fn entity(w:&mut World,pos:GridPosition)->EntityId{let e=w.spawn(pos).unwrap();w.configure_health(e,100,n(100)).unwrap();e}
fn profile(radius:u32)->AiProfile{AiProfile{aggro_radius:radius,leash_radius:radius.saturating_mul(3),bonus_radius:radius.saturating_mul(2),attack:AttackProfile{range:0,damage:n(7),cooldown_ticks:NonZeroU16::new(3).unwrap(),windup_ticks:2},attack_break_range:0,chase:ChaseConfig::default()}}
fn monster(w:&mut World,pos:GridPosition,radius:u32)->EntityId{let e=entity(w,pos);w.register_ai_actor(e,profile(radius)).unwrap();e}
fn player(w:&mut World,pos:GridPosition)->EntityId{let e=entity(w,pos);w.register_ai_target(e,true).unwrap();e}
fn run(w:&mut World,ai:&mut AiSystem,limit:usize,work:u32)->AiReport{let r=ai.resolve(w,limit,&mut CombatQueryBudget::new(work),&mut Vec::new()).unwrap();assert!(r.visited+r.wake_jobs<=limit);assert!(r.work.cells+r.work.candidates+r.work.sight_steps<=work);r}
fn drain(w:&mut World,ai:&mut AiSystem,limit:usize,work:u32)->AiReport{for _ in 0..20000{let r=run(w,ai,limit,work);if r.remaining==0{assert!(r.deferred.is_none());return r;}}panic!("wake phase did not converge")}
fn revive(w:&mut World,e:EntityId){let h=w.health(e).unwrap();let mut result=[Err(HealthError::Unconfigured)];w.apply_health_commands(&[HealthCommand::Revive{entity:e,current:n(100)}],&mut result,&mut [HealthChange{entity:e,before:h,after:h}]).unwrap();assert!(result[0].is_ok());}

#[test]
fn static_idle_world_skips_all_actor_and_query_work(){
 let mut w=World::default();for x in 0..1000{monster(&mut w,p(x*1000,0),100);}let far=player(&mut w,p(-10000,0));let mut ai=AiSystem::new(n(32));assert_eq!(drain(&mut w,&mut ai,4096,100000).sleeping,1000);
 for _ in 0..10{w.advance_tick().unwrap();let r=run(&mut w,&mut ai,4096,0);assert_eq!((r.visited,r.processed,r.remaining,r.wake_jobs),(0,0,0,0));assert_eq!(r.work,CombatQueryWork::default());}
 w.set_position(far,p(-10000,0)).unwrap();assert_eq!(run(&mut w,&mut ai,4096,0).pending_wakes,0);
}

#[test]
fn same_cell_motion_uses_exact_inclusive_circle_and_target_registration(){
 let mut w=World::default();let m=monster(&mut w,p(100,100),50);let t=player(&mut w,p(151,100));let mut ai=AiSystem::new(n(32));assert_eq!(drain(&mut w,&mut ai,4096,10000).sleeping,1);
 w.set_position(t,p(140,131)).unwrap();assert_eq!(drain(&mut w,&mut ai,4096,10000).sleeping,1);assert_eq!(w.ai_state(m).unwrap().target,None);
 w.set_position(t,p(140,130)).unwrap();let r=drain(&mut w,&mut ai,4096,10000);assert_eq!(r.woken,1);assert_eq!(w.ai_state(m).unwrap().target,Some(t));
 w.unregister_ai_target(t).unwrap();drain(&mut w,&mut ai,4096,10000);w.advance_tick().unwrap();drain(&mut w,&mut ai,4096,10000);
 let replacement=player(&mut w,p(100,100));drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(replacement));
}

#[test]
fn tier_boundaries_negative_coordinates_and_extreme_radius_do_not_miss(){
 for (origin,target,radius) in [(p(-256,-256),p(-257,-256),1),(p(-1,0),p(255,0),256),(p(256,256),p(-1,256),257),(p(i32::MIN,0),p(i32::MAX,0),u32::MAX),(p(0,0),p(0,0),0)]{
  let mut w=World::default();let m=monster(&mut w,origin,radius);let mut ai=AiSystem::new(n(32));drain(&mut w,&mut ai,4096,10000);let t=player(&mut w,target);drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(t),"{origin:?} {target:?} {radius}");
 }
}

#[test]
fn eligibility_revival_control_and_actor_relocation_wake_without_target_motion(){
 let mut w=World::default();let m=monster(&mut w,p(0,0),50);let t=player(&mut w,p(20,0));w.set_ai_target_eligible(t,false).unwrap();let mut ai=AiSystem::new(n(32));assert_eq!(drain(&mut w,&mut ai,4096,10000).sleeping,1);
 w.set_ai_target_eligible(t,true).unwrap();drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(t));
 w.apply_damage(t,100).unwrap();drain(&mut w,&mut ai,4096,10000);w.advance_tick().unwrap();assert_eq!(drain(&mut w,&mut ai,4096,10000).sleeping,1);
 revive(&mut w,t);drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(t));
 w.set_position(t,p(500,0)).unwrap();drain(&mut w,&mut ai,4096,10000);w.advance_tick().unwrap();drain(&mut w,&mut ai,4096,10000);
 w.set_ai_control_until(m,Tick(3)).unwrap();drain(&mut w,&mut ai,4096,10000);assert!(w.ai_state(m).unwrap().controlled);w.advance_tick().unwrap();drain(&mut w,&mut ai,4096,10000);assert!(!w.ai_state(m).unwrap().controlled);
 w.set_position(m,p(480,0)).unwrap();drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(t));
 w.apply_damage(m,100).unwrap();drain(&mut w,&mut ai,4096,10000);w.advance_tick().unwrap();assert_eq!(run(&mut w,&mut ai,4096,0).visited,0);revive(&mut w,m);drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(t));
}

#[test]
fn ordinary_movement_hooks_wake_sleeping_monster_and_target(){
 let mut w=World::default();let m=monster(&mut w,p(100,0),50);let t=player(&mut w,p(151,0));let mut ai=AiSystem::new(n(32));drain(&mut w,&mut ai,4096,10000);
 w.move_to(t,p(150,0),NonZeroU16::new(20).unwrap()).unwrap();w.advance_tick().unwrap();drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(t));
 let other=monster(&mut w,p(1000,0),50);drain(&mut w,&mut ai,4096,10000);w.move_to(other,p(1001,0),NonZeroU16::new(20).unwrap()).unwrap();w.advance_tick().unwrap();let r=drain(&mut w,&mut ai,4096,10000);assert!(r.processed>=2);assert_eq!(w.position(other).unwrap(),p(1001,0));
 w.move_to(other,p(1200,0),NonZeroU16::new(20).unwrap()).unwrap();w.advance_tick().unwrap();let r=drain(&mut w,&mut ai,4096,10000);assert_eq!(r.sleeping,0);assert_eq!(w.movement_status(other).unwrap(),MovementStatus::Moving);
}

#[test]
fn bonus_radius_and_damage_explicitly_wake_but_zero_damage_does_not(){
 let mut w=World::default();let m=monster(&mut w,p(0,0),100);let t=player(&mut w,p(500,0));let mut ai=AiSystem::new(n(32));drain(&mut w,&mut ai,4096,10000);
 w.add_ai_damage_threat(m,t,0).unwrap();assert_eq!(run(&mut w,&mut ai,4096,0).visited,0);
 w.set_ai_bonus_threat(m,t,10,Tick(10)).unwrap();assert_eq!(drain(&mut w,&mut ai,4096,10000).sleeping,1);
 w.set_position(t,p(200,0)).unwrap();drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(t));
 w.set_position(t,p(1000,0)).unwrap();w.add_ai_damage_threat(m,t,1).unwrap();drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(t));
}

#[test]
fn tiny_wake_budget_resumes_bucket_after_despawn_reuse_and_new_member(){
 let mut w=World::default();let actors:Vec<_>=(0..32).map(|i|monster(&mut w,p(i,0),100)).collect();let mut ai=AiSystem::new(n(32));drain(&mut w,&mut ai,4096,10000);let target=player(&mut w,p(0,0));
 let first=run(&mut w,&mut ai,1,1);assert!(first.deferred.is_some());assert_eq!(first.pending_wakes,1);
 w.despawn(actors[0]).unwrap();w.despawn(actors[20]).unwrap();let added=monster(&mut w,p(1,0),100);
 drain(&mut w,&mut ai,1,3);for &m in &actors{if m!=actors[0]&&m!=actors[20]{assert_eq!(w.ai_state(m).unwrap().target,Some(target));}}assert_eq!(w.ai_state(added).unwrap().target,Some(target));
 w.unregister_ai_actor(actors[31]).unwrap();w.set_ai_target_eligible(target,false).unwrap();drain(&mut w,&mut ai,1,3);w.advance_tick().unwrap();drain(&mut w,&mut ai,1,3);
 let t2=player(&mut w,p(2,0));drain(&mut w,&mut ai,1,3);assert_eq!(w.ai_state(added).unwrap().target,Some(t2));
}

#[test]
fn pending_target_motion_and_dense_target_swap_restart_without_stale_wakes(){
 let mut w=World::default();let a=monster(&mut w,p(0,0),30);let b=monster(&mut w,p(1000,0),30);let mut ai=AiSystem::new(n(32));drain(&mut w,&mut ai,4096,10000);
 let gone=player(&mut w,p(10,0));let moved=player(&mut w,p(20,0));assert_eq!(run(&mut w,&mut ai,1,1).pending_wakes,2);
 w.despawn(gone).unwrap();w.set_position(moved,p(1010,0)).unwrap();let reused=player(&mut w,p(2000,0));drain(&mut w,&mut ai,1,3);assert_eq!(w.ai_state(a).unwrap().target,None);assert_eq!(w.ai_state(b).unwrap().target,Some(moved));assert_ne!(gone,reused);
 w.despawn(moved).unwrap();drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(b).unwrap().target,Some(reused));
}

#[test]
fn attack_loss_gets_next_tick_idle_acquisition_before_sleeping(){
 let mut w=World::default();let m=monster(&mut w,p(0,0),100);let a=player(&mut w,p(0,0));let b=player(&mut w,p(50,0));let mut ai=AiSystem::new(n(32));drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().mode,AiMode::Attacking);
 w.set_ai_target_eligible(a,false).unwrap();let r=drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,None);assert_eq!(r.sleeping,0);
 w.advance_tick().unwrap();drain(&mut w,&mut ai,4096,10000);assert_eq!(w.ai_state(m).unwrap().target,Some(b));
}
