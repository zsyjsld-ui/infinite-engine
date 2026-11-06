//! Source-only chase demonstration. Run only when requested, not a benchmark.
use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};

fn centre(x:i32,y:i32) -> GridPosition { GridPosition { x:x*64+32,y:y*64+32 } }
fn main() {
    let mut map = CollisionMap::new(GridPosition{x:0,y:0},NonZeroU32::new(20).unwrap(),
        NonZeroU32::new(14).unwrap(),NonZeroU32::new(64).unwrap()).unwrap();
    for y in 0..11 { map.set_blocked(9,y,true).unwrap(); }
    let navigation = NavigationConfig { work_per_tick:8192,work_per_slice:MIN_NAVIGATION_WORK_SLICE,
        queued_work_reserve:MIN_NAVIGATION_WORK_SLICE,queue_capacity:4,max_wait_ticks:NonZeroU32::new(60) };
    let mut world = World::with_map_and_navigation_config(DEFAULT_TICK_MS,map,navigation).unwrap();
    let attacker = world.spawn(centre(2,3)).unwrap();
    let target = world.spawn(centre(17,3)).unwrap();
    world.configure_health(attacker,100,NonZeroU32::new(100).unwrap()).unwrap();
    world.configure_health(target,120,NonZeroU32::new(120).unwrap()).unwrap();
    let profile = AttackProfile { range:96,damage:NonZeroU32::new(20).unwrap(),cooldown_ticks:NonZeroU16::new(4).unwrap(),windup_ticks:0 };
    let chase = ChaseConfig { speed:NonZeroU16::new(640).unwrap(),..ChaseConfig::default() };
    world.start_chase_attack(attacker,target,profile,chase).unwrap();
    let initial = world.health(target).unwrap();
    let mut changes = [HealthChange{entity:target,before:initial,after:initial};4];
    let mut events = [CombatEvent{attacker,target,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};4];
    let mut previous = None;
    for step in 1..=150 {
        if step == 30 { world.set_position(target,centre(17,8)).unwrap(); }
        world.advance_tick().unwrap(); // Existing queue consumes the world's shared budget here.
        let report = world.resolve_combat(&mut events,&mut changes).unwrap();
        let state = world.attack_state(attacker).unwrap();
        let status = state.chase.map(|c|c.status);
        if status != previous || report.events_written != 0 || step%10 == 0 {
            println!("tick={} pos={:?} chase={status:?} queued={} nav_work={} requests={} ready={} target_hp={}",
                report.tick.0,world.position(attacker).unwrap(),world.pending_navigation_count(),
                world.last_navigation_progress().work_used,report.chase_requests,state.next_ready.0,world.health(target).unwrap().current);
            for event in &events[..report.events_written] { println!("  {event:?}"); }
            for change in &changes[..report.changed_entities] { println!("  {change:?}"); }
        }
        previous = status;
    }
}
