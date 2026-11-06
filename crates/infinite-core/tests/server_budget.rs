use infinite_core::*;
fn config(maps: usize, total: u64) -> ServerBudgetConfig {
    ServerBudgetConfig { maps, weighted_units_per_tick: total,
        lanes: [ServerLaneBudget { unit_cost: 1, units_per_tick: 100,
            quantum: 2, max_wait_ticks: 2 }; SERVER_WORK_LANES] }
}
#[test]
fn fairness_survives_tick_reset_and_busy_map_replenishment() {
    let mut budget=ServerWorkBudget::new(config(3,2)).unwrap();
    budget.begin_tick(Tick(0)).unwrap();
    for map in 0..3 { budget.request(map,ServerWorkLane::Navigation,2).unwrap(); }
    for tick in 0..12 {
        if tick>0 { budget.begin_tick(Tick(tick)).unwrap(); }
        let grant=budget.next_grant().unwrap();assert_eq!(grant.map,tick as usize%3);
        assert_eq!(grant.units,2);assert!(budget.next_grant().is_none());
        budget.request(grant.map,grant.lane,2).unwrap();
        assert!(!budget.report().unwrap().over_budget);
    }
}
#[test]
fn cheap_work_cannot_starve_expensive_lanes_across_fragmented_ticks() {
    let mut c=config(4,2000);c.lanes[0].unit_cost=1000;c.lanes[0].quantum=4;
    c.lanes[3].units_per_tick=1000;c.lanes[3].quantum=24;
    let mut budget=ServerWorkBudget::new(c).unwrap();let mut navigation=[0u32;4];
    for tick in 0..80 {
        budget.begin_tick(Tick(tick)).unwrap();budget.record_authority(64).unwrap();
        for map in 0..4 { for (lane,units) in [(ServerWorkLane::Navigation,4),(ServerWorkLane::Network,240)] {
            match budget.request(map,lane,units) {Ok(_)|Err(ServerBudgetError::AlreadyPending)=>(),other=>panic!("{other:?}")}
        }}
        while let Some(grant)=budget.next_grant(){if grant.lane==ServerWorkLane::Navigation{navigation[grant.map]+=grant.units;}}
        assert!(!budget.report().unwrap().over_budget);
    }
    assert_eq!(navigation,[20;4]);
}
#[test]
fn weighted_total_and_lane_cap_are_shared_between_maps() {
    let mut c=config(2,17);c.lanes[0].unit_cost=3;c.lanes[0].units_per_tick=3;
    let mut b=ServerWorkBudget::new(c).unwrap();b.begin_tick(Tick(1)).unwrap();
    b.record_authority(5).unwrap();
    for map in 0..2 {b.request(map,ServerWorkLane::Navigation,10).unwrap();b.request(map,ServerWorkLane::Network,10).unwrap();}
    let mut granted=0;while let Some(g)=b.next_grant(){granted+=g.weighted_units;}
    let r=b.report().unwrap();assert_eq!(granted,12);assert_eq!(r.lane_units[0],3);
    assert_eq!(r.lane_units[3],3);assert!(!r.over_budget);
}
#[test]
fn authority_overload_and_age_are_reported_without_dropping_work() {
    let mut b=ServerWorkBudget::new(config(1,2)).unwrap();b.begin_tick(Tick(0)).unwrap();
    let ticket=b.request(0,ServerWorkLane::Persistence,3).unwrap();
    b.begin_tick(Tick(5)).unwrap();b.record_authority(7).unwrap();
    assert!(b.next_grant().is_none());let r=b.report().unwrap();
    assert!(r.over_budget);assert_eq!(r.overdue,1);assert_eq!(r.oldest_wait_ticks,5);
    b.begin_tick(Tick(6)).unwrap();assert_eq!(b.next_grant().unwrap().remaining,1);
    assert!(b.cancel(ticket));assert_eq!(b.report().unwrap().pending,0);
}
#[test]
fn stale_cancel_duplicate_and_double_reset_cannot_erase_or_refill_work() {
    let mut b=ServerWorkBudget::new(config(1,2)).unwrap();
    assert_eq!(b.request(0,ServerWorkLane::Spawn,1),Err(ServerBudgetError::TickNotStarted));
    b.begin_tick(Tick(1)).unwrap();let old=b.request(0,ServerWorkLane::Spawn,1).unwrap();
    assert_eq!(b.request(0,ServerWorkLane::Spawn,5),Err(ServerBudgetError::AlreadyPending));
    b.next_grant().unwrap();let new=b.request(0,ServerWorkLane::Spawn,2).unwrap();
    assert!(!b.cancel(old));assert_eq!(b.begin_tick(Tick(1)),Err(ServerBudgetError::TimeNotAdvanced));
    assert_eq!(b.next_grant().unwrap().units,1);assert!(b.next_grant().is_none());
    assert!(b.cancel(new));assert!(!b.cancel(new));
}
#[test]
fn rejects_impossible_configuration_and_overflow_atomically() {
    let mut c=config(1,1);c.lanes[0].unit_cost=2;
    assert!(matches!(ServerWorkBudget::new(c),Err(ServerBudgetError::InvalidConfig)));
    let mut b=ServerWorkBudget::new(config(1,1)).unwrap();b.begin_tick(Tick(u64::MAX)).unwrap();
    b.record_authority(u64::MAX).unwrap();
    assert_eq!(b.record_authority(1),Err(ServerBudgetError::AccountingOverflow));
    assert_eq!(b.report().unwrap().authority_units,u64::MAX);
    assert_eq!(b.request(1,ServerWorkLane::Spawn,1),Err(ServerBudgetError::InvalidMap));
    assert_eq!(b.request(0,ServerWorkLane::Spawn,0),Err(ServerBudgetError::InvalidUnits));
    assert_eq!(b.report().unwrap().pending,0);
}

#[test]
fn navigation_cap_cannot_be_refilled_and_resets_only_on_advance() {
    let mut world=World::default();let configured=world.navigation_budget().remaining();
    assert_eq!(world.restrict_navigation_work(17),17);
    assert_eq!(world.restrict_navigation_work(u32::MAX),17);
    assert_eq!(world.restrict_navigation_work(0),0);
    world.advance_tick().unwrap();assert_eq!(world.last_navigation_budget().limit(),0);
    assert_eq!(world.navigation_budget().remaining(),configured);
}

#[test]
fn cancellation_ticket_is_bound_to_its_scheduler() {
    let mut first=ServerWorkBudget::new(config(1,2)).unwrap();
    let mut second=ServerWorkBudget::new(config(1,2)).unwrap();
    first.begin_tick(Tick(0)).unwrap();second.begin_tick(Tick(0)).unwrap();
    let a=first.request(0,ServerWorkLane::Network,1).unwrap();
    let b=second.request(0,ServerWorkLane::Network,1).unwrap();
    assert!(!second.cancel(a));assert!(!first.cancel(b));
    assert_eq!(second.next_grant().unwrap().ticket,b);
    assert!(first.cancel(a));
}
