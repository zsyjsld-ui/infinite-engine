use infinite_core::*;use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn main(){
 let mut w=World::default();let source=w.spawn(GridPosition::default()).unwrap();let target=w.spawn(GridPosition{x:16,y:0}).unwrap();for id in [source,target]{w.configure_health(id,100,n(100)).unwrap();}w.configure_resource(target,0,50).unwrap();
 w.configure_attributes(target,CombatAttributes{attack:100,break_power:0,defense:10,maximum_health:n(100),maximum_resource:50,level:t(1)}).unwrap();let mut changes=Vec::new();w.resolve_attributes(&mut changes);
 let mut modifiers=BuffModifiers::default();modifiers.flat[0]=20;w.apply_buff(AttributeBuff{source,target,key:1,duration:t(3),modifiers}).unwrap();w.resolve_attributes(&mut changes);
 for (key,kind,amount)in [(2,PeriodicKind::Damage,7),(3,PeriodicKind::RestoreResource,10)]{w.apply_periodic(PeriodicEffect{source,target,key,kind,amount:n(amount),first_delay:1,interval:t(2),pulses:t(3),refresh:PeriodicRefresh::StrongerOrExtend}).unwrap();}
 let mut events=Vec::new();let mut combat=Vec::new();for _ in 0..6{w.advance_tick().unwrap();combat.clear();let report=w.resolve_effects(MAX_EFFECT_WORK,&mut events,&mut combat).unwrap();w.resolve_attributes(&mut changes);println!("{report:?}, hp={}, resource={}, attack={}, feedback={}",w.health(target).unwrap().current,w.resource(target).unwrap().current,w.combat_attributes(target).unwrap().attack,combat.len());for event in &events{println!("  {event:?}");}}
}
