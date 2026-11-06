use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};

fn n(v: u32) -> NonZeroU32 { NonZeroU32::new(v).unwrap() }
fn p(x: i32, y: i32) -> GridPosition { GridPosition { x, y } }
fn spawn(w: &mut World, pos: GridPosition, hp: u32) -> EntityId {
    let id = w.spawn(pos).unwrap(); w.configure_health(id, hp, n(hp.max(1))).unwrap(); id
}
fn impact(attacker: EntityId, damage: u32, radius: u32) -> AreaDamage {
    AreaDamage { attacker, area: CombatArea::Circle { center: p(0, 0), radius },
        damage: n(damage), require_sight: false }
}
fn acquire(attacker: EntityId) -> AcquireAttack {
    AcquireAttack { attacker, search_radius: 80, require_sight: false,
        profile: AttackProfile { range: 32, damage: n(5), cooldown_ticks: NonZeroU16::new(5).unwrap(), windup_ticks: 2 },
        chase: Some(ChaseConfig::default()) }
}
fn placeholder(a: EntityId, b: EntityId) -> CombatEvent {
    CombatEvent { attacker: a, target: b, kind: CombatEventKind::Ended(CombatEndReason::TargetGone) }
}

#[test]
fn sequential_impacts_match_independent_health_and_order_reference() {
    let mut w = World::default(); let caster = spawn(&mut w, p(0, 0), 100);
    let ids: Vec<_> = (0..80).map(|i| spawn(&mut w, p((i % 10)*13-50, (i/10)*11-35), (i%7+1) as u32*9)).collect();
    let mut expected: Vec<_> = ids.iter().map(|&id| w.health(id).unwrap().current).collect();
    let positions: Vec<_> = ids.iter().map(|&id| w.position(id).unwrap()).collect();
    let mut reference = Vec::new(); let mut events = Vec::new();
    let mut actions = CombatActions::new(n(32)); let mut budget = CombatQueryBudget::new(10000);
    {
        let mut phase = actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap();
        for damage in [11, 20, 100] {
            let mut count = 0; let mut deaths = 0;
            for (i, &id) in ids.iter().enumerate() {
                let point = positions[i];
                if id.index%3 == 0 || expected[i] == 0 || i64::from(point.x).pow(2)+i64::from(point.y).pow(2)>45*45 { continue; }
                let before = expected[i]; expected[i] = before.saturating_sub(damage);
                count += 1; deaths += usize::from(expected[i] == 0);
                reference.push((id, before, expected[i]));
            }
            let report = phase.damage_area(impact(caster, damage, 45), |_, id| id.index%3 != 0).unwrap();
            assert_eq!((report.hits, report.depleted), (count, deaths));
        }
    }
    let actual: Vec<_> = events.iter().map(|e| match e.kind {
        CombatEventKind::Hit { health, .. } => (e.target, health.before.current, health.after.current),
        _ => panic!("not a hit"),
    }).collect();
    assert_eq!(actual, reference);
    for (id, hp) in ids.into_iter().zip(expected) { assert_eq!(w.health(id).unwrap().current, hp); }
    assert_eq!(w.health(caster).unwrap().current, 100);
}

#[test]
fn query_and_event_limits_are_atomic_but_do_not_refund_work() {
    let mut w = World::default(); let a = spawn(&mut w, p(0, 0), 100);
    let b = spawn(&mut w, p(1, 0), 100); let c = spawn(&mut w, p(2, 0), 100);
    let prefix = placeholder(a, b); let mut events = vec![prefix];
    let mut actions = CombatActions::new(n(16)); let mut budget = CombatQueryBudget::new(1);
    {
        let mut phase = actions.begin(&mut w, &mut budget, &mut events, 3).unwrap();
        assert_eq!(phase.damage_area(impact(a, 10, 5), |_, _| true), Err(CombatActionError::Query(CombatQueryError::WorkLimitExceeded)));
    }
    assert_eq!(budget.used(), 1); assert_eq!(events, vec![prefix]);
    let mut budget = CombatQueryBudget::new(100);
    {
        let mut phase = actions.begin(&mut w, &mut budget, &mut events, 2).unwrap();
        assert_eq!(phase.damage_area(impact(a, 10, 5), |_, _| true), Err(CombatActionError::EventLimitExceeded));
        let ok = phase.damage_area(impact(a, 10, 5), |_, id| id == b).unwrap();
        assert_eq!((ok.offset, ok.hits), (1, 1));
    }
    assert!(budget.used() > 0); assert_eq!(events[0], prefix);
    assert_eq!(w.health(b).unwrap().current, 90); assert_eq!(w.health(c).unwrap().current, 100);
    assert!(matches!(actions.begin(&mut w, &mut budget, &mut events, 1), Err(CombatActionError::EventLimitExceeded)));
}

