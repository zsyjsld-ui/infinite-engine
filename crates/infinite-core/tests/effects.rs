use infinite_core::*;use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn actor(w:&mut World,hp:u32)->EntityId{let id=w.spawn(GridPosition::default()).unwrap();w.configure_health(id,hp,n(100)).unwrap();w.configure_resource(id,0,50).unwrap();id}
fn attrs()->CombatAttributes{CombatAttributes{attack:100,break_power:10,defense:10,maximum_health:n(100),maximum_resource:50,level:t(1)}}
fn periodic(source:EntityId,target:EntityId,key:u16,kind:PeriodicKind,amount:u32,delay:u16,interval:u16,pulses:u16)->PeriodicEffect{PeriodicEffect{source,target,key,kind,amount:n(amount),first_delay:delay,interval:t(interval),pulses:t(pulses),refresh:PeriodicRefresh::StrongerOrExtend}}
fn advance(w:&mut World,count:usize){for _ in 0..count{w.advance_tick().unwrap();}}
fn buff(source:EntityId,target:EntityId,key:u16,duration:u16,attack:i32)->AttributeBuff{let mut modifiers=BuffModifiers::default();modifiers.flat[AttributeKind::Attack as usize]=attack;AttributeBuff{source,target,key,duration:t(duration),modifiers}}

#[test]
fn catchup_preserves_original_ticks_and_interleaves_health_and_resource(){
 let mut w=World::default();let source=actor(&mut w,100);let target=actor(&mut w,40);
 w.apply_periodic(periodic(source,target,1,PeriodicKind::Damage,15,1,2,3)).unwrap();
 w.apply_periodic(periodic(source,target,2,PeriodicKind::Heal,10,1,3,2)).unwrap();
 w.apply_periodic(periodic(source,target,3,PeriodicKind::RestoreResource,12,2,2,2)).unwrap();
 advance(&mut w,6);let mut out=Vec::new();let mut combat=Vec::new();let mut observed=Vec::new();
 loop{let r=w.resolve_effects(2,&mut out,&mut combat).unwrap();assert!(r.processed<=2);observed.extend(out.iter().map(|e|(e.scheduled_at,e.key)));if r.overdue.is_none(){break;}}
 assert_eq!(observed,vec![(Tick(1),1),(Tick(1),2),(Tick(2),3),(Tick(3),1),(Tick(4),2),(Tick(4),3),(Tick(5),1)]);
 assert_eq!(w.health(target).unwrap().current,15);assert_eq!(w.resource(target).unwrap().current,24);assert_eq!(w.active_effects(),0);assert_eq!(combat.len(),3);
}

#[test]
fn weak_refresh_extends_count_but_preserves_strength_interval_and_next_due(){
 let mut w=World::default();let source=actor(&mut w,100);let target=actor(&mut w,100);let initial=periodic(source,target,1,PeriodicKind::Damage,10,2,2,2);
 let id=w.apply_periodic(initial).unwrap().id;advance(&mut w,1);let result=w.apply_periodic(periodic(source,target,1,PeriodicKind::Damage,3,9,9,4)).unwrap();assert_eq!(result.id,id);assert_eq!(result.disposition,EffectDisposition::Extended);let state=w.effect_state(id).unwrap();assert_eq!(state.next_due,Tick(2));assert_eq!(state.expires_at,Tick(37));assert_eq!(state.kind,EffectStateKind::Periodic{kind:PeriodicKind::Damage,amount:n(10),interval:t(2),remaining:4});
 advance(&mut w,7);let mut out=Vec::new();let mut combat=Vec::new();w.resolve_effects(10,&mut out,&mut combat).unwrap();assert_eq!(w.health(target).unwrap().current,60);assert_eq!(out.iter().map(|e|e.scheduled_at).collect::<Vec<_>>(),vec![Tick(2),Tick(4),Tick(6),Tick(8)]);assert_eq!(w.active_effects(),0);
 let id=w.apply_periodic(initial).unwrap().id;advance(&mut w,1);let r=w.apply_periodic(periodic(source,target,1,PeriodicKind::Damage,20,3,3,2)).unwrap();assert_eq!(r.id,id);assert_eq!(r.disposition,EffectDisposition::Replaced);assert_eq!(w.effect_state(id).unwrap().next_due,Tick(12));advance(&mut w,6);combat.clear();w.resolve_effects(10,&mut out,&mut combat).unwrap();assert_eq!(w.health(target).unwrap().current,20);
}

