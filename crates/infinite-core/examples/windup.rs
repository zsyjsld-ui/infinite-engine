//! Finite windup/chase demonstration source; not executed or a test.
use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};

fn main() {
    let mut world = World::default();
    let attacker = world.spawn(GridPosition { x:0,y:0 }).unwrap();
    let target = world.spawn(GridPosition { x:64,y:0 }).unwrap();
    world.configure_health(attacker,100,NonZeroU32::new(100).unwrap()).unwrap();
    world.configure_health(target,90,NonZeroU32::new(90).unwrap()).unwrap();
    let profile = AttackProfile { range:64,damage:NonZeroU32::new(30).unwrap(),
        cooldown_ticks:NonZeroU16::new(2).unwrap(),windup_ticks:2 };
    let chase = ChaseConfig { speed:NonZeroU16::new(320).unwrap(),..ChaseConfig::default() };
    world.start_chase_attack(attacker,target,profile,chase).unwrap();
    let initial = world.health(target).unwrap();
    let mut changes = [HealthChange { entity:target,before:initial,after:initial };4];
    let mut events = [CombatEvent { attacker,target,kind:CombatEventKind::Ended(CombatEndReason::TargetGone) };4];
    for step in 1..=24 {
        if step == 2 { world.set_position(target,GridPosition { x:160,y:0 }).unwrap(); }
        if step == 4 { world.set_position(target,GridPosition { x:64,y:0 }).unwrap(); }
        if step == 6 { world.move_to(attacker,GridPosition { x:-32,y:0 },NonZeroU16::new(640).unwrap()).unwrap(); }
        if step == 8 || step == 11 { world.start_chase_attack(attacker,target,profile,chase).unwrap(); }
        world.advance_tick().unwrap();
        let report = world.resolve_combat(&mut events,&mut changes).unwrap();
        let state = world.attack_state(attacker).unwrap();
        println!("tick={} windup={:?} next_ready={} chase={:?} hp={}",report.tick.0,
            state.windup.map(|w|(w.started_at,w.hit_at)),state.next_ready.0,
            state.chase.map(|c|c.status),world.health(target).unwrap().current);
        for event in &events[..report.events_written] { println!("  {event:?}"); }
        for change in &changes[..report.changed_entities] { println!("  {change:?}"); }
    }
}
