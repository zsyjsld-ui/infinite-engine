use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn p(x:i32,y:i32)->GridPosition{GridPosition{x,y}}
fn view()->ViewRect{ViewRect{min:p(-20,-20),max:p(20,20)}}
fn row(kind:ComponentKind,instance:u32,scope:ComponentScope,value:u32)->ComponentRecord{
    ComponentRecord{key:ComponentKey{kind,instance},scope,fields:ComponentFields::from_words(&[value]).unwrap()}
}
fn appearance(value:u32)->ComponentRecord{row(ComponentKind::Appearance,0,ComponentScope::Nearby,value)}
fn capture(frame:&mut ComponentFrame,w:&mut World)->ComponentCaptureReport{w.advance_tick().unwrap();frame.capture(w).unwrap()}

#[test]
fn shared_public_views_are_bounded_and_never_cache_owner_or_previous_capture() {
    let mut world = World::default();
    let a = world.spawn(p(0,0)).unwrap(); let b = world.spawn(p(30,0)).unwrap();
    for (id, value) in [(a,11),(b,22)] {
        world.set_component(id, appearance(value)).unwrap();
        world.set_component(id, row(ComponentKind::Skill,1,ComponentScope::Owner,value)).unwrap();
    }
    let mut frame = ComponentFrame::new(n(32)); capture(&mut frame, &mut world);
    let mut bound = 0;
    for width in 1..=16 {
        let rect = ViewRect{min:p(-1,-1),max:p(width*4,1)};
        for owner in [Some(a),Some(b),None] {
            let actual = ComponentSelector::new(owner).select(&frame,rect).unwrap().to_vec();
            let mut expected = Vec::new();
            for id in [a,b] { for &record in world.components(id).unwrap() {
                let include = if record.scope == ComponentScope::Owner { owner == Some(id) }
                    else { world.position(id).unwrap().x <= rect.max.x };
                if include { expected.push(VisibleComponent{entity:id,record}); }
            }}
            assert_eq!(actual,expected);
        }
        if width==8 { bound=frame.public_view_cache_bytes(); assert!(bound>0); }
        if width>8 { assert_eq!(frame.public_view_cache_bytes(),bound); }
    }
    world.set_position(a,p(100,0)).unwrap();world.set_component(b,appearance(33)).unwrap();
    capture(&mut frame,&mut world);
    let rect=ViewRect{min:p(-1,-1),max:p(40,1)};
    let rows=ComponentSelector::new(None).select(&frame,rect).unwrap().to_vec();
    assert_eq!(rows,vec![VisibleComponent{entity:b,record:appearance(33)}]);
}

#[test]
fn selector_incremental_scope_removal_skipped_captures_and_slot_reuse_match_world(){
 let mut w=World::default();let owner=w.spawn(p(0,0)).unwrap();let remote=w.spawn(p(1000,0)).unwrap();w.set_component(owner,appearance(1)).unwrap();w.set_component(remote,row(ComponentKind::GroundItem,0,ComponentScope::Map,2)).unwrap();
 let mut frame=ComponentFrame::new(n(32));let mut selector=ComponentSelector::new(Some(owner));
 let check=|w:&World,frame:&ComponentFrame,selector:&mut ComponentSelector|{
  let mut expected=Vec::new();for motion in w.motion_states(){for &record in w.components(motion.entity).unwrap(){if record.scope==ComponentScope::Map||(record.scope==ComponentScope::Owner&&motion.entity==owner)||(record.scope==ComponentScope::Nearby&&motion.position.x>=-20&&motion.position.x<=20&&motion.position.y>=-20&&motion.position.y<=20){expected.push(VisibleComponent{entity:motion.entity,record});}}}
  expected.sort_unstable_by_key(|r|((r.entity.index,r.entity.generation),r.record.key));assert_eq!(selector.select(frame,view()).unwrap(),expected);
 };
 capture(&mut frame,&mut w);check(&w,&frame,&mut selector);
 for i in 0..4 { w.set_component(remote,row(ComponentKind::GroundItem,0,if i%2==0{ComponentScope::Nearby}else{ComponentScope::Map},i)).unwrap();w.set_component(owner,row(ComponentKind::Skill,1,ComponentScope::Owner,i)).unwrap();capture(&mut frame,&mut w);check(&w,&frame,&mut selector); }
 w.remove_component(remote,ComponentKey{kind:ComponentKind::GroundItem,instance:0}).unwrap();capture(&mut frame,&mut w);w.set_component(owner,appearance(9)).unwrap();capture(&mut frame,&mut w);check(&w,&frame,&mut selector);
 w.despawn(remote).unwrap();let replacement=w.spawn(p(1000,0)).unwrap();assert_eq!(replacement.index,remote.index);w.set_component(replacement,row(ComponentKind::GroundItem,0,ComponentScope::Map,7)).unwrap();capture(&mut frame,&mut w);check(&w,&frame,&mut selector);assert!(frame.entity_records(remote).is_empty());
}