#[test]
fn buffs_preserve_equipment_other_sources_and_refresh_without_stacking(){
 let mut w=World::default();let a=actor(&mut w,100);let b=actor(&mut w,100);let target=actor(&mut w,100);w.configure_attributes(target,attrs()).unwrap();let mut changes=Vec::new();w.resolve_attributes(&mut changes);
 let mut gear=AttributeModifiers::default();gear.set(AttributeKind::Attack,10,0);w.set_attribute_modifiers(target,gear).unwrap();
 let mut first=buff(a,target,1,3,20);first.modifiers.rate[0]=1000;let id1=w.apply_buff(first).unwrap().id;
 let mut second=buff(b,target,1,4,30);second.modifiers.flat[AttributeKind::MaximumResource as usize]=50;let id2=w.apply_buff(second).unwrap().id;w.resolve_attributes(&mut changes);assert_eq!(w.combat_attributes(target).unwrap().attack,176);w.apply_resource(target,ResourceCommand::Restore(100)).unwrap();assert_eq!(w.resource(target).unwrap().current,100);
 let refreshed=w.apply_buff(buff(a,target,1,3,40)).unwrap();assert_eq!(refreshed.id,id1);assert_eq!(w.active_effects(),2);w.resolve_attributes(&mut changes);assert_eq!(w.combat_attributes(target).unwrap().attack,180);
 w.cancel_effect(id1).unwrap();gear.set(AttributeKind::Attack,40,0);w.set_attribute_modifiers(target,gear).unwrap();w.resolve_attributes(&mut changes);assert_eq!(w.combat_attributes(target).unwrap().attack,170);
 advance(&mut w,4);let mut out=Vec::new();let mut combat=Vec::new();w.resolve_effects(10,&mut out,&mut combat).unwrap();assert_eq!(out[0].id,id2);w.resolve_attributes(&mut changes);assert_eq!(w.combat_attributes(target).unwrap().attack,140);assert_eq!(w.resource(target).unwrap().current,50);assert!(combat.is_empty());
}

#[test]
fn dirty_capacity_blocks_expiration_without_losing_modifier_or_task(){
 let mut w=World::default();let a=actor(&mut w,100);w.configure_attributes(a,attrs()).unwrap();let mut changes=Vec::new();w.resolve_attributes(&mut changes);let id=w.apply_buff(buff(a,a,1,1,20)).unwrap().id;w.resolve_attributes(&mut changes);
 for _ in 0..MAX_DIRTY_ATTRIBUTES{let id=actor(&mut w,100);w.configure_attributes(id,attrs()).unwrap();}
 advance(&mut w,1);let mut out=Vec::new();let mut combat=Vec::new();let report=w.resolve_effects(100,&mut out,&mut combat).unwrap();assert_eq!(report.blocked,Some(EffectError::Attribute(AttributeError::CapacityExhausted)));assert_eq!(report.processed,0);assert!(w.effect_state(id).is_ok());assert_eq!(w.combat_attributes(a).unwrap().attack,120);
 w.resolve_attributes(&mut changes);let report=w.resolve_effects(100,&mut out,&mut combat).unwrap();assert_eq!(report.ended,1);w.resolve_attributes(&mut changes);assert_eq!(w.combat_attributes(a).unwrap().attack,100);
}

#[test]
fn combat_capacity_blocks_before_damage_and_dead_targets_do_not_revive(){
 let mut w=World::default();let a=actor(&mut w,100);let b=actor(&mut w,10);let id=w.apply_periodic(periodic(a,b,1,PeriodicKind::Damage,20,0,1,3)).unwrap().id;w.apply_periodic(periodic(a,b,2,PeriodicKind::Heal,99,0,1,3)).unwrap();
 let placeholder=CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};let mut combat=vec![placeholder;MAX_ACTIVE_COMBATANTS];let mut out=Vec::new();let report=w.resolve_effects(100,&mut out,&mut combat).unwrap();assert_eq!(report.blocked,Some(EffectError::CombatEventCapacity));assert_eq!(w.health(b).unwrap().current,10);assert_eq!(w.effect_state(id).unwrap().next_due,Tick(0));
 combat.clear();let report=w.resolve_effects(100,&mut out,&mut combat).unwrap();assert_eq!((report.pulses,report.ended),(1,2));assert_eq!(w.health(b).unwrap().current,0);assert_eq!(combat.len(),1);assert!(matches!(out[1].kind,EffectEventKind::Ended(EffectEnd::TargetDepleted)));
}

