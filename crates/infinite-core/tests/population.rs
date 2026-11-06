use infinite_core::*;use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn entity(w:&mut World)->EntityId{let e=w.spawn(p(0)).unwrap();w.configure_health(e,100,n(100)).unwrap();e}
fn template()->SpawnTemplate{SpawnTemplate{maximum_health:n(100),corpse_ticks:3,respawn_ticks:5,ai:None}}
fn advance(w:&mut World,ticks:usize){for _ in 0..ticks{w.advance_tick().unwrap();}}
fn spawn(w:&mut World,species:u32,count:usize)->(SpawnGroupId,Vec<EntityId>){let mut out=Vec::new();let group=w.spawn_group(species,None,template(),&(0..count).map(|i|p(i as i32*16)).collect::<Vec<_>>(),&mut SpawnBudget::new(4096,256).unwrap(),&mut out).unwrap();(group,out)}

#[test]
fn last_living_member_releases_group_and_corpse_removal_cannot_restart_cooldown(){
 let mut w=World::default();w.resize_population(1).unwrap();let (group,ids)=spawn(&mut w,1,3);assert_eq!(w.population_state().alive,1);assert_eq!(w.spawn_group_of(ids[0]).unwrap(),Some(group));
 w.apply_damage(ids[0],100).unwrap();advance(&mut w,2);w.apply_damage(ids[1],100).unwrap();assert_eq!(w.population_state().alive,1);w.set_spawn_group_cooldown(group,7).unwrap();advance(&mut w,1);w.apply_damage(ids[2],100).unwrap();
 assert_eq!(w.population_state().alive,0);assert_eq!(w.population_state().next_due,Some(Tick(10)));assert_eq!(w.population_state().cooling_down,1);assert_eq!(w.reserve_spawn(1),Err(PopulationError::NoVacancy));
 for _ in 0..4{w.resolve_lifecycle(4096,&mut Vec::new()).unwrap();w.advance_tick().unwrap();}assert_eq!(w.len(),0);assert_eq!(w.population_state().next_due,Some(Tick(10)));advance(&mut w,3);assert_eq!(w.population_state().ready,1);assert!(w.reserve_spawn(1).is_ok());
}

#[test]
fn shrink_grow_preserves_cooldowns_and_retires_vacancies_before_living_groups(){
 let mut w=World::default();w.resize_population(3).unwrap();let (_,a)=spawn(&mut w,1,1);let (_,b)=spawn(&mut w,1,1);let reserved=w.reserve_spawn(2).unwrap();w.resize_population(1).unwrap();assert_eq!(w.cancel_spawn_reservation(reserved),Err(PopulationError::StaleReservation));assert_eq!((w.population_state().enabled,w.population_state().alive),(2,2));
 w.apply_damage(a[0],100).unwrap();assert_eq!(w.population_state().enabled,1);w.resize_population(2).unwrap();assert_eq!(w.population_state().cooling_down,1);assert_eq!(w.population_state().ready,0);advance(&mut w,3);w.apply_damage(b[0],100).unwrap();w.resize_population(0).unwrap();w.resize_population(3).unwrap();let state=w.population_state();assert_eq!((state.cooling_down,state.ready),(2,1));advance(&mut w,2);assert_eq!((w.population_state().cooling_down,w.population_state().ready),(1,2));advance(&mut w,3);assert_eq!(w.population_state().ready,3);
}

#[test]
fn reservation_generation_species_backoff_and_other_slots_remain_independent(){
 let mut w=World::default();w.resize_population(3).unwrap();let boss=w.reserve_spawn(10).unwrap();w.defer_spawn_species(10,n(5)).unwrap();assert_eq!(w.reserve_spawn(10),Err(PopulationError::SpeciesCoolingDown));let regular=w.reserve_spawn(20).unwrap();w.defer_spawn_species(20,n(2)).unwrap();w.cancel_spawn_reservation(regular).unwrap();
 let fresh=w.reserve_spawn(30).unwrap();assert_eq!(fresh.slot,regular.slot);assert_ne!(fresh.id,regular.id);assert_eq!(w.cancel_spawn_reservation(regular),Err(PopulationError::StaleReservation));let mut ready=Vec::new();w.ready_spawn_reservations(&mut ready);assert_eq!(ready,vec![fresh]);advance(&mut w,5);w.ready_spawn_reservations(&mut ready);assert_eq!(ready,vec![boss,fresh]);
 let a=entity(&mut w);let b=entity(&mut w);assert_eq!(w.allocate_spawn_group(20,Some(boss),&[a],0),Err(PopulationError::StaleReservation));let group=w.allocate_spawn_group(10,Some(boss),&[a,b],2).unwrap();assert_eq!(group.0,boss.id);assert_eq!(w.allocate_spawn_group(10,Some(boss),&[a],2),Err(PopulationError::StaleReservation));assert_eq!(w.release_spawn_member(a,Some(SpawnGroupId(group.0+1))),Err(PopulationError::GroupMismatch));
}

