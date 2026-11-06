use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};

fn main() {
    let mut world = World::default();
    let attacker = world.spawn(GridPosition { x:0, y:0 }).unwrap();
    let target = world.spawn(GridPosition { x:96, y:0 }).unwrap();
    world.configure_health(attacker,100,NonZeroU32::new(100).unwrap()).unwrap();
    world.configure_health(target,90,NonZeroU32::new(90).unwrap()).unwrap();
    let profile = AttackProfile { range:64, damage:NonZeroU32::new(30).unwrap(),
        cooldown_ticks:NonZeroU16::new(3).unwrap(), windup_ticks:0 };
    world.start_attack(attacker,target,profile).unwrap();
    let initial = world.health(target).unwrap();
    let mut events = [CombatEvent { attacker,target,kind:CombatEventKind::Ended(CombatEndReason::TargetGone) };4];
    let mut changes = [HealthChange { entity:target,before:initial,after:initial };4];
    for step in 1..=9 {
        if step == 2 || step == 9 { world.set_position(target,GridPosition { x:64, y:0 }).unwrap(); }
        if step == 3 {
            world.stop_attack(attacker).unwrap();
            world.start_attack(attacker,target,profile).unwrap(); // Preserve existing cooldown.
        }
        if step == 6 { world.set_position(target,GridPosition { x:65, y:0 }).unwrap(); }
        world.advance_tick().unwrap();
        let report = world.resolve_combat(&mut events,&mut changes).unwrap();
        println!("tick={} active={} hits={} cooldown={} out_of_range={} ended={} hp={}",
            report.tick.0,report.remaining_active,report.hits,report.cooling_down,
            report.out_of_range,report.ended,world.health(target).unwrap().current);
        for event in &events[..report.events_written] { println!("  event: {event:?}"); }
        for change in &changes[..report.changed_entities] { println!("  final health: {change:?}"); }
    }
    // Real application: consume ordered events for rewards/death handling, then
    // capture outgoing motion/health. This example has no clock or network loop.
}
