use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn view()->ViewRect{ViewRect{min:p(-512),max:p(512)}}
fn actor(w:&mut World,x:i32,hp:u32)->EntityId{let e=w.spawn(p(x)).unwrap();w.configure_health(e,hp,n(hp)).unwrap();e}
fn setup()->(World,EntityId,EntityId,CombatFrame,CombatSyncSender,CombatReplica){
    let mut w=World::default();let a=actor(&mut w,0,50);let b=actor(&mut w,64,50);
    (w,a,b,CombatFrame::new(n(512)),CombatSyncSender::new(17,CombatSyncConfig::default()).unwrap(),CombatReplica::new(17))
}
fn profile(windup:u16)->AttackProfile{AttackProfile{range:96,damage:n(10),cooldown_ticks:NonZeroU16::new(2).unwrap(),windup_ticks:windup}}
fn phase(w:&mut World)->Vec<CombatEvent>{
    w.advance_tick().unwrap();let id=EntityId{index:0,generation:0};let h=HealthState{current:1,maximum:n(1)};
    let mut events=vec![CombatEvent{attacker:id,target:id,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};w.active_combatants()];
    let mut changes=vec![HealthChange{entity:id,before:h,after:h};w.active_combatants()];
    let r=w.resolve_combat(&mut events,&mut changes).unwrap();events.truncate(r.events_written);events
}
fn health(w:&mut World,commands:&[HealthCommand]){
    let id=EntityId{index:0,generation:0};let h=HealthState{current:1,maximum:n(1)};
    let mut out=vec![Err(HealthError::Unconfigured);commands.len()];let mut changes=vec![HealthChange{entity:id,before:h,after:h};commands.len()];
    w.apply_health_commands(commands,&mut out,&mut changes).unwrap();assert!(out.iter().all(Result::is_ok));
}
fn deliver(s:&mut CombatSyncSender,c:&mut CombatReplica)->(Vec<u8>,Vec<CombatFeedback>){
    let bytes=s.prepare().unwrap().unwrap().to_vec();let r=c.apply(&bytes).unwrap();assert!(!r.duplicate);
    let events=c.drain_feedback().collect();s.acknowledge_packet(&r.acknowledgement()).unwrap();(bytes,events)
}
fn capture(w:&mut World,f:&mut CombatFrame,s:&mut CombatSyncSender){w.advance_tick().unwrap();f.capture(w,&[]).unwrap();s.observe(f,view()).unwrap();}
fn hp(c:&CombatReplica,id:EntityId)->Option<HealthState>{c.states().iter().find(|s|s.entity==id).unwrap().health}

#[test]
fn cached_views_match_fresh_senders_during_movement_churn_and_view_changes(){
    let mut w=World::default();let mut ids:Vec<_>=(0..16).map(|i|actor(&mut w,(i-8)*32,1000)).collect();
    for i in (0..16).step_by(2){w.start_attack(ids[i],ids[i+1],profile(2)).unwrap();}
    let mut f=CombatFrame::new(n(64));let mut senders:Vec<_>=(1..=3).map(|id|CombatSyncSender::new(id,CombatSyncConfig{events_per_packet:2,..CombatSyncConfig::default()}).unwrap()).collect();let mut clients:Vec<_>=(1..=3).map(CombatReplica::new).collect();
    for t in 1..=48{
        if t%4==0{let i=(t*3%16)as usize;w.set_position(ids[i],p(if t%8==0{65}else{-65})).unwrap();}
        if t%7==0{let i=(t%16)as usize;w.despawn(ids[i]).unwrap();ids[i]=actor(&mut w,if i%2==0{-64}else{64},700+t);}
        if t%12==0{for i in (0..16).step_by(2){w.stop_attack(ids[i]).unwrap();w.start_attack(ids[i],ids[i+1],profile(2)).unwrap();}}
        let events=phase(&mut w);f.capture(&w,&events).unwrap();
        for r in 0..3{
            let v=match r{0=>ViewRect{min:p(-64),max:p(64)},1=>{let x=if t%6<3{-96}else{0};ViewRect{min:p(x),max:p(x+128)}},_=>ViewRect{min:p(5000),max:p(5010)}};
            if t%9==0{assert_eq!(senders[r].observe(&f,ViewRect{min:p(1),max:p(0)}),Err(SyncError::InvalidView.into()));}
            let mut reference=CombatSyncSender::new(99,CombatSyncConfig{events_per_packet:2,..CombatSyncConfig::default()}).unwrap();let mut replica=CombatReplica::new(99);
            reference.observe(&f,v).unwrap();senders[r].observe(&f,v).unwrap();
            let drain=|s:&mut CombatSyncSender,c:&mut CombatReplica|{let mut out=Vec::new();while let Some(packet)=s.prepare().unwrap(){let report=c.apply(packet).unwrap();out.extend(c.drain_feedback());s.acknowledge_packet(&report.acknowledgement()).unwrap();}out};
            assert_eq!(drain(&mut senders[r],&mut clients[r]),drain(&mut reference,&mut replica));assert_eq!(clients[r].states(),replica.states());
            let mut visible:Vec<_>=w.motion_states().filter(|s|v.contains(s.position)).map(|s|s.entity).collect();visible.sort_unstable_by_key(|e|(e.index,e.generation));
            assert_eq!(clients[r].states().iter().map(|s|s.entity).collect::<Vec<_>>(),visible);
            for s in clients[r].states(){assert_eq!(s.health,w.health(s.entity).ok());let windup=w.attack_state(s.entity).ok().and_then(|a|a.windup).filter(|a|visible.contains(&a.target)).map(|a|WindupView{target:a.target,started_at:a.started_at,hit_at:a.hit_at});assert_eq!(s.windup,windup);}
        }
    }
}

