use infinite_core::*;use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn actor(w:&mut World,resource:u32)->EntityId{let id=w.spawn(GridPosition::default()).unwrap();w.configure_health(id,100,n(100)).unwrap();w.configure_resource(id,resource,100).unwrap();id}
fn spec(id:u16)->SkillSpec{SkillSpec{id:SkillId(id),resource_cost:10,cooldown_ticks:10,windup_ticks:2,recovery:None,ingress_grace_ticks:0}}
fn request(caster:EntityId,id:u64,skill:u16)->CastRequest{CastRequest{caster,request_id:id,skill:SkillId(skill),target:CastTarget::Entity(caster)}}

#[test]
fn indexed_deadlines_match_sorted_reference_under_cancel_reschedule_and_reuse(){
 let mut queue=TickQueue::new(512).unwrap();let mut reference=Vec::new();
 for i in 0..512u64{let due=Tick((i*37)%97);let id=queue.schedule(due,i).unwrap();reference.push((id,due,i));}
 assert_eq!(queue.schedule(Tick(0),9),Err(TickQueueError::Full));
 for i in (0..512).step_by(3){let (id,_,v)=reference[i];assert_eq!(queue.cancel(id).unwrap().value,v);assert!(queue.get(id).is_none());}
 reference.retain(|(_,_,v)|v%3!=0);
 for entry in &mut reference{if entry.2%5==0{entry.1=Tick(7);queue.reschedule(entry.0,entry.1).unwrap();}}
 let old=reference[0].0;queue.cancel(old).unwrap();reference.remove(0);let replacement=queue.schedule(Tick(7),999).unwrap();assert_ne!(old,replacement);assert_eq!(queue.reschedule(old,Tick(0)),Err(TickQueueError::InvalidTask));reference.push((replacement,Tick(7),999));
 reference.sort_by_key(|(_,due,order)|(*due,*order));
 let mut output=Vec::new();for tick in 0..=100{while let Some(task)=queue.pop_due(Tick(tick)){output.push((task.id,task.due,task.value));}}
 assert_eq!(output,reference);assert!(queue.is_empty());let capacity=queue.capacity_bytes();
 for i in 0..10000{let id=queue.schedule(Tick(i),i).unwrap();queue.cancel(id).unwrap();}assert_eq!(queue.capacity_bytes(),capacity);
}

#[test]
fn resource_spend_restore_and_attribute_maximum_are_atomic(){
 let mut w=World::default();let a=actor(&mut w,10);assert_eq!(w.apply_resource(a,ResourceCommand::Spend(11)),Err(ResourceError::Insufficient));assert_eq!(w.resource(a).unwrap().current,10);
 assert_eq!(w.apply_resource(a,ResourceCommand::Restore(u32::MAX)).unwrap().after.current,100);
 let attrs=CombatAttributes{attack:1,break_power:0,defense:0,maximum_health:n(100),maximum_resource:20,level:NonZeroU16::new(1).unwrap()};w.configure_attributes(a,attrs).unwrap();let mut out=Vec::new();w.resolve_attributes(&mut out);
 assert_eq!(w.resource(a).unwrap(),ResourceState{current:20,maximum:20});assert!(out[0].resource.unwrap().changed());
 w.set_base_attributes(a,CombatAttributes{maximum_resource:100,..attrs}).unwrap();w.resolve_attributes(&mut out);assert_eq!(w.resource(a).unwrap().current,20);
 w.apply_resource(a,ResourceCommand::SetMaximum(0)).unwrap();assert_eq!(w.resource(a).unwrap(),ResourceState{current:0,maximum:0});
 let b=w.spawn(GridPosition::default()).unwrap();w.configure_health(b,100,n(100)).unwrap();w.configure_attributes(b,attrs).unwrap();assert_eq!(w.configure_resource(b,10,20),Err(ResourceError::AttributesPending));w.resolve_attributes(&mut out);assert_eq!(w.configure_resource(b,10,100),Err(ResourceError::MaximumMismatch));w.configure_resource(b,10,20).unwrap();
}