#[test]
fn source_and_target_reuse_never_redirect_effects_or_buff_removal(){
 let mut w=World::default();let a=actor(&mut w,100);let b=actor(&mut w,100);let c=actor(&mut w,100);w.configure_attributes(b,attrs()).unwrap();w.configure_attributes(c,attrs()).unwrap();let mut changes=Vec::new();w.resolve_attributes(&mut changes);
 let old=w.apply_buff(buff(a,b,1,1,99)).unwrap().id;let surviving=w.apply_buff(buff(a,c,1,1,5)).unwrap().id;w.apply_periodic(periodic(a,c,2,PeriodicKind::Damage,10,1,1,3)).unwrap();w.resolve_attributes(&mut changes);
 w.despawn(b).unwrap();let replacement=actor(&mut w,100);w.despawn(a).unwrap();assert!(w.effect_state(old).is_err());advance(&mut w,1);let mut out=Vec::new();let mut combat=Vec::new();w.resolve_effects(10,&mut out,&mut combat).unwrap();assert_eq!(out[0].id,surviving);assert!(matches!(out[1].kind,EffectEventKind::Ended(EffectEnd::SourceGone)));w.resolve_attributes(&mut changes);assert_eq!(w.combat_attributes(c).unwrap().attack,100);assert_eq!(w.health(c).unwrap().current,100);assert_eq!(w.health(replacement).unwrap().current,100);
}

#[test]
fn dense_batch_matches_heap_for_partial_work_backpressure_repeats_and_reuse(){
 fn fixture()->(World,EntityId,Vec<EntityId>,Vec<EffectId>){
  let mut w=World::default();let source=actor(&mut w,100);let mut targets=Vec::new();let mut ids=Vec::new();
  for i in 0..256{let target=actor(&mut w,100);w.configure_attributes(target,attrs()).unwrap();targets.push(target);
   ids.push(w.apply_periodic(periodic(source,target,1,PeriodicKind::Damage,10,1,(i%3+1)as u16,15)).unwrap().id);
   ids.push(w.apply_periodic(periodic(source,target,2,PeriodicKind::RestoreResource,2,1,(i%4+1)as u16,15)).unwrap().id);
   ids.push(w.apply_buff(buff(source,target,3,1,5)).unwrap().id);
  }w.resolve_attributes(&mut Vec::new());(w,source,targets,ids)
 }
 fn drain(w:&mut World,limit:usize,combat:&mut Vec<CombatEvent>)->(Vec<EffectEvent>,Option<EffectError>){
  let mut all=Vec::new();let mut out=Vec::new();loop{let r=w.resolve_effects(limit,&mut out,combat).unwrap();all.extend_from_slice(&out);if r.blocked.is_some()||r.overdue.is_none(){return(all,r.blocked);}}
 }
 let(mut a,source,targets,ids)=fixture();let(mut b,_,_,_)=fixture();
 for tick in 1..=22{
  a.advance_tick().unwrap();b.advance_tick().unwrap();
  if tick==4{a.cancel_effect(ids[19]).unwrap();b.cancel_effect(ids[19]).unwrap();}
  if tick==6{a.despawn(targets[7]).unwrap();b.despawn(targets[7]).unwrap();assert_eq!(actor(&mut a,100),actor(&mut b,100));}
  let placeholder=CombatEvent{attacker:source,target:targets[0],kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};
  let mut ac=if tick==1{vec![placeholder;MAX_ACTIVE_COMBATANTS-91]}else{Vec::new()};let mut bc=ac.clone();
  assert_eq!(drain(&mut a,4096,&mut ac),drain(&mut b,127,&mut bc));assert_eq!(ac,bc);
  ac.clear();bc.clear();assert_eq!(drain(&mut a,4096,&mut ac),drain(&mut b,127,&mut bc));assert_eq!(ac,bc);
  let(mut aa,mut ba)=(Vec::new(),Vec::new());a.resolve_attributes(&mut aa);b.resolve_attributes(&mut ba);assert_eq!(aa,ba);
  for &id in &ids{assert_eq!(a.effect_state(id),b.effect_state(id));}for &id in &targets{assert_eq!(a.health(id),b.health(id));assert_eq!(a.resource(id),b.resource(id));}
 }
}