#[test]
fn group_birth_preflight_is_atomic_for_budget_collision_profile_and_members(){
 let map=CollisionMap::new(p(0),n(10),n(10),n(16)).unwrap();let mut w=World::with_map(DEFAULT_TICK_MS,map);w.resize_population(1).unwrap();let r=w.reserve_spawn(1).unwrap();let sentinel=entity(&mut w);let mut output=vec![sentinel];let mut budget=SpawnBudget::new(1,1).unwrap();
 assert_eq!(w.spawn_group(1,Some(r),template(),&[p(0),p(16)],&mut budget,&mut output),Err(PopulationError::BudgetExhausted));assert_eq!((w.len(),budget.remaining_members(),output.len()),(1,1,1));
 budget=SpawnBudget::new(64,1).unwrap();assert_eq!(w.spawn_group(1,Some(r),template(),&[p(0),p(-16)],&mut budget,&mut output),Err(PopulationError::World(WorldError::BlockedPosition)));assert_eq!(output,vec![sentinel]);assert_eq!(w.len(),1);
 let ai=AiProfile{aggro_radius:100,leash_radius:1,bonus_radius:100,attack:AttackProfile{range:1,damage:n(1),cooldown_ticks:t(1),windup_ticks:0},attack_break_range:1,chase:ChaseConfig::default()};
 assert_eq!(w.spawn_group(1,Some(r),SpawnTemplate{ai:Some(ai),..template()},&[p(0)],&mut budget,&mut output),Err(PopulationError::Ai(AiError::InvalidProfile)));assert_eq!(w.len(),1);
 assert_eq!(w.allocate_spawn_group(1,Some(r),&[sentinel,sentinel],0),Err(PopulationError::DuplicateMember));let group=w.spawn_group(1,Some(r),template(),&[p(0),p(16)],&mut budget,&mut output).unwrap();assert_eq!(output.len(),2);assert_eq!(budget.remaining_members(),62);assert_eq!(budget.remaining_groups(),0);for &e in &output{assert_eq!(w.health(e).unwrap().current,100);assert_eq!(w.spawn_group_of(e).unwrap(),Some(group));assert_eq!(w.lifecycle_state(e).unwrap().phase,LifecyclePhase::Alive);}
}

#[test]
fn raw_health_and_shielded_damage_release_only_on_actual_death_and_dense_reuse_is_safe(){
 let mut w=World::default();w.resize_population(2).unwrap();let (g,ids)=spawn(&mut w,1,2);let (_,other)=spawn(&mut w,2,1);w.grant_shield(ShieldSpec{source:ids[0],target:ids[0],duration:None,profile:ShieldProfile::Absorb{capacity:n(100)}}).unwrap();w.apply_damage(ids[0],100).unwrap();assert_eq!(w.population_state().alive,2);
 let h=w.health(ids[0]).unwrap();w.apply_health_commands(&[HealthCommand::Damage{entity:ids[0],amount:100}],&mut[Err(HealthError::Unconfigured)],&mut[HealthChange{entity:ids[0],before:h,after:h}]).unwrap();assert_eq!(w.population_state().alive,2);w.despawn(other[0]).unwrap();w.despawn(ids[0]).unwrap();assert_eq!(w.spawn_group_of(ids[1]).unwrap(),Some(g));let reused=entity(&mut w);assert!(w.spawn_group_of(reused).unwrap().is_none());assert_ne!(reused,ids[0]);w.despawn(ids[1]).unwrap();assert_eq!(w.population_state().alive,0);assert_eq!(w.population_state().cooling_down,2);
}

#[test]
fn batch_birth_budget_limits_whole_groups_across_calls(){
 let mut w=World::default();w.resize_population(4).unwrap();let mut budget=SpawnBudget::new(5,4).unwrap();let mut out=Vec::new();for _ in 0..2{w.spawn_group(1,None,template(),&[p(0),p(16)],&mut budget,&mut out).unwrap();}
 assert_eq!(w.spawn_group(1,None,template(),&[p(0),p(16)],&mut budget,&mut out),Err(PopulationError::BudgetExhausted));assert_eq!(w.len(),4);assert_eq!(w.population_state().alive,2);w.spawn_group(2,None,template(),&[p(32)],&mut budget,&mut out).unwrap();assert_eq!(w.len(),5);assert_eq!(SpawnBudget::new(4097,1),Err(PopulationError::InvalidBudget));
}

#[test]
fn species_gate_capacity_is_bounded_and_expired_gates_are_reclaimed(){
 let mut w=World::default();for species in 1..=1024{w.defer_spawn_species(species,n(1)).unwrap();}assert_eq!(w.defer_spawn_species(1025,n(1)),Err(PopulationError::SpeciesGateCapacity));w.advance_tick().unwrap();assert_eq!(w.defer_spawn_species(1025,n(1)),Ok(Tick(2)));assert_eq!(w.defer_spawn_species(0,n(1)),Err(PopulationError::InvalidSpecies));
}

#[test]
fn slot_word_boundaries_and_retirement_order_do_not_change_first_ready_choice(){
 let mut w=World::default();w.resize_population(256).unwrap();let mut ids=Vec::new();
 for _ in 0..256{let e=entity(&mut w);w.allocate_spawn_group(1,None,&[e],0).unwrap();ids.push(e);}
 assert_eq!(w.population_state().alive,256);assert_eq!(w.reserve_spawn(1),Err(PopulationError::NoVacancy));
 w.resize_population(255).unwrap();w.release_spawn_member(ids[0],None).unwrap();w.resize_population(256).unwrap();
 for index in [64,128,255]{w.release_spawn_member(ids[index],None).unwrap();}
 let mut reservations=Vec::new();for expected in [64,128,255,0]{let r=w.reserve_spawn(1).unwrap();assert_eq!(usize::from(r.slot),expected);reservations.push(r);}
 let mut ready=Vec::new();w.ready_spawn_reservations(&mut ready);assert_eq!(ready,reservations);let state=w.population_state();assert_eq!((state.alive,state.ready,state.reserved,state.eligible_unreserved),(252,4,4,0));
 for r in reservations{w.cancel_spawn_reservation(r).unwrap();}assert_eq!(w.population_state().eligible_unreserved,4);
 w.resize_population(252).unwrap();assert_eq!(w.population_state().alive,252);w.ready_spawn_reservations(&mut ready);assert!(ready.is_empty());
}
