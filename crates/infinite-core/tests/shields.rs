use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn t(v:u16)->NonZeroU16{NonZeroU16::new(v).unwrap()}
fn actor(w:&mut World,hp:u32,mp:u32)->EntityId{let id=w.spawn(GridPosition::default()).unwrap();w.configure_health(id,hp,n(hp.max(1))).unwrap();w.configure_resource(id,mp,mp).unwrap();id}
fn absorb(source:EntityId,target:EntityId,amount:u32,duration:Option<u16>)->ShieldSpec{ShieldSpec{source,target,duration:duration.map(t),profile:ShieldProfile::Absorb{capacity:n(amount)}}}
fn mana(id:EntityId,numerator:u16,denominator:u16,minimum:u32)->ShieldSpec{ShieldSpec{source:id,target:id,duration:None,profile:ShieldProfile::Mana{numerator:t(numerator),denominator:t(denominator),minimum_resource:n(minimum)}}}
fn advance(w:&mut World,n:usize){for _ in 0..n{w.advance_tick().unwrap();}}
fn attrs(max_resource:u32)->CombatAttributes{CombatAttributes{attack:100,break_power:0,defense:0,maximum_health:n(100),maximum_resource:max_resource,level:t(1)}}

#[test]
fn absorb_precedes_health_clamp_and_depletion_is_latched(){
 let mut w=World::default();let a=actor(&mut w,10,0);w.grant_shield(absorb(a,a,100,None)).unwrap();
 let r=w.apply_damage(a,50).unwrap();assert_eq!(r.absorbed,50);assert_eq!(r.health.after.current,10);assert_eq!(w.shield(a).unwrap().unwrap().profile,ShieldProfile::Absorb{capacity:n(50)});
 let r=w.apply_damage(a,70).unwrap();assert_eq!(r.absorbed,50);assert!(r.health.depleted());assert_eq!(w.shield(a).unwrap(),None);assert_eq!(w.apply_damage(a,1),Err(HealthError::Depleted));
}

#[test]
fn integer_mana_credit_matches_aggregate_cost_and_extreme_inputs(){
 for (num,den) in [(41,40),(1,40),(80,40),(65535,65535)]{
  let mut w=World::default();let a=actor(&mut w,1000,10000);let b=actor(&mut w,1000,10000);w.grant_shield(mana(a,num,den,1)).unwrap();w.grant_shield(mana(b,num,den,1)).unwrap();
  let mut damage=0;for i in 0..100{let hit=i%7+1;damage+=hit;assert_eq!(w.apply_damage(a,hit).unwrap().absorbed,hit);}
  assert_eq!(w.apply_damage(b,damage).unwrap().absorbed,damage);assert_eq!(w.resource(a),w.resource(b));assert_eq!(w.shield(a).unwrap().unwrap().prepaid_units,w.shield(b).unwrap().unwrap().prepaid_units);
  let expected=(u64::from(damage)*u64::from(num)).div_ceil(u64::from(den))as u32;assert_eq!(w.resource(a).unwrap().current,10000-expected);
 }
 let mut w=World::default();let a=actor(&mut w,u32::MAX,u32::MAX);w.grant_shield(mana(a,65535,1,1)).unwrap();let r=w.apply_damage(a,u32::MAX).unwrap();assert_eq!(r.absorbed,u32::MAX/65535);assert_eq!(r.resource.unwrap().after.current,0);assert_eq!(r.health.after.current,r.absorbed);assert!(w.shield(a).unwrap().is_none());
 let b=actor(&mut w,u32::MAX,u32::MAX);w.grant_shield(mana(b,1,65535,1)).unwrap();let r=w.apply_damage(b,u32::MAX).unwrap();assert_eq!(r.absorbed,u32::MAX);assert_eq!(r.resource.unwrap().after.current,u32::MAX-65537);
}