#[test]
fn rejected_cast_does_not_consume_resource_cooldown_or_request_id(){
 let mut w=World::default();let a=actor(&mut w,5);w.learn_skill(a,spec(1)).unwrap();let r=request(a,1,1);
 assert_eq!(w.start_cast(r),Err(SkillError::Resource(ResourceError::Insufficient)));assert_eq!(w.skill_state(a,SkillId(1)).unwrap().cooldown_ready,Tick(0));assert_eq!(w.active_casts(),0);
 w.apply_resource(a,ResourceCommand::Restore(5)).unwrap();let admitted=w.start_cast(r).unwrap();assert!(!admitted.duplicate);assert_eq!(w.resource(a).unwrap().current,0);assert_eq!(admitted.receipt.cooldown_ready,Tick(10));
 assert!(w.start_cast(r).unwrap().duplicate);assert_eq!(w.resource(a).unwrap().current,0);
 assert_eq!(w.start_cast(CastRequest{target:CastTarget::Point(GridPosition::default()),..r}),Err(SkillError::RequestConflict));
 assert_eq!(w.start_cast(request(a,0,1)),Err(SkillError::StaleRequest));
 let cancel=w.cancel_cast(a).unwrap().unwrap();assert!(matches!(cancel,SkillEvent::Cancelled{reason:CastCancelReason::Manual,..}));assert_eq!(w.active_casts(),0);assert_eq!(w.skill_state(a,SkillId(1)).unwrap().cooldown_ready,Tick(10));assert!(w.start_cast(r).unwrap().duplicate);
}

#[test]
fn cooldown_is_per_skill_and_recovery_only_for_explicit_groups(){
 let mut w=World::default();let a=actor(&mut w,100);for id in 1..=4{let mut s=spec(id);s.windup_ticks=0;s.recovery=if id<=2{Some(SkillRecovery{group:3,ticks:5,ingress_grace_ticks:0})}else{None};w.learn_skill(a,s).unwrap();}
 let mut events=Vec::new();w.start_cast(request(a,1,1)).unwrap();w.resolve_casts(10,&mut events).unwrap();
 assert_eq!(w.start_cast(request(a,2,1)),Err(SkillError::CoolingDown));assert_eq!(w.start_cast(request(a,2,2)),Err(SkillError::Recovering));
 w.start_cast(request(a,2,3)).unwrap();w.resolve_casts(10,&mut events).unwrap();w.start_cast(request(a,3,4)).unwrap();w.resolve_casts(10,&mut events).unwrap();
 for _ in 0..5{w.advance_tick().unwrap();}w.start_cast(request(a,4,2)).unwrap();assert_eq!(w.resource(a).unwrap().current,60);
}

#[test]
fn ingress_grace_anchors_to_old_deadline_instead_of_increasing_rate(){
 let mut w=World::default();let a=actor(&mut w,100);let mut s=spec(1);s.windup_ticks=0;s.ingress_grace_ticks=1;w.learn_skill(a,s).unwrap();let mut out=Vec::new();
 w.start_cast(request(a,1,1)).unwrap();w.resolve_casts(1,&mut out).unwrap();for _ in 0..9{w.advance_tick().unwrap();}
 assert_eq!(w.start_cast(request(a,2,1)).unwrap().receipt.cooldown_ready,Tick(20));w.resolve_casts(1,&mut out).unwrap();
 for _ in 0..9{w.advance_tick().unwrap();}assert_eq!(w.start_cast(request(a,3,1)),Err(SkillError::CoolingDown));w.advance_tick().unwrap();assert_eq!(w.start_cast(request(a,3,1)).unwrap().receipt.cooldown_ready,Tick(30));
}

#[test]
fn due_queue_reports_budget_and_cancels_dead_or_reused_targets(){
 let mut w=World::default();let a=actor(&mut w,100);let b=actor(&mut w,100);let c=actor(&mut w,100);for id in [a,b,c]{w.learn_skill(id,spec(1)).unwrap();}
 w.start_cast(request(a,1,1)).unwrap();w.start_cast(CastRequest{target:CastTarget::Entity(c),..request(b,1,1)}).unwrap();
 let h=w.health(a).unwrap();let mut result=[Err(HealthError::InvalidEntity)];let mut change=[HealthChange{entity:a,before:h,after:h}];w.apply_health_commands(&[HealthCommand::Damage{entity:a,amount:100}],&mut result,&mut change).unwrap();w.despawn(c).unwrap();let new_c=actor(&mut w,100);assert_ne!(c,new_c);
 w.advance_tick().unwrap();w.advance_tick().unwrap();let mut out=Vec::new();let r=w.resolve_casts(1,&mut out).unwrap();assert_eq!(r.overdue,Some(Tick(2)));assert!(matches!(out[0],SkillEvent::Cancelled{reason:CastCancelReason::CasterDepleted,..}));
 assert_eq!(w.resolve_casts(1,&mut out).unwrap().overdue,None);assert!(matches!(out[0],SkillEvent::Cancelled{reason:CastCancelReason::TargetGone,..}));assert_eq!(w.resolve_casts(1,&mut out).unwrap().processed,0);
}

