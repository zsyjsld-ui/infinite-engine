use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn main(){
 let mut world=World::default();let actor=world.spawn(GridPosition::default()).unwrap();world.configure_health(actor,100,n(200)).unwrap();
 world.configure_attributes(actor,CombatAttributes{attack:100,break_power:20,defense:30,maximum_health:n(200),maximum_resource:80,level:NonZeroU16::new(10).unwrap()}).unwrap();
 let mut changes=Vec::new();println!("initial {:?}",world.resolve_attributes(&mut changes));
 let mut equipment=AttributeModifiers::default();equipment.set(AttributeKind::Attack,50,2000);world.set_attribute_modifiers(actor,equipment).unwrap();
 println!("changed {:?}",world.resolve_attributes(&mut changes));println!("idle {:?}",world.resolve_attributes(&mut changes));
 let attributes=world.combat_attributes(actor).unwrap();let damage=physical_damage(attributes.attack,200,attributes.break_power,NonZeroU16::new(10).unwrap(),0,ArmorCurve{base:n(50),per_level:2,armor_rate_bps:600}).unwrap();
 println!("attributes={attributes:?}, physical damage against defense 200={damage}");
}