#[test]
fn resource_spend_skill_and_derived_clamp_invalidate_before_regeneration(){
 let mut w=World::default();let a=actor(&mut w,100,300);
 for mode in 0..3{
  w.grant_shield(mana(a,41,40,100)).unwrap();
  match mode{
   0=>{w.apply_resource(a,ResourceCommand::Spend(201)).unwrap();},
   1=>{w.learn_skill(a,SkillSpec{id:SkillId(1),resource_cost:201,cooldown_ticks:1,windup_ticks:0,recovery:None,ingress_grace_ticks:0}).unwrap();w.start_cast(CastRequest{caster:a,request_id:1,skill:SkillId(1),target:CastTarget::Entity(a)}).unwrap();},
   _=>{w.configure_attributes(a,attrs(50)).unwrap();w.resolve_attributes(&mut Vec::new());},
  }
  assert_eq!(w.shield(a).unwrap(),None);w.apply_resource(a,ResourceCommand::SetMaximum(300)).unwrap();w.apply_resource(a,ResourceCommand::Restore(300)).unwrap();assert_eq!(w.shield(a).unwrap(),None);
 }
 w.grant_shield(mana(a,41,40,100)).unwrap();let r=w.apply_damage(a,200).unwrap();assert_eq!(r.absorbed,200);assert!(w.resource(a).unwrap().current<100);w.apply_resource(a,ResourceCommand::Restore(300)).unwrap();assert_eq!(w.shield(a).unwrap(),None);
}

#[test]
fn expired_shield_cannot_absorb_and_deadlines_survive_replacement_and_dense_swaps(){
 let mut w=World::default();let a=actor(&mut w,100,100);let b=actor(&mut w,100,100);let c=actor(&mut w,100,100);
 w.grant_shield(absorb(a,b,10,Some(2))).unwrap();w.grant_shield(absorb(a,c,20,Some(2))).unwrap();w.despawn(a).unwrap();let replacement=actor(&mut w,100,100);advance(&mut w,2);
 assert_eq!(w.shield(b).unwrap(),None);assert_eq!(w.apply_damage(b,5).unwrap().absorbed,0);
 let renewed=w.grant_shield(absorb(replacement,c,30,Some(3))).unwrap();assert_eq!(renewed.expires_at,Some(Tick(5)));
 let mut expired=Vec::new();assert_eq!(w.resolve_shields(4096,&mut expired).unwrap().expired,0);advance(&mut w,3);let r=w.resolve_shields(0,&mut expired).unwrap();assert_eq!(r.overdue,Some(Tick(5)));assert_eq!(w.resolve_shields(1,&mut expired).unwrap().expired,1);assert_eq!(expired[0].target,c);assert_eq!(expired[0].scheduled_at,Tick(5));assert!(w.shield(replacement).unwrap().is_none());
 w.grant_shield(absorb(replacement,b,20,Some(1))).unwrap();w.despawn(b).unwrap();let new_b=actor(&mut w,100,100);advance(&mut w,1);assert_eq!(w.resolve_shields(10,&mut expired).unwrap().expired,0);assert!(w.shield(new_b).unwrap().is_none());
}

#[test]
fn invalid_grants_and_raw_health_death_keep_atomic_contracts(){
 let mut w=World::default();let a=actor(&mut w,100,99);assert_eq!(w.grant_shield(mana(a,41,40,100)),Err(ShieldError::InsufficientResource));let state=w.grant_shield(absorb(a,a,20,Some(2))).unwrap();assert_eq!(w.grant_shield(absorb(a,a,99,Some(1))),Err(ShieldError::AlreadyShielded));assert_eq!(w.shield(a).unwrap(),Some(state));
 let raw=HealthCommand::Damage{entity:a,amount:100};let health=w.health(a).unwrap();let mut out=[Err(HealthError::InvalidEntity)];let mut changes=[HealthChange{entity:a,before:health,after:health}];w.apply_health_commands(&[raw],&mut out,&mut changes).unwrap();assert!(out[0].unwrap().depleted());assert_eq!(w.shield(a).unwrap(),None);
 w.apply_health_commands(&[HealthCommand::Revive{entity:a,current:n(100)}],&mut out,&mut changes).unwrap();assert_eq!(w.apply_damage(a,10).unwrap().absorbed,0);
}