#[test]
fn cached_full_cohort_rejects_equal_count_handle_reuse(){
 let mut w=World::default();let source=actor(&mut w,100);let mut targets=Vec::new();let mut ids=Vec::new();
 for _ in 0..256{let target=actor(&mut w,100);targets.push(target);ids.push(w.apply_periodic(periodic(source,target,1,PeriodicKind::Damage,1,1,1,8)).unwrap().id);}
 let(mut out,mut combat)=(Vec::new(),Vec::new());
 for _ in 0..2{w.advance_tick().unwrap();combat.clear();w.resolve_effects(4096,&mut out,&mut combat).unwrap();assert_eq!(out.iter().map(|e|e.target).collect::<Vec<_>>(),targets);}
 w.cancel_effect(ids[0]).unwrap();let replacement=w.apply_periodic(periodic(source,targets[0],1,PeriodicKind::Damage,1,1,1,8)).unwrap().id;assert_ne!(ids[0],replacement);assert_eq!(w.active_effects(),256);
 for _ in 0..2{w.advance_tick().unwrap();combat.clear();w.resolve_effects(4096,&mut out,&mut combat).unwrap();let mut order=targets[1..].to_vec();order.push(targets[0]);assert_eq!(out.iter().map(|e|e.target).collect::<Vec<_>>(),order);}
 assert!(w.effect_state(ids[0]).is_err());assert_eq!(w.effect_state(replacement).unwrap().next_due,Tick(5));
}

#[test]
fn key_and_per_target_limits_are_atomic_and_large_contributions_reversible(){
 let mut w=World::default();let a=actor(&mut w,100);w.configure_attributes(a,attrs()).unwrap();let mut out=Vec::new();w.resolve_attributes(&mut out);let mut ids=Vec::new();
 for key in 0..64{let mut b=buff(a,a,key,10,0);b.modifiers.flat=[i32::MAX;ATTRIBUTE_COUNT];b.modifiers.rate=[i32::MAX;ATTRIBUTE_COUNT];ids.push(w.apply_buff(b).unwrap().id);}
 assert_eq!(w.apply_buff(buff(a,a,99,10,1)),Err(EffectError::PerEntityLimit));assert_eq!(w.apply_periodic(periodic(a,a,0,PeriodicKind::Damage,1,1,1,1)),Err(EffectError::KindConflict));w.resolve_attributes(&mut out);assert_eq!(w.combat_attributes(a).unwrap().attack,u32::MAX);
 let mut listed=Vec::new();w.entity_effects(a,&mut listed).unwrap();assert_eq!(listed,ids);for id in ids{w.cancel_effect(id).unwrap();}w.resolve_attributes(&mut out);assert_eq!(w.combat_attributes(a).unwrap(),attrs());assert_eq!(w.active_effects(),0);
}

#[test]
fn skill_ready_dispatches_periodic_damage_and_v3_ack_does_not_repeat_pulses(){
 let mut w=World::default();let a=actor(&mut w,100);let b=actor(&mut w,100);w.learn_skill(a,SkillSpec{id:SkillId(1),resource_cost:0,cooldown_ticks:10,windup_ticks:0,recovery:None,ingress_grace_ticks:0}).unwrap();let request=CastRequest{caster:a,request_id:1,skill:SkillId(1),target:CastTarget::Entity(b)};w.start_cast(request).unwrap();let mut ready=Vec::new();w.resolve_casts(10,&mut ready).unwrap();for e in ready{if let SkillEvent::Ready(c)=e{w.apply_periodic(periodic(c.request.caster,b,1,PeriodicKind::Damage,7,1,1,2)).unwrap();}}
 let mut frame=CombatFrame::new(n(16));let mut sender=CombatSyncSender::new_batched(1,CombatSyncConfig::default()).unwrap();let mut replica=CombatReplica::new(1);let mut output=Vec::new();let mut combat=Vec::new();let mut hits=0;
 for _ in 0..2{w.advance_tick().unwrap();combat.clear();w.resolve_effects(10,&mut output,&mut combat).unwrap();frame.capture(&w,&combat).unwrap();sender.observe(&frame,ViewRect{min:GridPosition{x:-1,y:-1},max:GridPosition{x:1,y:1}}).unwrap();let packet=sender.prepare().unwrap().unwrap().to_vec();let report=replica.apply(&packet).unwrap();hits+=replica.drain_feedback().count();assert!(replica.apply(&packet).unwrap().duplicate);assert_eq!(replica.drain_feedback().count(),0);sender.acknowledge_packet(&report.acknowledgement()).unwrap();}
 assert_eq!(hits,2);assert_eq!(w.health(b).unwrap().current,86);assert!(w.start_cast(request).unwrap().duplicate);assert_eq!(w.active_effects(),0);
}