#[test]
fn recapture_same_tick_handles_positions_generations_and_different_worlds() {
    let mut w = World::default(); let a = spawn(&mut w, p(0, 0), 100); let old = spawn(&mut w, p(1, 0), 10);
    let mut actions = CombatActions::new(n(16)); let mut events = Vec::new(); let mut budget = CombatQueryBudget::new(1000);
    actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(impact(a, 1, 2), |_, _| true).unwrap();
    w.despawn(old).unwrap(); let replacement = spawn(&mut w, p(100, 0), 10);
    assert_ne!(old, replacement);
    assert_eq!(actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(impact(a, 1, 2), |_, _| true).unwrap().hits, 0);
    w.set_position(replacement, p(1, 0)).unwrap();
    actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(impact(a, 1, 2), |_, _| true).unwrap();
    assert_eq!(events.last().unwrap().target, replacement); assert_eq!(w.health(replacement).unwrap().current, 9);
    let mut other = World::default(); let a2 = spawn(&mut other, p(0, 0), 100); let b2 = spawn(&mut other, p(90, 0), 100);
    assert_eq!(actions.begin(&mut other, &mut budget, &mut events, 4096).unwrap().damage_area(impact(a2, 1, 2), |_, _| true).unwrap().hits, 0);
    assert_eq!(other.health(b2).unwrap().current, 100);
}

#[test]
fn acquire_preserves_windup_cooldown_on_repeat_and_intent_on_no_match_or_error() {
    let mut w = World::default(); let a = spawn(&mut w, p(0, 0), 100); let b = spawn(&mut w, p(16, 0), 100);
    let mut actions = CombatActions::new(n(32)); let mut events = Vec::new(); let mut budget = CombatQueryBudget::new(10000);
    let result = actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().acquire_attack(acquire(a), |_, _| true).unwrap();
    assert_eq!(result.target.unwrap().entity, b);
    let mut regular = [placeholder(a, b)]; let h = w.health(b).unwrap(); let mut changes = [HealthChange { entity: b, before: h, after: h }];
    w.resolve_combat(&mut regular, &mut changes).unwrap(); let before = w.attack_state(a).unwrap(); assert!(before.windup.is_some());
    {
        let mut phase = actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap();
        phase.acquire_attack(acquire(a), |_, _| true).unwrap();
        assert!(phase.acquire_attack(acquire(a), |_, _| false).unwrap().target.is_none());
    }
    assert_eq!(w.attack_state(a).unwrap(), before);
    let mut none = CombatQueryBudget::new(0);
    assert_eq!(actions.begin(&mut w, &mut none, &mut events, 4096).unwrap().acquire_attack(acquire(a), |_, _| true), Err(CombatActionError::Query(CombatQueryError::WorkLimitExceeded)));
    assert_eq!(w.attack_state(a).unwrap(), before);
}

#[test]
fn area_sight_uses_caster_not_ground_center_and_missing_map_fails() {
    let mut map = CollisionMap::new(p(0, 0), n(8), n(8), n(16)).unwrap(); map.set_blocked(3, 3, true).unwrap();
    let mut w = World::with_map(DEFAULT_TICK_MS, map);
    let a = spawn(&mut w, p(24, 56), 100); let b = spawn(&mut w, p(88, 56), 100);
    let mut actions = CombatActions::new(n(32)); let mut events = Vec::new(); let mut budget = CombatQueryBudget::new(1000);
    let request = AreaDamage { attacker: a, area: CombatArea::Circle { center: p(88, 56), radius: 0 }, damage: n(7), require_sight: true };
    assert_eq!(actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(request, |_, _| true).unwrap().hits, 0);
    actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(AreaDamage { require_sight: false, ..request }, |_, _| true).unwrap();
    assert_eq!(w.health(b).unwrap().current, 93);
    let mut no_map = World::default(); let a = spawn(&mut no_map, p(0, 0), 100); let b = spawn(&mut no_map, p(1, 0), 100);
    assert_eq!(actions.begin(&mut no_map, &mut budget, &mut events, 4096).unwrap().damage_area(AreaDamage { require_sight: true, ..impact(a, 7, 2) }, |_, _| true), Err(CombatActionError::Query(CombatQueryError::MapRequired)));
    assert_eq!(no_map.health(b).unwrap().current, 100);
}

