use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn level(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn base()->CombatAttributes{CombatAttributes{attack:100,break_power:0,defense:100,maximum_health:n(200),maximum_resource:80,level:level(1)}}
fn spawn(w:&mut World,hp:u32)->EntityId{let id=w.spawn(GridPosition::default()).unwrap();w.configure_health(id,hp,n(200)).unwrap();id}
fn curve()->ArmorCurve{ArmorCurve{base:n(50),per_level:2,armor_rate_bps:600}}

#[test]
fn dirty_coalescing_and_health_clamps_do_not_heal_or_revive(){
 let mut w=World::default();let a=spawn(&mut w,100);w.configure_attributes(a,base()).unwrap();
 assert_eq!(w.combat_attributes(a),Err(AttributeError::Dirty));assert_eq!(w.pending_attribute_updates(),1);
 assert!(!w.set_base_attributes(a,base()).unwrap());let mut changes=Vec::new();
 assert_eq!(w.resolve_attributes(&mut changes).changed,1);assert_eq!(changes[0].before,None);
 assert_eq!(w.resolve_attributes(&mut changes).recomputed,0);assert!(changes.is_empty());
 let mut modifiers=AttributeModifiers::default();modifiers.set(AttributeKind::Attack,7,125);
 w.set_attribute_modifiers(a,modifiers).unwrap();modifiers.set(AttributeKind::MaximumHealth,-150,0);w.set_attribute_modifiers(a,modifiers).unwrap();
 assert_eq!(w.pending_attribute_updates(),1);assert_eq!(w.resolve_attributes(&mut changes).recomputed,1);
 assert_eq!(w.combat_attributes(a).unwrap().attack,108);assert_eq!(w.health(a).unwrap().current,50);assert_eq!(w.health(a).unwrap().maximum,n(50));
 w.set_attribute_modifiers(a,AttributeModifiers::default()).unwrap();w.resolve_attributes(&mut changes);
 assert_eq!(w.health(a).unwrap().current,50);assert_eq!(w.health(a).unwrap().maximum,n(200));
 let dead=spawn(&mut w,0);w.configure_attributes(dead,base()).unwrap();w.set_attribute_modifiers(dead,modifiers).unwrap();w.resolve_attributes(&mut changes);assert_eq!(w.health(dead).unwrap().current,0);
}

#[test]
fn cancelling_changes_before_resolution_emits_no_net_change(){
 let mut w=World::default();let a=spawn(&mut w,100);let mut changes=Vec::new();w.configure_attributes(a,base()).unwrap();w.resolve_attributes(&mut changes);
 let mut modified=base();modified.attack=999;w.set_base_attributes(a,modified).unwrap();w.set_base_attributes(a,base()).unwrap();
 let report=w.resolve_attributes(&mut changes);assert_eq!((report.recomputed,report.changed),(1,0));assert!(changes.is_empty());
}

#[test]
fn capacity_failure_is_atomic_and_despawn_repairs_dirty_dense_rows(){
 let mut w=World::default();let ids:Vec<_>=(0..MAX_DIRTY_ATTRIBUTES+1).map(|_|spawn(&mut w,100)).collect();
 for &id in &ids[..MAX_DIRTY_ATTRIBUTES]{w.configure_attributes(id,base()).unwrap();}
 assert_eq!(w.configure_attributes(ids[MAX_DIRTY_ATTRIBUTES],base()),Err(AttributeError::CapacityExhausted));
 assert_eq!(w.combat_attributes(ids[MAX_DIRTY_ATTRIBUTES]),Err(AttributeError::Unconfigured));
 // Remove a pending non-tail row; the world's last (unconfigured) row moves in.
 w.despawn(ids[3]).unwrap();w.configure_attributes(ids[MAX_DIRTY_ATTRIBUTES],base()).unwrap();
 let mut changes=Vec::new();assert_eq!(w.resolve_attributes(&mut changes).changed,4096);
 assert!(!changes.iter().any(|c|c.entity==ids[3]));for c in &changes{assert_eq!(w.combat_attributes(c.entity).unwrap(),base());}
 // Now swap a dirty last row into another dirty row's position.
 let replacement=spawn(&mut w,100);w.configure_attributes(replacement,base()).unwrap();let mut b=base();b.attack=7;w.set_base_attributes(ids[1],b).unwrap();
 w.despawn(ids[1]).unwrap();assert_eq!(w.resolve_attributes(&mut changes).changed,1);assert_eq!(changes[0].entity,replacement);
 assert_eq!(w.combat_attributes(ids[1]),Err(AttributeError::InvalidEntity));
}

#[test]
fn extreme_modifier_intermediates_clamp_without_overflow(){
 let mut w=World::default();let a=spawn(&mut w,100);w.configure_attributes(a,base()).unwrap();let mut out=Vec::new();
 let huge=AttributeModifiers{flat:[i64::MAX;ATTRIBUTE_COUNT],rate:[i64::MAX;ATTRIBUTE_COUNT]};w.set_attribute_modifiers(a,huge).unwrap();w.resolve_attributes(&mut out);
 let derived=w.combat_attributes(a).unwrap();assert_eq!(derived.attack,u32::MAX);assert_eq!(derived.maximum_health,n(u32::MAX));assert_eq!(w.health(a).unwrap().current,100);
 let negative=AttributeModifiers{flat:[i64::MIN;ATTRIBUTE_COUNT],rate:[i64::MIN;ATTRIBUTE_COUNT]};w.set_attribute_modifiers(a,negative).unwrap();w.resolve_attributes(&mut out);
 let derived=w.combat_attributes(a).unwrap();assert_eq!(derived.attack,0);assert_eq!(derived.maximum_resource,0);assert_eq!(derived.maximum_health,n(1));assert_eq!(w.health(a).unwrap().current,1);
}

#[test]
fn rational_armor_known_values_and_monotonic_limits(){
 assert_eq!(physical_damage(1000,100,0,level(1),0,curve()),Ok(897));
 assert_eq!(physical_damage(1000,100,0,level(1),5000,curve()),Ok(945));
 assert_eq!(physical_damage(1000,100,100,level(1),0,curve()),Ok(1000));
 assert_eq!(physical_damage(0,u32::MAX,0,level(1),0,curve()),Ok(0));
 assert_eq!(physical_damage(1,u32::MAX,0,level(1),0,curve()),Ok(1));
 assert_eq!(physical_damage(1,0,0,level(1),10001,curve()),Err(AttributeError::InvalidIgnoreRate));
 let extreme=ArmorCurve{base:n(u32::MAX),per_level:u16::MAX,armor_rate_bps:u16::MAX};
 assert_eq!(physical_damage(u32::MAX,u32::MAX,u32::MAX,level(u16::MAX),0,extreme),Ok(u32::MAX));
 // Independently calculated BigInt fixtures exercise wide products and the
 // rounding addition, rather than only the no-armor shortcut at extreme inputs.
 assert_eq!(physical_damage(u32::MAX,u32::MAX,0,level(u16::MAX),0,extreme),Ok(1_004_247_871));
 assert_eq!(physical_damage(u32::MAX,u32::MAX,0,level(1),0,curve()),Ok(867));
 assert_eq!(physical_damage(u32::MAX,4_000_000_000,3_000_000_000,level(u16::MAX),1000,curve()),Ok(15_586_456));
 let mut last=u32::MAX;for defense in (0..100000).step_by(37){let hit=physical_damage(1000,defense,0,level(1),0,curve()).unwrap();assert!(hit<=last&&hit>=1);last=hit;}
}

#[test]
fn configured_attributes_drive_actual_attack_and_health_sync(){
 let mut w=World::default();let a=spawn(&mut w,200);let b=spawn(&mut w,200);
 w.configure_attributes(a,base()).unwrap();w.configure_attributes(b,base()).unwrap();let mut out=Vec::new();w.resolve_attributes(&mut out);
 let actor=w.combat_attributes(a).unwrap();let target=w.combat_attributes(b).unwrap();
 let damage=physical_damage(actor.attack,target.defense,actor.break_power,target.level,0,curve()).unwrap();assert_eq!(damage,90);
 w.start_attack(a,b,AttackProfile{range:0,damage:n(damage),cooldown_ticks:level(2),windup_ticks:0}).unwrap();
 let mut events=[CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}];let h=w.health(b).unwrap();let mut changes=[HealthChange{entity:b,before:h,after:h}];w.resolve_combat(&mut events,&mut changes).unwrap();assert_eq!(w.health(b).unwrap().current,110);
 let mut frame=CombatFrame::new(n(16));frame.capture(&w,&events).unwrap();let mut sender=CombatSyncSender::new_batched(7,CombatSyncConfig::default()).unwrap();sender.observe(&frame,ViewRect{min:GridPosition{x:-1,y:-1},max:GridPosition{x:1,y:1}}).unwrap();let mut replica=CombatReplica::new(7);replica.apply(sender.prepare().unwrap().unwrap()).unwrap();
 assert_eq!(replica.drain_feedback().next().unwrap().kind,CombatFeedbackKind::Hit{damage:90,remaining:110});
}