#[test]
fn dense_capture_survives_swap_removal_reuse_and_later_world_changes(){
    let(mut w,a,b,mut f,mut s,mut c)=setup();let tail=actor(&mut w,128,70);
    w.despawn(b).unwrap();let reused=actor(&mut w,-128,90);assert_eq!(reused.index,b.index);assert_ne!(reused.generation,b.generation);
    w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();
    // Mutating the world after capture must not change this snapshot's rows.
    w.despawn(a).unwrap();w.set_position(tail,p(10000)).unwrap();health(&mut w,&[HealthCommand::Damage{entity:reused,amount:87},HealthCommand::SetMaximum{entity:reused,maximum:n(9)}]);
    s.observe(&f,view()).unwrap();deliver(&mut s,&mut c);
    assert_eq!(hp(&c,a).unwrap().current,50);assert_eq!(hp(&c,tail).unwrap().current,70);assert_eq!(hp(&c,reused).unwrap().current,90);
    capture(&mut w,&mut f,&mut s);deliver(&mut s,&mut c);
    assert_eq!(c.states().len(),1);assert_eq!(hp(&c,reused).unwrap().current,3);
    w.set_position(tail,p(64)).unwrap();capture(&mut w,&mut f,&mut s);deliver(&mut s,&mut c);
    assert_eq!(c.states().len(),2);assert_eq!(hp(&c,tail).unwrap().current,70);
}

#[test]
fn event_bounds_match_visible_membership_in_two_dimensions_and_at_extremes(){
    let mut w=World::default();let positions=[GridPosition{x:i32::MIN,y:-10},GridPosition{x:-2,y:1},GridPosition{x:1,y:-2},GridPosition{x:i32::MAX,y:10}];
    let mut ids:Vec<_>=positions.iter().map(|&p|w.spawn(p).unwrap()).collect();let stale=ids[1];w.despawn(stale).unwrap();ids[1]=w.spawn(positions[1]).unwrap();
    let mut events=Vec::new();for &a in &ids{for &b in &ids{if a!=b{events.push(CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)});}}}
    events.push(CombatEvent{attacker:stale,target:ids[2],kind:CombatEventKind::Ended(CombatEndReason::TargetGone)});
    w.advance_tick().unwrap();let mut f=CombatFrame::new(n(u32::MAX));f.capture(&w,&events).unwrap();
    for rect in [ViewRect{min:GridPosition{x:i32::MIN,y:-10},max:GridPosition{x:i32::MAX,y:10}},ViewRect{min:GridPosition{x:-2,y:-2},max:GridPosition{x:1,y:1}},ViewRect{min:p(-2),max:GridPosition{x:1,y:1}},ViewRect{min:positions[0],max:positions[0]}]{
        let visible:Vec<_>=f.motion_frame().visible(rect).unwrap().iter().map(|m|m.entity).collect();
        let expected:Vec<_>=events.iter().filter(|e|visible.contains(&e.attacker)&&visible.contains(&e.target)).map(|e|(e.attacker,e.target)).collect();
        let mut s=CombatSyncSender::new(9,CombatSyncConfig{events_per_packet:2,..CombatSyncConfig::default()}).unwrap();let mut c=CombatReplica::new(9);s.observe(&f,rect).unwrap();let mut actual=Vec::new();
        while let Some(packet)=s.prepare().unwrap(){let r=c.apply(packet).unwrap();actual.extend(c.drain_feedback().map(|e|(e.attacker,e.target)));s.acknowledge_packet(&r.acknowledgement()).unwrap();}
        assert_eq!(actual,expected);assert_eq!(c.states().iter().map(|s|s.entity).collect::<Vec<_>>(),visible);
    }
}

