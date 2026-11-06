//! Idle sleep -> same-tick region wake -> real navigation and attack.
use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn p(x:i32,y:i32)->GridPosition{GridPosition{x,y}}
fn resolve(w:&mut World,ai:&mut AiSystem)->AiReport{
    let report=ai.resolve(w,4096,&mut CombatQueryBudget::new(100000),&mut Vec::new()).unwrap();
    assert_eq!(report.remaining,0);assert!(report.deferred.is_none());report
}
fn main(){
    let map=CollisionMap::new(p(-32,-32),n(50),n(10),n(16)).unwrap();
    let mut w=World::with_map(DEFAULT_TICK_MS,map);
    let monster=w.spawn(p(0,0)).unwrap();let player=w.spawn(p(512,0)).unwrap();
    for entity in [monster,player]{w.configure_health(entity,100,n(100)).unwrap();}
    w.register_ai_actor(monster,AiProfile{aggro_radius:64,leash_radius:192,bonus_radius:128,
        attack:AttackProfile{range:1,damage:n(7),cooldown_ticks:t(3),windup_ticks:2},attack_break_range:1,
        chase:ChaseConfig{speed:t(320),..ChaseConfig::default()}}).unwrap();
    w.register_ai_target(player,true).unwrap();let mut ai=AiSystem::new(n(32));
    let first=resolve(&mut w,&mut ai);assert_eq!(first.sleeping,1);
    w.advance_tick().unwrap();let idle=resolve(&mut w,&mut ai);assert_eq!(idle.visited,0);
    println!("idle tick={} sleeping={} actor_visits={} query_work={:?}",w.tick().0,idle.sleeping,idle.visited,idle.work);
    w.set_position(player,p(64,0)).unwrap();let wake=resolve(&mut w,&mut ai);
    assert_eq!(wake.woken,1);assert_eq!(w.ai_state(monster).unwrap().target,Some(player));
    println!("wake tick={} woken={} target={:?}",w.tick().0,wake.woken,w.ai_state(monster).unwrap().target);
    let health=w.health(player).unwrap();let mut changes=[HealthChange{entity:player,before:health,after:health}];
    let mut events=[CombatEvent{attacker:monster,target:player,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}];
    for _ in 0..20{
        w.step(&[],&mut [],&[],&mut []).unwrap();resolve(&mut w,&mut ai);
        let combat=w.resolve_combat(&mut events,&mut changes).unwrap();
        if combat.hits>0{assert_eq!(w.health(player).unwrap().current,93);
            println!("hit tick={} monster={:?} player_hp=93",w.tick().0,w.position(monster).unwrap());return;}
    }
    panic!("woken monster did not finish real navigation and attack");
}
