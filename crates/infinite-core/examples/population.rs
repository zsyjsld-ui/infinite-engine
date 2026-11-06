//! Whole-group birth budget, final-member cooldown and deadline-driven map wake.
use infinite_core::*;use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn life(w:&mut World){let mut events=Vec::new();loop{let r=w.resolve_lifecycle(4096,&mut events).unwrap();assert!(r.blocked.is_none());if r.overdue.is_none(){break;}}}
fn main(){
 let mut w=World::default();w.resize_population(1).unwrap();let template=SpawnTemplate{maximum_health:n(100),corpse_ticks:2,respawn_ticks:5,ai:None};let mut ids=Vec::new();
 let first=w.spawn_group(1,None,template,&[p(0),p(16)],&mut SpawnBudget::new(2,1).unwrap(),&mut ids).unwrap();println!("born tick=0 group={first:?} members={}",ids.len());
 w.apply_damage(ids[0],100).unwrap();life(&mut w);assert_eq!(w.population_state().alive,1);w.skip_idle_to(Tick(2),MapWork::default()).unwrap();life(&mut w);
 w.apply_damage(ids[1],100).unwrap();life(&mut w);assert_eq!(w.population_state().next_due,Some(Tick(7)));println!("final member died tick=2; slot ready tick=7");
 w.skip_idle_to(Tick(4),MapWork::default()).unwrap();life(&mut w);assert_eq!(w.len(),0);w.skip_idle_to(Tick(7),MapWork::default()).unwrap();
 let next=w.spawn_group(1,None,template,&[p(32),p(48)],&mut SpawnBudget::new(2,1).unwrap(),&mut ids).unwrap();assert_ne!(first,next);assert_eq!(w.len(),2);println!("reborn tick={} group={next:?} members={} full_health={}",w.tick().0,ids.len(),ids.iter().all(|&e|w.health(e).unwrap().current==100));
}