#[test]
fn event_only_ack_does_not_replace_a_newer_observed_snapshot(){
    let(mut w,a,b,mut f,_,mut c)=setup();let mut s=CombatSyncSender::new(17,CombatSyncConfig{events_per_packet:1,..CombatSyncConfig::default()}).unwrap();
    capture(&mut w,&mut f,&mut s);deliver(&mut s,&mut c);
    let event=CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};
    w.advance_tick().unwrap();f.capture(&w,&[event;3]).unwrap();s.observe(&f,view()).unwrap();
    let r=c.apply(s.prepare().unwrap().unwrap()).unwrap();assert_eq!(c.drain_feedback().count(),1);
    health(&mut w,&[HealthCommand::Damage{entity:b,amount:7}]);capture(&mut w,&mut f,&mut s);
    s.acknowledge_packet(&r.acknowledgement()).unwrap();let r=c.apply(s.prepare().unwrap().unwrap()).unwrap();assert_eq!(hp(&c,b).unwrap().current,43);assert_eq!(c.drain_feedback().count(),1);
    health(&mut w,&[HealthCommand::Heal{entity:b,amount:3}]);capture(&mut w,&mut f,&mut s);
    s.acknowledge_packet(&r.acknowledgement()).unwrap();let(_,e)=deliver(&mut s,&mut c);assert_eq!(e.len(),1);assert_eq!(hp(&c,b).unwrap().current,46);
    assert!(s.prepare().unwrap().is_none());s.request_full().unwrap();deliver(&mut s,&mut c);assert_eq!(hp(&c,b).unwrap().current,46);
}

#[test]
fn life_deltas_coalesce_and_share_the_motion_frame(){
    let(mut w,a,b,mut f,mut s,mut c)=setup();let plain=w.spawn(p(100)).unwrap();
    capture(&mut w,&mut f,&mut s);let(_,events)=deliver(&mut s,&mut c);assert!(events.is_empty());assert_eq!(hp(&c,plain),None);
    let mut motion=SyncSender::new(99);let mut replica=Replica::new(99);replica.apply(motion.prepare(f.motion_frame(),view()).unwrap().unwrap()).unwrap();assert_eq!(replica.states().len(),3);
    health(&mut w,&[HealthCommand::Damage{entity:b,amount:10},HealthCommand::Heal{entity:b,amount:3}]);
    capture(&mut w,&mut f,&mut s);let(bytes,events)=deliver(&mut s,&mut c);assert_eq!(bytes.len(),40+13);assert_eq!(hp(&c,b).unwrap().current,43);assert!(events.is_empty());
    health(&mut w,&[HealthCommand::SetMaximum{entity:b,maximum:n(u32::MAX)},HealthCommand::Heal{entity:b,amount:u32::MAX}]);
    w.configure_health(plain,7,n(9)).unwrap();capture(&mut w,&mut f,&mut s);deliver(&mut s,&mut c);assert_eq!(hp(&c,b).unwrap().current,u32::MAX);assert_eq!(hp(&c,plain).unwrap().current,7);
    health(&mut w,&[HealthCommand::Damage{entity:b,amount:u32::MAX},HealthCommand::Revive{entity:b,current:n(11)}]);
    capture(&mut w,&mut f,&mut s);deliver(&mut s,&mut c);assert_eq!(hp(&c,b).unwrap().current,11);assert_eq!(hp(&c,a).unwrap().current,50);
    capture(&mut w,&mut f,&mut s);assert!(s.prepare().unwrap().is_none());
}

