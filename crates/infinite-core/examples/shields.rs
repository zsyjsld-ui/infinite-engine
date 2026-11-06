use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn main(){
    let mut world=World::default();let source=world.spawn(GridPosition::default()).unwrap();let target=world.spawn(GridPosition{x:16,y:0}).unwrap();
    for id in[source,target]{world.configure_health(id,100,n(100)).unwrap();}
    world.configure_resource(target,250,250).unwrap();world.configure_attributes(target,CombatAttributes{attack:100,break_power:0,defense:0,maximum_health:n(100),maximum_resource:250,level:t(1)}).unwrap();
    let mut attributes=Vec::new();world.resolve_attributes(&mut attributes);let mut modifiers=BuffModifiers::default();modifiers.flat[0]=20;
    let sustained=world.apply_buff(AttributeBuff{source,target,key:1,duration:t(100),modifiers}).unwrap().id;world.resolve_attributes(&mut attributes);
    world.grant_shield(ShieldSpec{source,target,duration:None,profile:ShieldProfile::Mana{numerator:t(41),denominator:t(40),minimum_resource:n(100)}}).unwrap();
    let hit=world.apply_damage(target,100).unwrap();assert_eq!(hit.absorbed,100);assert_eq!(world.resource(target).unwrap().current,147);
    let combat=[CombatEvent{attacker:source,target,kind:CombatEventKind::Hit{health:hit.health,next_ready:Tick(1)}}];
    let mut frame=CombatFrame::new(n(32));frame.capture(&world,&combat).unwrap();let mut sender=CombatSyncSender::new_shielded(1,CombatSyncConfig::default()).unwrap();let mut replica=CombatReplica::new(1);
    sender.observe(&frame,ViewRect{min:GridPosition{x:-32,y:-32},max:GridPosition{x:32,y:32}}).unwrap();let bytes=sender.prepare().unwrap().unwrap().to_vec();let applied=replica.apply(&bytes).unwrap();println!("IC v{}: {:?}",bytes[2],replica.drain_feedback().collect::<Vec<_>>());sender.acknowledge_packet(&applied.acknowledgement()).unwrap();
    world.apply_periodic(PeriodicEffect{source,target,key:2,kind:PeriodicKind::DrainResource,amount:n(100),first_delay:1,interval:t(1),pulses:t(10),refresh:PeriodicRefresh::Replace}).unwrap();
    let(mut effects,mut combat)=(Vec::new(),Vec::new());for _ in 0..2{world.advance_tick().unwrap();world.resolve_effects(100,&mut effects,&mut combat).unwrap();for effect in &effects{if effect.ended==Some(EffectEnd::ResourceDepleted){world.cancel_effect(sustained).unwrap();}}world.resolve_attributes(&mut attributes);println!("tick {}: resource={}, shield={:?}, attack={}",world.tick().0,world.resource(target).unwrap().current,world.shield(target).unwrap(),world.combat_attributes(target).unwrap().attack);}
    let hit=world.apply_damage(target,20).unwrap();assert_eq!(hit.absorbed,0);assert_eq!(hit.health.after.current,80);assert_eq!(world.resource(target).unwrap().current,0);assert_eq!(world.combat_attributes(target).unwrap().attack,100);assert_eq!(world.active_effects(),0);println!("unshielded hit: HP={}",hit.health.after.current);
}
