//! Four maps share navigation admission while due periodic damage runs on time.
use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn small(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn main(){
    let mut maps=Vec::new();let mut actors=Vec::new();
    for _ in 0..4{
        let mut map=CollisionMap::new(GridPosition::default(),n(20),n(20),n(16)).unwrap();
        for y in 0..16{map.set_blocked(10,y,true).unwrap();}
        let mut world=World::with_map(DEFAULT_TICK_MS,map);
        let entity=world.spawn(GridPosition{x:24,y:24}).unwrap();
        world.configure_health(entity,100,n(100)).unwrap();
        world.apply_periodic(PeriodicEffect{source:entity,target:entity,key:1,
            kind:PeriodicKind::Damage,amount:n(3),first_delay:1,interval:small(2),
            pulses:small(4),refresh:PeriodicRefresh::Replace}).unwrap();
        world.queue_navigation(entity,GridPosition{x:280,y:24},small(1600)).unwrap();
        actors.push(entity);maps.push(world);
    }
    let mut budget=ServerWorkBudget::new(ServerBudgetConfig{maps:4,
        weighted_units_per_tick:6,lanes:[ServerLaneBudget{unit_cost:1,
        units_per_tick:2,quantum:1,max_wait_ticks:4};SERVER_WORK_LANES]}).unwrap();
    let mut tickets=[None;4];let mut served=[0;4];let mut effects=Vec::new();
    let mut feedback=Vec::new();let mut actual_work=0u64;let mut allowed_work=0u64;
    for tick in 0..48{
        budget.begin_tick(Tick(tick)).unwrap();
        // Authority runs first even if optional navigation is queued elsewhere.
        for (index,world) in maps.iter_mut().enumerate(){
            assert_eq!(world.tick(),Tick(tick));
            feedback.clear();world.resolve_effects(MAX_EFFECT_WORK,&mut effects,&mut feedback).unwrap();
            let pulses=if tick==0{0}else{((tick+1)/2).min(4)};
            assert_eq!(world.health(actors[index]).unwrap().current,100-pulses as u32*3);
            budget.record_authority(1).unwrap();
            if world.pending_navigation_count()>0&&tickets[index].is_none(){
                tickets[index]=Some(budget.request(index,ServerWorkLane::Navigation,100).unwrap());
            }else if world.pending_navigation_count()==0{
                if let Some(ticket)=tickets[index].take(){budget.cancel(ticket);}
            }
        }
        let mut allowance=[0;4];
        while let Some(grant)=budget.next_grant(){
            allowance[grant.map]+=grant.units*MIN_NAVIGATION_WORK_SLICE;
            served[grant.map]+=1;allowed_work+=u64::from(grant.units*MIN_NAVIGATION_WORK_SLICE);
            if grant.remaining==0{tickets[grant.map]=None;}
        }
        assert!(!budget.report().unwrap().over_budget);
        for (index,world) in maps.iter_mut().enumerate(){
            world.restrict_navigation_work(allowance[index]);world.advance_tick().unwrap();
            let used=world.last_navigation_budget().used();assert!(used<=allowance[index]);
            actual_work+=u64::from(used);
        }
    }
    assert!(served.iter().all(|&count|count>0));
    assert!(maps.iter().all(|world|world.pending_navigation_count()==0));
    for (world,actor) in maps.iter().zip(&actors){
        assert_eq!(world.position(*actor).unwrap(),GridPosition{x:280,y:24});
        assert_eq!(world.health(*actor).unwrap().current,88);
    }
    println!("maps=4 ticks=48 grants={served:?} navigation_work={actual_work} admitted_work={allowed_work} due_damage_on_time=true all_arrived=true");
}