#[test]
fn basic_area_and_periodic_damage_all_consume_the_same_shield(){
 let mut w=World::default();let a=actor(&mut w,100,0);let b=actor(&mut w,100,0);w.grant_shield(absorb(a,b,25,None)).unwrap();
 w.start_attack(a,b,AttackProfile{range:1,damage:n(10),cooldown_ticks:t(10),windup_ticks:0}).unwrap();let h=w.health(b).unwrap();let placeholder=CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};let mut events=[placeholder];let mut changes=[HealthChange{entity:b,before:h,after:h}];let r=w.resolve_combat(&mut events,&mut changes).unwrap();assert_eq!(r.hits,1);assert_eq!(w.health(b).unwrap().current,100);
 let mut actions=CombatActions::new(n(16));let mut budget=CombatQueryBudget::new(100);let mut combat=Vec::new();actions.begin(&mut w,&mut budget,&mut combat,4096).unwrap().damage_area(AreaDamage{attacker:a,area:CombatArea::Circle{center:GridPosition::default(),radius:1},damage:n(10),require_sight:false},|_,_|true).unwrap();assert_eq!(w.health(b).unwrap().current,100);
 w.apply_periodic(PeriodicEffect{source:a,target:b,key:1,kind:PeriodicKind::Damage,amount:n(10),first_delay:0,interval:t(1),pulses:t(1),refresh:PeriodicRefresh::Replace}).unwrap();let mut effects=Vec::new();w.resolve_effects(10,&mut effects,&mut combat).unwrap();assert_eq!(w.health(b).unwrap().current,95);assert!(w.shield(b).unwrap().is_none());
 let mut all=events.to_vec();all.extend(combat);let mut frame=CombatFrame::new(n(16));frame.capture(&w,&all).unwrap();let mut sender=CombatSyncSender::new_shielded(1,CombatSyncConfig::default()).unwrap();let mut replica=CombatReplica::new(1);let view=ViewRect{min:GridPosition{x:-1,y:-1},max:GridPosition{x:1,y:1}};sender.observe(&frame,view).unwrap();let packet=sender.prepare().unwrap().unwrap().to_vec();assert_eq!(packet[2],4);let report=replica.apply(&packet).unwrap();assert_eq!(replica.drain_feedback().map(|e|e.kind).collect::<Vec<_>>(),vec![CombatFeedbackKind::Hit{damage:0,remaining:100},CombatFeedbackKind::Hit{damage:0,remaining:100},CombatFeedbackKind::Hit{damage:5,remaining:95}]);assert!(replica.apply(&packet).unwrap().duplicate);assert_eq!(replica.drain_feedback().count(),0);sender.acknowledge_packet(&report.acknowledgement()).unwrap();
 for mut old in [CombatSyncSender::new(2,CombatSyncConfig::default()).unwrap(),CombatSyncSender::new_compact(2,CombatSyncConfig::default()).unwrap(),CombatSyncSender::new_batched(2,CombatSyncConfig::default()).unwrap()]{assert_eq!(old.observe(&frame,view),Err(CombatSyncError::UnsupportedFeedback));assert!(old.pending_packet().is_none());assert_eq!(old.queued_events(),0);}
 let mut old_packet=packet.clone();old_packet[2]=3;let mut strict=CombatReplica::new(1);assert!(strict.apply(&old_packet).is_err());assert_eq!(strict.sequence(),0);assert!(strict.states().is_empty());
}