#[test]
fn real_combat_order_survives_delayed_ack_and_snapshot_coalescing(){
    let(mut w,a,b,mut f,_,mut c)=setup();let mut s=CombatSyncSender::new(17,CombatSyncConfig{events_per_packet:2,..CombatSyncConfig::default()}).unwrap();
    w.start_attack(a,b,profile(2)).unwrap();let mut first_packet=Vec::new();
    for t in 1..=16{
        if t==6{w.set_position(b,p(256)).unwrap();}if t==8{w.set_position(b,p(64)).unwrap();}
        if t==10{w.stop_attack(a).unwrap();}if t==11{w.start_attack(a,b,profile(2)).unwrap();}
        let events=phase(&mut w);f.capture(&w,&events).unwrap();s.observe(&f,view()).unwrap();
        if t==1{first_packet=s.prepare().unwrap().unwrap().to_vec();}
        if t==10{assert!(w.attack_state(a).unwrap().windup.is_none());}
    }
    assert_eq!(s.pending_packet().unwrap(),first_packet);assert_eq!(s.queued_events(),7);
    let report=c.apply(&first_packet).unwrap();let mut feedback:Vec<_>=c.drain_feedback().collect();s.acknowledge_packet(&report.acknowledgement()).unwrap();
    while s.queued_events()>0{let(_,events)=deliver(&mut s,&mut c);assert!(events.len()<=2);feedback.extend(events);}
    let shape:Vec<_>=feedback.iter().map(|e|(e.tick.0,match e.kind{CombatFeedbackKind::WindupStarted{..}=>0,CombatFeedbackKind::WindupCancelled{..}=>1,CombatFeedbackKind::Hit{..}=>2,_=>3})).collect();
    assert_eq!(shape,[(1,0),(3,2),(5,0),(6,1),(9,0),(13,0),(15,2)]);
    assert!(feedback.iter().all(|e|e.attacker==a&&e.target==b));assert_eq!(hp(&c,b).unwrap().current,30);
    assert_eq!(c.states().iter().find(|s|s.entity==a).unwrap().windup,None);assert!(s.prepare().unwrap().is_none());
}

#[test]
fn retransmission_and_wrong_ack_do_not_commit_or_replay(){
    let(mut w,a,b,mut f,mut s,mut c)=setup();w.start_attack(a,b,profile(2)).unwrap();let events=phase(&mut w);f.capture(&w,&events).unwrap();s.observe(&f,view()).unwrap();
    let bytes=s.prepare().unwrap().unwrap().to_vec();assert_eq!(s.request_full(),Err(SyncError::WaitingForAck.into()));
    let r=c.apply(&bytes).unwrap();let ack=r.acknowledgement();assert!(c.apply(&bytes).unwrap().duplicate);assert_eq!(c.drain_feedback().count(),1);assert_eq!(c.drain_feedback().count(),0);
    for offset in [0,4,12]{let mut bad=ack;bad[offset]^=1;assert!(s.acknowledge_packet(&bad).is_err());assert_eq!(s.pending_packet().unwrap(),bytes);assert_eq!(s.queued_events(),1);}
    s.acknowledge_packet(&ack).unwrap();s.acknowledge_packet(&ack).unwrap();assert_eq!(s.queued_events(),0);
    let mut forged=bytes.clone();forged.push(0);assert!(c.apply(&forged).is_err());assert_eq!(c.sequence(),1);
    let events=phase(&mut w);f.capture(&w,&events).unwrap();s.observe(&f,view()).unwrap();assert!(s.prepare().unwrap().is_none());
    let events=phase(&mut w);f.capture(&w,&events).unwrap();s.observe(&f,view()).unwrap();let next=s.prepare().unwrap().unwrap().to_vec();
    s.acknowledge_packet(&ack).unwrap();assert_eq!(s.pending_packet().unwrap(),next);let r=c.apply(&next).unwrap();assert_eq!(c.drain_feedback().count(),1);s.acknowledge_packet(&r.acknowledgement()).unwrap();
}