#[test]
fn missing_zero_extremes_and_scope_validation_are_distinct(){
    let mut fields=ComponentFields::default();assert_eq!(fields.get(0),None);fields.set(0,0).unwrap();fields.set(15,u32::MAX).unwrap();assert_eq!(fields.mask(),0x8001);assert_eq!(fields.get(0),Some(0));assert_eq!(fields.get(15),Some(u32::MAX));fields.remove(0).unwrap();assert_eq!(fields.get(0),None);assert_eq!(fields.set(16,0),Err(ComponentError::InvalidField));assert_eq!(fields.remove(usize::MAX),Err(ComponentError::InvalidField));assert_eq!(fields.get(usize::MAX),None);
    let mut w=World::default();let id=w.spawn(p(0,0)).unwrap();assert_eq!(w.set_component(id,row(ComponentKind::Skill,1,ComponentScope::Nearby,0)),Err(ComponentError::InvalidScope));assert_eq!(w.component_count(),0);
}
#[test]
fn incremental_capture_preserves_frozen_values_and_noop_writes(){
    let mut w=World::default();let a=w.spawn(p(0,0)).unwrap();let b=w.spawn(p(1,0)).unwrap();w.set_component(a,appearance(1)).unwrap();w.set_component(b,appearance(2)).unwrap();let mut f=ComponentFrame::new(n(32));let first=capture(&mut f,&mut w);assert!(first.rebuilt);assert_eq!((first.changed_entities,first.records),(2,2));
    assert!(!w.set_component(a,appearance(1)).unwrap());assert_eq!(capture(&mut f,&mut w).changed_entities,0);w.set_component(a,appearance(7)).unwrap();assert_eq!(f.entity_records(a)[0].fields.get(0),Some(1));let update=capture(&mut f,&mut w);assert!(!update.rebuilt);assert_eq!(update.changed_entities,1);assert_eq!(f.entity_records(a)[0].fields.get(0),Some(7));assert_eq!(f.entity_records(b)[0].fields.get(0),Some(2));
}
#[test]
fn owner_nearby_and_map_views_never_share_private_state(){
    let mut w=World::default();let owner=w.spawn(p(1000,1000)).unwrap();let near=w.spawn(p(0,0)).unwrap();let drop=w.spawn(p(5000,5000)).unwrap();w.set_component(owner,row(ComponentKind::Skill,1,ComponentScope::Owner,10)).unwrap();w.set_component(near,row(ComponentKind::Skill,1,ComponentScope::Owner,20)).unwrap();w.set_component(near,appearance(3)).unwrap();w.set_component(drop,row(ComponentKind::GroundItem,0,ComponentScope::Map,30)).unwrap();let mut f=ComponentFrame::new(n(32));capture(&mut f,&mut w);
    let mut selector=ComponentSelector::new(Some(owner));let selected=selector.select(&f,view()).unwrap().to_vec();assert_eq!(selected.len(),3);assert!(selected.iter().any(|r|r.entity==owner));assert!(selected.iter().any(|r|r.entity==drop));assert!(!selected.iter().any(|r|r.entity==near&&r.record.key.kind==ComponentKind::Skill));
    let mut anonymous=ComponentSelector::new(None);assert_eq!(anonymous.select(&f,view()).unwrap().len(),2);w.set_position(near,p(100,100)).unwrap();capture(&mut f,&mut w);assert_eq!(selector.select(&f,view()).unwrap().len(),2);
}
#[test]
fn despawn_dense_swap_and_generation_reuse_remove_only_old_components(){
    let mut w=World::default();let a=w.spawn(p(0,0)).unwrap();let b=w.spawn(p(1,0)).unwrap();w.set_component(a,appearance(1)).unwrap();w.set_component(b,appearance(2)).unwrap();let mut f=ComponentFrame::new(n(32));capture(&mut f,&mut w);w.despawn(a).unwrap();let new=w.spawn(p(2,0)).unwrap();assert_eq!(new.index,a.index);assert_ne!(new.generation,a.generation);w.set_component(new,appearance(3)).unwrap();capture(&mut f,&mut w);assert!(f.entity_records(a).is_empty());assert_eq!(w.components(b).unwrap()[0],appearance(2));assert_eq!(f.entity_records(new)[0],appearance(3));assert_eq!(w.component_count(),2);assert_eq!(w.set_component(a,appearance(4)),Err(ComponentError::InvalidEntity));
}
#[test]
fn journal_overflow_rebuilds_complete_truth_and_cross_world_is_rejected(){
    let mut w=World::default();let id=w.spawn(p(0,0)).unwrap();w.set_component(id,appearance(0)).unwrap();let mut f=ComponentFrame::new(n(32));capture(&mut f,&mut w);for v in 1..=COMPONENT_JOURNAL_CAPACITY+3{w.set_component(id,appearance(v as u32)).unwrap();}let report=capture(&mut f,&mut w);assert!(report.rebuilt);assert_eq!(report.records,1);assert_eq!(f.entity_records(id)[0].fields.get(0),Some((COMPONENT_JOURNAL_CAPACITY+3)as u32));let other=World::default();assert_eq!(f.capture(&other),Err(ComponentError::WrongWorld));assert_eq!(f.capture(&w),Err(ComponentError::State(SyncError::StaleTick)));
}
#[test]
fn replacement_limit_and_duplicate_fail_atomically(){
    let mut w=World::default();let e=w.spawn(p(0,0)).unwrap();for i in 0..MAX_COMPONENTS_PER_ENTITY{w.set_component(e,row(ComponentKind::Effect,i as u32,ComponentScope::Owner,i as u32)).unwrap();}assert_eq!(w.set_component(e,appearance(1)),Err(ComponentError::EntityLimit));let before=w.components(e).unwrap().to_vec();assert_eq!(w.replace_component_kind(e,ComponentKind::Effect,&[before[0],before[0]]),Err(ComponentError::DuplicateKey));assert_eq!(w.components(e).unwrap(),before);w.replace_component_kind(e,ComponentKind::Effect,&[before[1]]).unwrap();assert_eq!(w.component_count(),1);assert!(w.remove_component(e,before[1].key).unwrap());assert!(!w.remove_component(e,before[1].key).unwrap());assert_eq!(w.component_count(),0);
}
#[test]
fn wire_roundtrip_changes_fields_removes_records_and_reenters_with_full_state(){
    let mut w=World::default();let a=w.spawn(p(0,0)).unwrap();let b=w.spawn(p(1,0)).unwrap();let mut component=appearance(0);component.fields.set(15,u32::MAX).unwrap();w.set_component(a,component).unwrap();w.set_component(b,appearance(9)).unwrap();let mut f=ComponentFrame::new(n(32));let mut sender=ComponentSender::new(7,Some(a));let mut replica=ComponentReplica::new(7);capture(&mut f,&mut w);
    let packet=sender.prepare(&f,view()).unwrap().unwrap().to_vec();let ack=replica.apply(&packet).unwrap().acknowledgement();sender.acknowledge_packet(&ack).unwrap();assert_eq!(replica.states().len(),2);assert_eq!(replica.states()[0].record.fields.get(0),Some(0));
    component.fields.remove(15).unwrap();component.fields.set(1,128).unwrap();w.set_component(a,component).unwrap();w.set_position(b,p(1000,1000)).unwrap();capture(&mut f,&mut w);let packet=sender.prepare(&f,view()).unwrap().unwrap().to_vec();let ack=replica.apply(&packet).unwrap().acknowledgement();sender.acknowledge_packet(&ack).unwrap();assert_eq!(replica.states().len(),1);assert_eq!(replica.states()[0].record,component);
    w.set_position(b,p(0,0)).unwrap();w.remove_component(a,component.key).unwrap();capture(&mut f,&mut w);let ack=replica.apply(sender.prepare(&f,view()).unwrap().unwrap()).unwrap().acknowledgement();sender.acknowledge_packet(&ack).unwrap();assert_eq!(replica.states(),&[VisibleComponent{entity:b,record:appearance(9)}]);capture(&mut f,&mut w);assert!(sender.prepare(&f,view()).unwrap().is_none());
}
#[test]
fn ack_loss_keeps_bytes_and_later_snapshot_does_not_rewrite_pending(){
    let mut w=World::default();let a=w.spawn(p(0,0)).unwrap();w.set_component(a,appearance(1)).unwrap();let mut f=ComponentFrame::new(n(32));capture(&mut f,&mut w);let mut sender=ComponentSender::new(10,None);let mut replica=ComponentReplica::new(10);let packet=sender.prepare(&f,view()).unwrap().unwrap().to_vec();let ack=replica.apply(&packet).unwrap().acknowledgement();w.set_component(a,appearance(2)).unwrap();capture(&mut f,&mut w);assert_eq!(sender.prepare(&f,view()),Err(ComponentError::State(SyncError::WaitingForAck)));assert_eq!(sender.pending_packet(),Some(packet.as_slice()));assert!(replica.apply(&packet).unwrap().duplicate);assert_eq!(sender.acknowledge(11,1),Err(ComponentError::State(SyncError::WrongStream)));assert_eq!(sender.acknowledge(10,2),Err(ComponentError::State(SyncError::OutOfOrder)));sender.acknowledge_packet(&ack).unwrap();sender.acknowledge_packet(&ack).unwrap();let ack=replica.apply(sender.prepare(&f,view()).unwrap().unwrap()).unwrap().acknowledgement();sender.acknowledge_packet(&ack).unwrap();assert_eq!(replica.states()[0].record,appearance(2));
}
#[test]
fn malformed_packets_leave_replica_unchanged_and_retry_remains_valid(){
    let mut w=World::default();let id=w.spawn(p(0,0)).unwrap();w.set_component(id,appearance(300)).unwrap();let mut f=ComponentFrame::new(n(32));capture(&mut f,&mut w);let mut sender=ComponentSender::new(3,None);let packet=sender.prepare(&f,view()).unwrap().unwrap().to_vec();let mut replica=ComponentReplica::new(3);
    for end in 0..packet.len(){assert!(replica.apply(&packet[..end]).is_err());assert!(replica.states().is_empty());assert_eq!(replica.sequence(),0);}
    let mut trailing=packet.clone();trailing.push(0);assert!(replica.apply(&trailing).is_err());let mut flags=packet.clone();flags[3]=2;assert!(replica.apply(&flags).is_err());let mut reserved=packet.clone();reserved[38]=1;assert!(replica.apply(&reserved).is_err());let ack=replica.apply(&packet).unwrap().acknowledgement();sender.acknowledge_packet(&ack).unwrap();let old=replica.states().to_vec();let mut conflicting=packet.clone();*conflicting.last_mut().unwrap()^=1;assert!(replica.apply(&conflicting).is_err());assert_eq!(replica.states(),old);
    w.set_component(id,appearance(400)).unwrap();capture(&mut f,&mut w);let next=sender.prepare(&f,view()).unwrap().unwrap().to_vec();let mut missing_base=next.clone();missing_base[20..28].copy_from_slice(&0u64.to_le_bytes());assert!(replica.apply(&missing_base).is_err());assert_eq!(replica.states(),old);replica.apply(&next).unwrap();assert_eq!(replica.states()[0].record,appearance(400));
}
#[test]
fn full_request_during_pending_preserves_packet_then_resets_baseline(){
    let mut w=World::default();let id=w.spawn(p(0,0)).unwrap();w.set_component(id,appearance(1)).unwrap();let mut f=ComponentFrame::new(n(32));capture(&mut f,&mut w);let mut sender=ComponentSender::new(1,None);let first=sender.prepare(&f,view()).unwrap().unwrap().to_vec();sender.request_full();assert_eq!(sender.pending_packet(),Some(first.as_slice()));sender.acknowledge(1,1).unwrap();let next=sender.prepare(&f,view()).unwrap().unwrap().to_vec();assert_eq!(next[3],1);assert_eq!(sender.pending_header().unwrap().base,0);let mut fresh=ComponentReplica::new(1);fresh.apply(&next).unwrap();assert_eq!(fresh.states()[0].record,appearance(1));
    let mut other=World::default();other.spawn(p(0,0)).unwrap();let mut other_frame=ComponentFrame::new(n(32));capture(&mut other_frame,&mut other);sender.acknowledge(1,2).unwrap();assert_eq!(sender.prepare(&other_frame,view()),Err(ComponentError::WrongWorld));
}
#[test]
fn unchanged_newer_observation_still_prevents_older_snapshot_regression(){
    let mut w=World::default();let id=w.spawn(p(0,0)).unwrap();w.set_component(id,appearance(1)).unwrap();let mut first=ComponentFrame::new(n(32));capture(&mut first,&mut w);let mut sender=ComponentSender::new(4,None);sender.prepare(&first,view()).unwrap();sender.acknowledge(4,1).unwrap();
    w.set_component(id,appearance(2)).unwrap();let mut middle=ComponentFrame::new(n(32));capture(&mut middle,&mut w);w.set_component(id,appearance(1)).unwrap();let mut newest=ComponentFrame::new(n(32));capture(&mut newest,&mut w);assert!(sender.prepare(&newest,view()).unwrap().is_none());assert_eq!(sender.prepare(&middle,view()),Err(ComponentError::State(SyncError::StaleTick)));assert!(sender.pending_packet().is_none());
}
#[test]
fn noncanonical_varints_and_invalid_scope_are_rejected_before_commit(){
    let mut w=World::default();let id=w.spawn(p(0,0)).unwrap();w.set_component(id,appearance(0)).unwrap();let mut frame=ComponentFrame::new(n(32));capture(&mut frame,&mut w);let mut sender=ComponentSender::new(3,None);let packet=sender.prepare(&frame,view()).unwrap().unwrap().to_vec();let mut client=ComponentReplica::new(3);
    let mut oversized=packet.clone();oversized.splice(40..41,[0x80,0x80,0x80,0x80,0x10]);assert!(client.apply(&oversized).is_err());let mut noncanonical=packet.clone();noncanonical.splice(40..41,[0x80,0]);assert!(client.apply(&noncanonical).is_err());let mut scope=packet.clone();scope[45]=1;assert!(client.apply(&scope).is_err());assert_eq!(client.sequence(),0);assert!(client.states().is_empty());client.apply(&packet).unwrap();
}
#[test]
fn full_view_generation_replacement_handles_maximum_change_count_and_backpressure(){
    let mut w=World::default();let mut ids=Vec::new();for _ in 0..MAX_COMPONENT_VIEW_RECORDS/MAX_COMPONENTS_PER_ENTITY{let id=w.spawn(p(0,0)).unwrap();ids.push(id);for i in 0..MAX_COMPONENTS_PER_ENTITY{w.set_component(id,row(ComponentKind::GroundItem,i as u32,ComponentScope::Map,i as u32)).unwrap();}}
    let mut frame=ComponentFrame::new(n(32));capture(&mut frame,&mut w);let mut sender=ComponentSender::new(20,None);let mut replica=ComponentReplica::new(20);let ack=replica.apply(sender.prepare(&frame,view()).unwrap().unwrap()).unwrap().acknowledgement();sender.acknowledge_packet(&ack).unwrap();assert_eq!(replica.states().len(),MAX_COMPONENT_VIEW_RECORDS);
    for id in ids{w.despawn(id).unwrap();}for _ in 0..MAX_COMPONENT_VIEW_RECORDS/MAX_COMPONENTS_PER_ENTITY{let id=w.spawn(p(0,0)).unwrap();for i in 0..MAX_COMPONENTS_PER_ENTITY{w.set_component(id,row(ComponentKind::GroundItem,i as u32,ComponentScope::Map,(i+1)as u32)).unwrap();}}capture(&mut frame,&mut w);let packet=sender.prepare(&frame,view()).unwrap().unwrap().to_vec();assert_eq!(u16::from_le_bytes(packet[36..38].try_into().unwrap())as usize,MAX_COMPONENT_VIEW_RECORDS*2);let ack=replica.apply(&packet).unwrap().acknowledgement();sender.acknowledge_packet(&ack).unwrap();assert_eq!(replica.states().len(),MAX_COMPONENT_VIEW_RECORDS);assert!(replica.states().iter().all(|r|r.entity.generation==1));
    let extra=w.spawn(p(0,0)).unwrap();w.set_component(extra,row(ComponentKind::GroundItem,0,ComponentScope::Map,1)).unwrap();capture(&mut frame,&mut w);assert_eq!(sender.prepare(&frame,view()),Err(ComponentError::ViewLimit));assert!(sender.pending_packet().is_none());w.despawn(extra).unwrap();capture(&mut frame,&mut w);assert!(sender.prepare(&frame,view()).unwrap().is_none());assert_eq!(replica.sequence(),2);
}
#[test]
fn ground_ownership_uses_stable_u64_ids_and_preserves_omitted_empty_and_full_members(){
    let ids=[0,42,u32::MAX as u64+1,u64::MAX-1,u64::MAX];let full=ground_members_component(Some(&ids)).unwrap();assert_eq!(full.scope,ComponentScope::Map);for(i,&id)in ids.iter().enumerate(){let value=u64::from(full.fields.get(1+i*2).unwrap())|(u64::from(full.fields.get(2+i*2).unwrap())<<32);assert_eq!(value,id);}assert_ne!(ground_members_component(None).unwrap(),ground_members_component(Some(&[])).unwrap());assert_eq!(ground_members_component(Some(&[1,1])),Err(ComponentError::DuplicateKey));assert_eq!(ground_members_component(Some(&[1,2,3,4,5,6])),Err(ComponentError::EntityLimit));
    let mut w=World::default();let source=w.spawn(p(0,0)).unwrap();let drop=w.spawn(p(5000,0)).unwrap();let record=GroundItemComponent{definition:1,count:n(1),position:p(5000,0),height:0,source,from_monster:true,exclusive_owner:Some(u64::MAX),exclusive_until:Tick(1u64<<40),expires_at:Tick(1u64<<41)}.record();w.set_component(drop,record).unwrap();w.set_component(drop,full).unwrap();w.despawn(source).unwrap();w.spawn(p(0,0)).unwrap();let mut frame=ComponentFrame::new(n(32));capture(&mut frame,&mut w);let mut sender=ComponentSender::new_with_definitions(5,None);let mut client=ComponentReplica::new_with_definitions(5);client.apply(sender.prepare(&frame,view()).unwrap().unwrap()).unwrap();assert_eq!(client.states(),&[VisibleComponent{entity:drop,record},VisibleComponent{entity:drop,record:full}]);assert_eq!(record.fields.get(9),Some(u32::MAX));assert_eq!(record.fields.get(10),Some(u32::MAX));
}
#[test]
fn actual_effect_and_skill_projections_use_deadlines_and_remove_finished_state(){
    let mut w=World::default();let a=w.spawn(p(0,0)).unwrap();let b=w.spawn(p(1,0)).unwrap();for id in [a,b]{w.configure_health(id,100,n(100)).unwrap();}w.learn_skill(a,SkillSpec{id:SkillId(5),resource_cost:0,cooldown_ticks:10,windup_ticks:2,recovery:None,ingress_grace_ticks:0}).unwrap();assert!(w.publish_skill_component(a,SkillId(5)).unwrap());assert!(!w.publish_skill_component(a,SkillId(5)).unwrap());
    let effect=w.apply_periodic(PeriodicEffect{source:a,target:b,key:7,kind:PeriodicKind::Damage,amount:n(3),first_delay:1,interval:NonZeroU16::new(1).unwrap(),pulses:NonZeroU16::new(2).unwrap(),refresh:PeriodicRefresh::Replace}).unwrap();
    w.publish_effect_components(b,ComponentScope::Owner).unwrap();assert_eq!(w.components(b).unwrap().len(),1);let before=w.components(b).unwrap()[0];w.publish_effect_components(b,ComponentScope::Nearby).unwrap();assert!(w.components(b).unwrap()[0].fields.get(7).is_none());assert_eq!(before.fields.get(7),Some(3));let(mut effects,mut combat)=(Vec::new(),Vec::new());for _ in 0..2{w.advance_tick().unwrap();w.resolve_effects(100,&mut effects,&mut combat).unwrap();w.publish_effect_components(b,ComponentScope::Owner).unwrap();}assert!(w.components(b).unwrap().is_empty());assert_eq!(w.health(b).unwrap().current,94);let _=effect;
}
