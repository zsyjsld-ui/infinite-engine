use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn main(){
    let map=CollisionMap::new(GridPosition{x:-32,y:-32},n(20),n(20),n(16)).unwrap();let mut world=World::with_map(DEFAULT_TICK_MS,map);
    let monster=world.spawn(GridPosition::default()).unwrap();let player=world.spawn(GridPosition{x:80,y:0}).unwrap();
    for id in[monster,player]{world.configure_health(id,100,n(100)).unwrap();}
    world.register_ai_target(player,true).unwrap();world.register_ai_actor(monster,AiProfile{aggro_radius:100,leash_radius:300,bonus_radius:200,attack:AttackProfile{range:10,damage:n(7),cooldown_ticks:t(3),windup_ticks:2},attack_break_range:30,chase:ChaseConfig{speed:t(320),..ChaseConfig::default()}}).unwrap();
    let mut ai=AiSystem::new(n(32));let mut decisions=Vec::new();let mut hit=false;
    for _ in 0..30{
        world.step(&[],&mut [],&[],&mut []).unwrap();let report=ai.resolve(&mut world,64,&mut CombatQueryBudget::new(4096),&mut decisions).unwrap();assert_eq!(report.remaining,0);assert!(report.deferred.is_none());
        for decision in &decisions{println!("tick {} AI: {:?} {:?}",world.tick().0,decision.reason,decision.after);}
        let h=world.health(player).unwrap();let mut events=[CombatEvent{attacker:monster,target:player,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}];let mut changes=[HealthChange{entity:player,before:h,after:h}];let combat=world.resolve_combat(&mut events,&mut changes).unwrap();
        if combat.events_written>0{println!("tick {} combat: {:?}",world.tick().0,&events[..combat.events_written]);}
        if combat.hits>0{hit=true;break;}
    }
    assert!(hit);assert_eq!(world.health(player).unwrap().current,93);println!("reached {:?}, HP={}",world.position(monster).unwrap(),world.health(player).unwrap().current);
}