#[test]
fn despawn_reclaims_cast_and_skill_dense_rows_without_redirecting_replays(){
 let mut w=World::default();let a=actor(&mut w,100);let b=actor(&mut w,100);for id in [a,b]{w.learn_skill(id,spec(1)).unwrap();w.start_cast(request(id,1,1)).unwrap();}
 w.despawn(a).unwrap();assert_eq!(w.active_casts(),1);let replacement=actor(&mut w,100);w.learn_skill(replacement,spec(1)).unwrap();w.start_cast(request(replacement,1,1)).unwrap();
 w.advance_tick().unwrap();w.advance_tick().unwrap();let mut out=Vec::new();w.resolve_casts(10,&mut out).unwrap();let ids:Vec<_>=out.iter().map(|e|if let SkillEvent::Ready(c)=e{c.request.caster}else{panic!()}).collect();assert_eq!(ids,vec![b,replacement]);assert_eq!(w.resource(b).unwrap().current,90);assert_eq!(w.resource(replacement).unwrap().current,90);
}

#[test]
fn ready_skill_drives_actual_range_damage_only_once(){
 let mut w=World::default();let a=actor(&mut w,100);let b=actor(&mut w,100);w.learn_skill(a,spec(1)).unwrap();let r=request(a,1,1);w.start_cast(r).unwrap();
 w.advance_tick().unwrap();w.advance_tick().unwrap();let mut ready=Vec::new();w.resolve_casts(10,&mut ready).unwrap();let mut events=Vec::new();let mut actions=CombatActions::new(n(16));let mut budget=CombatQueryBudget::new(100);
 for event in &ready{if let SkillEvent::Ready(cast)=event{actions.begin(&mut w,&mut budget,&mut events,4096).unwrap().damage_area(AreaDamage{attacker:cast.request.caster,area:CombatArea::Circle{center:GridPosition::default(),radius:0},damage:n(17),require_sight:false},|_,id|id==b).unwrap();}}
 assert_eq!(w.health(b).unwrap().current,83);assert!(w.start_cast(r).unwrap().duplicate);assert_eq!(w.resolve_casts(10,&mut ready).unwrap().ready,0);assert_eq!(w.health(b).unwrap().current,83);
}

#[test]
fn full_cast_queue_rejects_before_charging_and_registration_is_bounded(){
 let mut w=World::default();let ids:Vec<_>=(0..MAX_ACTIVE_CASTS+1).map(|_|actor(&mut w,100)).collect();for &id in &ids{w.learn_skill(id,spec(1)).unwrap();}
 for &id in &ids[..MAX_ACTIVE_CASTS]{w.start_cast(request(id,1,1)).unwrap();}
 let last=ids[MAX_ACTIVE_CASTS];assert_eq!(w.start_cast(request(last,1,1)),Err(SkillError::Queue(TickQueueError::Full)));assert_eq!(w.resource(last).unwrap().current,100);assert_eq!(w.skill_state(last,SkillId(1)).unwrap().cooldown_ready,Tick(0));
 w.cancel_cast(ids[0]).unwrap();w.start_cast(request(last,1,1)).unwrap();assert_eq!(w.resource(last).unwrap().current,90);
 for id in 2..=MAX_SKILLS_PER_ENTITY as u16{w.learn_skill(last,spec(id)).unwrap();}assert_eq!(w.learn_skill(last,spec(99)),Err(SkillError::SkillCapacity));
 assert_eq!(w.learn_skill(last,SkillSpec{recovery:Some(SkillRecovery{group:16,ticks:1,ingress_grace_ticks:0}),..spec(1)}),Err(SkillError::InvalidSpec));
}