#[test]
fn bounded_attempts_and_dead_caster_do_not_create_hits() {
    let mut w = World::default(); let a = spawn(&mut w, p(0, 0), 100); let dead = spawn(&mut w, p(1, 0), 0);
    let mut actions = CombatActions::new(n(16)); let mut events = Vec::new(); let mut budget = CombatQueryBudget::new(100000);
    {
        let mut phase = actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap();
        assert_eq!(phase.damage_area(impact(dead, 1, 1), |_, _| true), Err(CombatActionError::Attack(AttackError::AttackerDepleted)));
        for _ in 1..MAX_COMBAT_QUERIES { assert_eq!(phase.damage_area(impact(a, 1, 1), |_, _| true).unwrap().hits, 0); }
        assert_eq!(phase.damage_area(impact(a, 1, 1), |_, _| true), Err(CombatActionError::TooManyActions));
    }
    assert!(events.is_empty()); assert_eq!(w.health(a).unwrap().current, 100);
}

#[test]
fn hit_limit_rejects_entire_dense_impact() {
    let mut w = World::default(); let a = spawn(&mut w, p(0, 0), 100);
    let ids: Vec<_> = (0..4097).map(|_| spawn(&mut w, p(0, 0), 100)).collect();
    let mut actions = CombatActions::new(n(16)); let mut events = Vec::new(); let mut budget = CombatQueryBudget::new(10000);
    assert_eq!(actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(impact(a, u32::MAX, 0), |_, _| true), Err(CombatActionError::Query(CombatQueryError::HitLimitExceeded)));
    assert!(events.is_empty()); for id in &ids { assert_eq!(w.health(*id).unwrap().current, 100); }
    let mut budget = CombatQueryBudget::new(10000);
    let result = actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(impact(a, u32::MAX, 0), |_, id| id != ids[4096]).unwrap();
    assert_eq!((result.hits, result.depleted, events.len()), (4096, 4096, 4096));
    assert_eq!(w.health(ids[4096]).unwrap().current, 100);
}

#[test]
fn actual_impacts_survive_v3_backpressure_and_ack_retry_without_reapplication() {
    let mut w = World::default(); let a = spawn(&mut w, p(0, 0), 100); let b = spawn(&mut w, p(1, 0), 20); let c = spawn(&mut w, p(2, 0), 30);
    let mut actions = CombatActions::new(n(16)); let mut events = Vec::new(); let mut budget = CombatQueryBudget::new(1000);
    let mut frame = CombatFrame::new(n(16));
    let mut sender = CombatSyncSender::new_batched(1, CombatSyncConfig { pending_events: 2, events_per_packet: 1 }).unwrap();
    let mut replica = CombatReplica::new(1); let view = ViewRect { min: p(-16, -16), max: p(16, 16) };
    actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(impact(a, 10, 5), |_, _| true).unwrap();
    frame.capture(&w, &events).unwrap(); sender.observe(&frame, view).unwrap();
    let bytes = sender.prepare().unwrap().unwrap().to_vec(); let first = replica.apply(&bytes).unwrap();
    let mut feedback: Vec<_> = replica.drain_feedback().collect();
    assert!(replica.apply(&bytes).unwrap().duplicate); assert_eq!(replica.drain_feedback().count(), 0);
    w.advance_tick().unwrap(); events.clear();
    actions.begin(&mut w, &mut budget, &mut events, 4096).unwrap().damage_area(impact(a, 1, 5), |_, _| true).unwrap();
    frame.capture(&w, &events).unwrap();
    assert_eq!(sender.observe(&frame, view), Err(CombatSyncError::EventBackpressure));
    assert_eq!(sender.pending_packet().unwrap(), bytes.as_slice());
    sender.acknowledge_packet(&first.acknowledgement()).unwrap();
    while let Some(packet) = sender.prepare().unwrap() { let r = replica.apply(packet).unwrap(); feedback.extend(replica.drain_feedback()); sender.acknowledge_packet(&r.acknowledgement()).unwrap(); }
    sender.observe(&frame, view).unwrap();
    while let Some(packet) = sender.prepare().unwrap() { let r = replica.apply(packet).unwrap(); feedback.extend(replica.drain_feedback()); sender.acknowledge_packet(&r.acknowledgement()).unwrap(); }
    assert_eq!(feedback.iter().map(|e| (e.tick, e.target, e.kind)).collect::<Vec<_>>(), vec![
        (Tick(0), b, CombatFeedbackKind::Hit { damage: 10, remaining: 10 }),
        (Tick(0), c, CombatFeedbackKind::Hit { damage: 10, remaining: 20 }),
        (Tick(1), b, CombatFeedbackKind::Hit { damage: 1, remaining: 9 }),
        (Tick(1), c, CombatFeedbackKind::Hit { damage: 1, remaining: 19 }),
    ]);
    assert_eq!(w.health(b).unwrap().current, 9); assert_eq!(w.health(c).unwrap().current, 19);
}