#[test]
fn sender_backpressure_can_retry_exact_frame_after_ack(){
    let(mut w,a,b,mut f,_,mut c)=setup();let mut s=CombatSyncSender::new(17,CombatSyncConfig{pending_events:1,events_per_packet:1}).unwrap();
    let mut attack=profile(0);attack.cooldown_ticks=NonZeroU16::new(1).unwrap();w.start_attack(a,b,attack).unwrap();
    let events=phase(&mut w);f.capture(&w,&events).unwrap();s.observe(&f,view()).unwrap();let first=s.prepare().unwrap().unwrap().to_vec();
    let events=phase(&mut w);f.capture(&w,&events).unwrap();assert_eq!(s.observe(&f,view()),Err(CombatSyncError::EventBackpressure));assert_eq!(s.queued_events(),1);assert_eq!(s.pending_packet().unwrap(),first);
    let r=c.apply(&first).unwrap();assert_eq!(hp(&c,b).unwrap().current,40);assert_eq!(c.drain_feedback().count(),1);s.acknowledge_packet(&r.acknowledgement()).unwrap();
    s.observe(&f,view()).unwrap();let(_,events)=deliver(&mut s,&mut c);assert_eq!(events.len(),1);assert_eq!(events[0].tick,Tick(2));assert_eq!(hp(&c,b).unwrap().current,30);
}

#[test]
fn receiver_partial_drain_retains_events_and_blocks_the_next_packet(){
    let(mut w,a,b,mut f,mut s,mut c)=setup();let other=actor(&mut w,32,50);for id in [a,other]{w.start_attack(id,b,profile(0)).unwrap();}
    let events=phase(&mut w);f.capture(&w,&events).unwrap();s.observe(&f,view()).unwrap();let first=s.prepare().unwrap().unwrap().to_vec();let r=c.apply(&first).unwrap();
    let first_event=c.drain_feedback().next().unwrap();assert_eq!(first_event.attacker,a);s.acknowledge_packet(&r.acknowledgement()).unwrap();
    health(&mut w,&[HealthCommand::Heal{entity:b,amount:5}]);capture(&mut w,&mut f,&mut s);let next=s.prepare().unwrap().unwrap().to_vec();let states=c.states().to_vec();
    assert_eq!(c.apply(&next),Err(CombatSyncError::EventBackpressure));assert_eq!(c.states(),states);assert_eq!(c.sequence(),1);assert!(c.apply(&first).unwrap().duplicate);
    let rest:Vec<_>=c.drain_feedback().collect();assert_eq!(rest.len(),1);assert_eq!(rest[0].attacker,other);c.apply(&next).unwrap();assert_eq!(hp(&c,b).unwrap().current,35);
}

#[test]
fn visibility_redacts_windup_and_events_but_preserves_visible_health(){
    let(mut w,a,b,mut f,_,_)=setup();w.start_attack(a,b,profile(2)).unwrap();
    let mut only_a=CombatSyncSender::new(1,CombatSyncConfig::default()).unwrap();let mut ca=CombatReplica::new(1);
    let mut only_b=CombatSyncSender::new(2,CombatSyncConfig::default()).unwrap();let mut cb=CombatReplica::new(2);
    for _ in 1..=3{let events=phase(&mut w);f.capture(&w,&events).unwrap();only_a.observe(&f,ViewRect{min:p(0),max:p(0)}).unwrap();only_b.observe(&f,ViewRect{min:p(64),max:p(64)}).unwrap();
        if let Some(bytes)=only_a.prepare().unwrap(){let r=ca.apply(bytes).unwrap();assert_eq!(ca.drain_feedback().count(),0);only_a.acknowledge_packet(&r.acknowledgement()).unwrap();}
        if let Some(bytes)=only_b.prepare().unwrap(){let r=cb.apply(bytes).unwrap();assert_eq!(cb.drain_feedback().count(),0);only_b.acknowledge_packet(&r.acknowledgement()).unwrap();}
        assert_eq!(ca.states().len(),1);assert_eq!(ca.states()[0].entity,a);assert_eq!(ca.states()[0].windup,None);
    }
    assert_eq!(hp(&cb,b).unwrap().current,40);
}

#[test]
fn late_join_has_current_windup_and_queued_history_survives_leaving_view(){
    let(mut w,a,b,mut f,mut s,mut c)=setup();w.start_attack(a,b,profile(2)).unwrap();
    let e=phase(&mut w);f.capture(&w,&e).unwrap();s.observe(&f,view()).unwrap();
    let e=phase(&mut w);f.capture(&w,&e).unwrap();
    let mut late=CombatSyncSender::new(3,CombatSyncConfig::default()).unwrap();let mut lc=CombatReplica::new(3);late.observe(&f,view()).unwrap();assert!(deliver(&mut late,&mut lc).1.is_empty());
    assert_eq!(lc.states().iter().find(|s|s.entity==a).unwrap().windup.unwrap(),WindupView{target:b,started_at:Tick(1),hit_at:Tick(3)});
    s.observe(&f,ViewRect{min:p(1000),max:p(1001)}).unwrap();let(_,events)=deliver(&mut s,&mut c);assert!(c.states().is_empty());assert_eq!(events.len(),1);assert_eq!(events[0].tick,Tick(1));
}

