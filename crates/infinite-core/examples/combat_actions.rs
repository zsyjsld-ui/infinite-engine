//! Query -> acquire/chase -> area impact -> health -> IC v3 feedback.
//! Implementation example, not a verified test or an authoritative skill API.
use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};

fn n(value: u32) -> NonZeroU32 { NonZeroU32::new(value).unwrap() }
fn p(x: i32, y: i32) -> GridPosition { GridPosition { x, y } }

fn main() {
    let mut map = CollisionMap::new(p(0, 0), n(8), n(8), n(16)).unwrap();
    map.set_blocked(3, 3, true).unwrap();
    let mut world = World::with_map(DEFAULT_TICK_MS, map);
    let caster = world.spawn(p(24, 56)).unwrap();
    let near = world.spawn(p(40, 56)).unwrap();
    let side = world.spawn(p(24, 88)).unwrap();
    let behind_wall = world.spawn(p(88, 56)).unwrap();
    let ally = world.spawn(p(24, 72)).unwrap();
    for entity in [caster, near, side, behind_wall, ally] {
        world.configure_health(entity, if entity == near { 25 } else { 100 }, n(100)).unwrap();
    }
    // App-owned allegiance/materialization rules, not engine entity kinds.
    let enemies = [near, side, behind_wall];
    let profile = AttackProfile { range: 32, damage: n(5),
        cooldown_ticks: NonZeroU16::new(2).unwrap(), windup_ticks: 2 };
    let mut actions = CombatActions::new(n(32));
    let mut events = Vec::with_capacity(MAX_ACTIVE_COMBATANTS);
    // This example has at most one ordinary active attacker. Reserve its slot
    // before accepting area impacts so the final capture cannot exceed 4096.
    let placeholder = CombatEvent { attacker: caster, target: near,
        kind: CombatEventKind::Ended(CombatEndReason::TargetGone) };
    let mut ordinary_events = [placeholder; 1];
    let health = world.health(near).unwrap();
    let mut ordinary_changes = [HealthChange { entity: near, before: health, after: health }; 1];
    let mut frame = CombatFrame::new(n(32));
    let mut sender = CombatSyncSender::new_batched(21, CombatSyncConfig::default()).unwrap();
    let mut replica = CombatReplica::new(21);
    let view = ViewRect { min: p(0, 0), max: p(128, 128) };

    for step in 1..=4 {
        world.advance_tick().unwrap();
        // All feedback from the preceding tick has already been observed below.
        events.clear();
        let mut budget = CombatQueryBudget::new(4096);
        {
            let mut phase = actions.begin(&mut world, &mut budget, &mut events,
                MAX_ACTIVE_COMBATANTS - ordinary_events.len()).unwrap();
            if step == 1 || step == 3 {
                let selected = phase.acquire_attack(AcquireAttack {
                    attacker: caster, search_radius: 80, require_sight: true,
                    profile, chase: Some(ChaseConfig::default()),
                }, |_, entity| enemies.contains(&entity)).unwrap();
                println!("tick={step} selected={:?}", selected.target);
            }
            if step == 2 {
                // Two trusted impacts in call order. A later impact observes
                // the preceding impact's live HP, including any deaths.
                for damage in [20, 10, 10] {
                    let report = phase.damage_area(AreaDamage {
                        attacker: caster,
                        area: CombatArea::Circle { center: p(24, 56), radius: 80 },
                        damage: n(damage), require_sight: true,
                    }, |_, entity| enemies.contains(&entity)).unwrap();
                    println!("tick={step} area={report:?}");
                }
            }
        }
        let combat = world.resolve_combat(&mut ordinary_events, &mut ordinary_changes).unwrap();
        events.extend_from_slice(&ordinary_events[..combat.events_written]);
        frame.capture(&world, &events).unwrap();
        sender.observe(&frame, view).unwrap();
        while let Some(packet) = sender.prepare().unwrap() {
            let bytes = packet.len();
            let applied = replica.apply(packet).unwrap();
            println!("tick={step} bytes={bytes} feedback={}", applied.events);
            for event in replica.drain_feedback() { println!("  {event:?}"); }
            sender.acknowledge_packet(&applied.acknowledgement()).unwrap();
        }
        println!("tick={step} near={:?} side={:?} wall={:?} ally={:?} work={:?}",
            world.health(near), world.health(side), world.health(behind_wall),
            world.health(ally), budget.work());
    }
}