#[test]
fn sustained_drain_stops_at_zero_and_application_removes_linked_buff(){
 let mut w=World::default();let a=actor(&mut w,100,250);w.configure_attributes(a,attrs(250)).unwrap();w.resolve_attributes(&mut Vec::new());let mut modifiers=BuffModifiers::default();modifiers.flat[0]=20;let buff=w.apply_buff(AttributeBuff{source:a,target:a,key:1,duration:t(100),modifiers}).unwrap().id;w.resolve_attributes(&mut Vec::new());
 w.grant_shield(mana(a,41,40,100)).unwrap();w.apply_periodic(PeriodicEffect{source:a,target:a,key:2,kind:PeriodicKind::DrainResource,amount:n(100),first_delay:1,interval:t(1),pulses:t(10),refresh:PeriodicRefresh::Replace}).unwrap();advance(&mut w,4);let(mut out,mut combat)=(Vec::new(),Vec::new());let r=w.resolve_effects(10,&mut out,&mut combat).unwrap();assert_eq!(r.pulses,3);assert_eq!(out.iter().map(|e|e.scheduled_at).collect::<Vec<_>>(),vec![Tick(1),Tick(2),Tick(3)]);assert_eq!(out.last().unwrap().ended,Some(EffectEnd::ResourceDepleted));assert_eq!(w.resource(a).unwrap().current,0);assert!(w.shield(a).unwrap().is_none());
 if out.iter().any(|e|e.ended==Some(EffectEnd::ResourceDepleted)){w.cancel_effect(buff).unwrap();}w.resolve_attributes(&mut Vec::new());assert_eq!(w.combat_attributes(a).unwrap().attack,100);w.apply_resource(a,ResourceCommand::Restore(250)).unwrap();advance(&mut w,10);w.resolve_effects(10,&mut out,&mut combat).unwrap();assert!(out.is_empty());assert_eq!(w.resource(a).unwrap().current,250);
}

#[test]
fn timed_expirations_keep_admission_order_across_bounded_calls(){
 let mut w=World::default();let source=actor(&mut w,100,0);let mut ids=Vec::new();for _ in 0..256{let id=actor(&mut w,100,0);ids.push(id);w.grant_shield(absorb(source,id,10,Some(2))).unwrap();}advance(&mut w,2);let(mut all,mut out)=(Vec::new(),Vec::new());loop{let r=w.resolve_shields(37,&mut out).unwrap();assert!(out.len()<=37);all.extend(out.iter().map(|e|e.target));if r.overdue.is_none(){break;}}assert_eq!(all,ids);
}

#[test]
fn v4_self_effect_and_zero_hit_survive_pending_ack_and_feedback_backpressure(){
 let mut w=World::default();let a=actor(&mut w,100,0);w.grant_shield(absorb(a,a,5,None)).unwrap();w.apply_periodic(PeriodicEffect{source:a,target:a,key:1,kind:PeriodicKind::Damage,amount:n(5),first_delay:1,interval:t(1),pulses:t(2),refresh:PeriodicRefresh::Replace}).unwrap();
 let mut frame=CombatFrame::new(n(16));let mut sender=CombatSyncSender::new_shielded(7,CombatSyncConfig{pending_events:4,events_per_packet:1}).unwrap();let mut replica=CombatReplica::new(7);let view=ViewRect{min:GridPosition{x:-1,y:-1},max:GridPosition{x:1,y:1}};let(mut effects,mut combat)=(Vec::new(),Vec::new());
 advance(&mut w,1);w.resolve_effects(10,&mut effects,&mut combat).unwrap();frame.capture(&w,&combat).unwrap();sender.observe(&frame,view).unwrap();let first=sender.prepare().unwrap().unwrap().to_vec();let ack1=replica.apply(&first).unwrap().acknowledgement();
 advance(&mut w,1);combat.clear();w.resolve_effects(10,&mut effects,&mut combat).unwrap();frame.capture(&w,&combat).unwrap();sender.observe(&frame,view).unwrap();assert_eq!(sender.pending_packet().unwrap(),first);assert!(replica.apply(&first).unwrap().duplicate);sender.acknowledge_packet(&ack1).unwrap();let second=sender.prepare().unwrap().unwrap().to_vec();assert_eq!(replica.apply(&second),Err(CombatSyncError::EventBackpressure));
 let feedback=replica.drain_feedback().collect::<Vec<_>>();assert_eq!(feedback.len(),1);assert_eq!(feedback[0].attacker,feedback[0].target);assert_eq!(feedback[0].kind,CombatFeedbackKind::Hit{damage:0,remaining:100});let ack2=replica.apply(&second).unwrap().acknowledgement();assert_eq!(replica.drain_feedback().next().unwrap().kind,CombatFeedbackKind::Hit{damage:5,remaining:95});assert!(replica.apply(&second).unwrap().duplicate);assert_eq!(replica.drain_feedback().count(),0);sender.acknowledge_packet(&ack2).unwrap();assert_eq!(sender.queued_events(),0);assert_eq!(w.health(a).unwrap().current,95);
}