#[test]
fn frame_errors_and_entity_reuse_are_explicit(){
    let(mut w,a,b,mut f,mut s,mut c)=setup();assert_eq!(s.prepare(),Err(SyncError::NoFrame.into()));capture(&mut w,&mut f,&mut s);deliver(&mut s,&mut c);
    assert_eq!(f.capture(&w,&[]),Err(SyncError::StaleTick));assert_eq!(s.observe(&f,view()),Err(CombatSyncError::FrameGap));
    w.despawn(a).unwrap();let next=w.spawn(p(0)).unwrap();assert_eq!(next.index,a.index);assert_ne!(next.generation,a.generation);capture(&mut w,&mut f,&mut s);deliver(&mut s,&mut c);
    assert!(!c.states().iter().any(|s|s.entity==a));assert_eq!(hp(&c,next),None);assert_eq!(hp(&c,b).unwrap().current,50);
    w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();assert_eq!(s.observe(&f,ViewRect{min:p(5),max:p(4)}),Err(SyncError::InvalidView.into()));s.observe(&f,view()).unwrap();
    w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();assert_eq!(s.observe(&f,view()),Err(CombatSyncError::FrameGap));assert!(s.prepare().unwrap().is_none());
}

#[test]
fn reset_keeps_sequence_and_rejects_old_packets(){
    let(mut w,_,b,mut f,mut s,mut c)=setup();capture(&mut w,&mut f,&mut s);let(first,_)=deliver(&mut s,&mut c);s.request_full().unwrap();
    health(&mut w,&[HealthCommand::Damage{entity:b,amount:7}]);capture(&mut w,&mut f,&mut s);let(reset,_)=deliver(&mut s,&mut c);
    assert_eq!(reset[3],1);assert_eq!(c.sequence(),2);assert_eq!(hp(&c,b).unwrap().current,43);assert_eq!(c.apply(&first),Err(SyncError::OutOfOrder.into()));
    let mut foreign=CombatReplica::new(999);assert_eq!(foreign.apply(&reset),Err(SyncError::WrongStream.into()));
}

#[test]
fn packet_event_limit_splits_a_real_multi_attacker_tick(){
    let mut w=World::default();let b=actor(&mut w,64,1000);let mut ids=Vec::new();
    for _ in 0..257{let a=actor(&mut w,0,50);ids.push(a);let mut attack=profile(0);attack.damage=n(1);w.start_attack(a,b,attack).unwrap();}
    let e=phase(&mut w);assert_eq!(e.len(),257);let mut f=CombatFrame::new(n(512));f.capture(&w,&e).unwrap();let mut s=CombatSyncSender::new(17,CombatSyncConfig::default()).unwrap();let mut c=CombatReplica::new(17);s.observe(&f,view()).unwrap();
    let(bytes,first)=deliver(&mut s,&mut c);assert_eq!(u16::from_le_bytes([bytes[38],bytes[39]]),256);assert_eq!(first.len(),256);assert_eq!(s.queued_events(),1);
    let(bytes,last)=deliver(&mut s,&mut c);assert_eq!(u16::from_le_bytes([bytes[36],bytes[37]]),0);assert_eq!(last.len(),1);assert_eq!(last[0].attacker,ids[256]);assert_eq!(hp(&c,b).unwrap().current,743);assert!(s.prepare().unwrap().is_none());
}

// Hand-built wire fixtures exercise decoding independently of the production encoder.
fn eid(index:u32)->EntityId{EntityId{index,generation:0}}
fn wire_id(out:&mut Vec<u8>,id:EntityId){out.extend_from_slice(&id.index.to_le_bytes());out.extend_from_slice(&id.generation.to_le_bytes());}
fn record(id:EntityId,mask:u8,body:&[u8])->Vec<u8>{let mut b=Vec::new();wire_id(&mut b,id);b.push(mask);b.extend_from_slice(body);b}
fn enter(id:EntityId)->Vec<u8>{let mut body=Vec::new();body.extend_from_slice(&50u32.to_le_bytes());body.extend_from_slice(&50u32.to_le_bytes());body.push(0);record(id,15,&body)}
fn packet(sequence:u64,base:u64,tick:u64,reset:bool,rows:&[Vec<u8>],events:&[Vec<u8>])->Vec<u8>{
    let mut b=b"IC\x01".to_vec();b.push(u8::from(reset));for n in [17,sequence,base,tick]{b.extend_from_slice(&n.to_le_bytes());}
    b.extend_from_slice(&(rows.len()as u16).to_le_bytes());b.extend_from_slice(&(events.len()as u16).to_le_bytes());for row in rows{b.extend(row);}for event in events{b.extend(event);}b
}
fn event(code:u8,tick:u64,attacker:EntityId,target:EntityId,body:&[u8])->Vec<u8>{let mut b=vec![code];b.extend_from_slice(&tick.to_le_bytes());wire_id(&mut b,attacker);wire_id(&mut b,target);b.extend(body);b}
fn hit(tick:u64,damage:u32,remaining:u32)->Vec<u8>{let mut body=damage.to_le_bytes().to_vec();body.extend_from_slice(&remaining.to_le_bytes());event(2,tick,eid(0),eid(1),&body)}
fn seed()->(CombatReplica,Vec<u8>){let bytes=packet(1,0,1,true,&[enter(eid(0)),enter(eid(1))],&[]);let mut c=CombatReplica::new(17);c.apply(&bytes).unwrap();(c,bytes)}
fn rejected_atomically(c:&mut CombatReplica,bytes:&[u8]){
    let states=c.states().to_vec();let sequence=c.sequence();assert!(c.apply(bytes).is_err());assert_eq!(c.states(),states);assert_eq!(c.sequence(),sequence);assert_eq!(c.drain_feedback().count(),0);
}

#[test]
fn truncated_and_malformed_packets_do_not_commit_partial_state_or_events(){
    let(mut c,initial)=seed();let delta=record(eid(1),1,&40u32.to_le_bytes());
    let good=packet(2,1,2,false,&[delta.clone()],&[hit(2,5,45),hit(2,5,40)]);
    for end in 0..good.len(){rejected_atomically(&mut c,&good[..end]);}
    let mut trailing=good.clone();trailing.push(0);rejected_atomically(&mut c,&trailing);
    for(offset,value)in [(0,b'X'),(2,2),(3,2),(4,18),(12,1),(20,0),(28,0)]{let mut bad=good.clone();bad[offset]=value;rejected_atomically(&mut c,&bad);}
    for(offset,count)in [(36,8193u16),(38,257u16)]{let mut bad=good.clone();bad[offset..offset+2].copy_from_slice(&count.to_le_bytes());rejected_atomically(&mut c,&bad);}
    rejected_atomically(&mut c,&vec![0;MAX_COMBAT_SYNC_PACKET_BYTES+1]);
    for row in [record(eid(1),0,&[]),record(eid(1),64,&[]),record(eid(1),17,&[]),record(eid(1),33,&[]),record(eid(1),1,&u32::MAX.to_le_bytes()),record(eid(1),2,&0u32.to_le_bytes()),record(eid(2),1,&40u32.to_le_bytes()),record(eid(2),8,&[])]{
        rejected_atomically(&mut c,&packet(2,1,2,false,&[row],&[]));
    }
    rejected_atomically(&mut c,&packet(2,1,2,false,&[delta.clone(),delta.clone()],&[]));
    rejected_atomically(&mut c,&packet(2,1,2,false,&[delta.clone(),record(eid(0),1,&40u32.to_le_bytes())],&[]));
    rejected_atomically(&mut c,&packet(2,1,2,false,&[enter(EntityId{index:0,generation:1})],&[]));
    rejected_atomically(&mut c,&packet(2,0,2,true,&[delta],&[]));
    assert!(c.apply(&initial).unwrap().duplicate);let r=c.apply(&good).unwrap();assert_eq!(r.events,2);assert_eq!(hp(&c,eid(1)).unwrap().current,40);
    assert_eq!(c.drain_feedback().map(|e|e.kind).collect::<Vec<_>>(),[CombatFeedbackKind::Hit{damage:5,remaining:45},CombatFeedbackKind::Hit{damage:5,remaining:40}]);
}

#[test]
fn decoder_validates_windup_references_health_transitions_and_event_order(){
    let(mut c,_)=seed();
    for(target,start,end,marker)in [(eid(9),1u64,3u64,1),(eid(0),1,3,1),(eid(1),3,4,1),(eid(1),2,1,1),(eid(1),1,3,2)]{
        let mut body=vec![marker];wire_id(&mut body,target);body.extend_from_slice(&start.to_le_bytes());body.extend_from_slice(&end.to_le_bytes());
        rejected_atomically(&mut c,&packet(2,1,2,false,&[record(eid(0),4,&body)],&[]));
    }
    let state=record(eid(1),1,&40u32.to_le_bytes());
    let mut windup=1u64.to_le_bytes().to_vec();windup.extend_from_slice(&3u64.to_le_bytes());
    let mut reversed=2u64.to_le_bytes().to_vec();reversed.extend_from_slice(&1u64.to_le_bytes());
    for bad in [hit(1,5,40),hit(3,5,40),hit(2,0,40),event(4,2,eid(0),eid(1),&[]),event(3,2,eid(0),eid(1),&[5]),
        event(2,2,eid(0),eid(0),&[1,0,0,0,40,0,0,0]),event(0,2,eid(0),eid(1),&windup),event(0,2,eid(0),eid(1),&reversed),event(1,2,eid(0),eid(1),&3u64.to_le_bytes())]{
        rejected_atomically(&mut c,&packet(2,1,2,false,&[state.clone()],&[hit(2,5,45),bad]));
    }
    let good=packet(2,1,2,false,&[state],&[hit(2,10,40)]);c.apply(&good).unwrap();assert_eq!(c.drain_feedback().count(),1);
    rejected_atomically(&mut c,&packet(3,2,3,false,&[],&[hit(1,1,39)]));
    c.apply(&packet(3,2,3,false,&[record(eid(1),32,&[])],&[])).unwrap();assert_eq!(hp(&c,eid(1)),None);
    rejected_atomically(&mut c,&packet(4,3,4,false,&[record(eid(1),1,&5u32.to_le_bytes())],&[]));
    let mut values=5u32.to_le_bytes().to_vec();values.extend_from_slice(&9u32.to_le_bytes());c.apply(&packet(4,3,4,false,&[record(eid(1),3,&values)],&[])).unwrap();assert_eq!(hp(&c,eid(1)),Some(HealthState{current:5,maximum:n(9)}));
}

#[test]
fn end_reasons_have_stable_wire_codes_and_preserve_order(){
    let(mut w,a,b,mut f,mut s,mut c)=setup();capture(&mut w,&mut f,&mut s);deliver(&mut s,&mut c);w.advance_tick().unwrap();
    let reasons=[CombatEndReason::AttackerDepleted,CombatEndReason::TargetDepleted,CombatEndReason::TargetGone,CombatEndReason::HealthUnavailable,CombatEndReason::CooldownOverflow];
    // Trusted synthetic inputs cover rare phase outcomes without forging World internals.
    let events:Vec<_>=reasons.iter().map(|&reason|CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(reason)}).collect();f.capture(&w,&events).unwrap();s.observe(&f,view()).unwrap();let(bytes,received)=deliver(&mut s,&mut c);
    for(i,reason)in reasons.into_iter().enumerate(){assert_eq!(bytes[40+i*26+25],i as u8);assert_eq!(received[i].kind,CombatFeedbackKind::Ended(reason));}
}

#[test]
fn empty_views_and_configuration_limits_are_explicit(){
    for config in [CombatSyncConfig{pending_events:0,events_per_packet:1},CombatSyncConfig{pending_events:1,events_per_packet:0},CombatSyncConfig{pending_events:MAX_PENDING_COMBAT_EVENTS+1,events_per_packet:1},CombatSyncConfig{pending_events:4096,events_per_packet:257},CombatSyncConfig{pending_events:1,events_per_packet:2}]{
        assert!(matches!(CombatSyncSender::new(17,config),Err(CombatSyncError::InvalidConfig)));
    }
    let mut w=World::default();let mut f=CombatFrame::new(n(512));let mut s=CombatSyncSender::new(17,CombatSyncConfig::default()).unwrap();let mut c=CombatReplica::new(17);
    capture(&mut w,&mut f,&mut s);let(bytes,events)=deliver(&mut s,&mut c);assert_eq!(bytes.len(),40);assert!(events.is_empty());assert!(c.states().is_empty());assert_eq!(MAX_COMBAT_SYNC_PACKET_BYTES,268589);
    capture(&mut w,&mut f,&mut s);assert!(s.prepare().unwrap().is_none());
}
