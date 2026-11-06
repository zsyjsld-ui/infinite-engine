use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(x:u32)->NonZeroU32{NonZeroU32::new(x).unwrap()}
fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn view()->ViewRect{ViewRect{min:p(-512),max:p(512)}}
fn spawn(w:&mut World,x:i32)->EntityId{let e=w.spawn(p(x)).unwrap();w.configure_health(e,100000,n(100000)).unwrap();e}
fn phase(w:&mut World)->Vec<CombatEvent>{
    w.advance_tick().unwrap();let id=EntityId{index:0,generation:0};let h=HealthState{current:1,maximum:n(1)};
    let mut e=vec![CombatEvent{attacker:id,target:id,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};4096];
    let mut c=vec![HealthChange{entity:id,before:h,after:h};4096];let r=w.resolve_combat(&mut e,&mut c).unwrap();e.truncate(r.events_written);e
}
fn feedback(e:&CombatEvent,tick:Tick)->CombatFeedback{CombatFeedback{tick,attacker:e.attacker,target:e.target,kind:match e.kind{
    CombatEventKind::WindupStarted{windup,..}=>CombatFeedbackKind::WindupStarted{started_at:windup.started_at,hit_at:windup.hit_at},
    CombatEventKind::WindupCancelled{windup}=>CombatFeedbackKind::WindupCancelled{started_at:windup.started_at},
    CombatEventKind::Hit{health,..}=>CombatFeedbackKind::Hit{damage:health.before.current-health.after.current,remaining:health.after.current},
    CombatEventKind::Ended(reason)=>CombatFeedbackKind::Ended(reason),
}}}
fn drain(s:&mut CombatSyncSender,c:&mut CombatReplica)->(usize,Vec<CombatFeedback>){
    let(mut bytes,mut events)=(0,Vec::new());while let Some(b)=s.prepare().unwrap(){bytes+=b.len();let r=c.apply(b).unwrap();events.extend(c.drain_feedback());s.acknowledge_packet(&r.acknowledgement()).unwrap();}(bytes,events)
}
#[test]
fn v3_matches_v2_and_world_through_dynamic_visibility_health_windup_and_reuse(){
    let mut w=World::default();let mut ids:Vec<_>=(0..160).map(|i|spawn(&mut w,(i-80)*16)).collect();
    let profile=AttackProfile{range:128,damage:n(7),cooldown_ticks:NonZeroU16::new(2).unwrap(),windup_ticks:2};
    for i in (0..160).step_by(2){w.start_attack(ids[i],ids[i+1],profile).unwrap();}
    let config=CombatSyncConfig{events_per_packet:7,..CombatSyncConfig::default()};
    let mut senders:Vec<_>=(0..6).map(|i|if i%2==0{CombatSyncSender::new_compact(i,config)}else{CombatSyncSender::new_batched(i,config)}.unwrap()).collect();
    let mut clients:Vec<_>=(0..6).map(CombatReplica::new).collect();let mut f=CombatFrame::new(n(64));let(mut v2_bytes,mut v3_bytes)=(0,0);
    for tick in 1..=48{
        for i in (3..160).step_by(11){w.set_position(ids[i],GridPosition{x:(i as i32-80)*16,y:if tick%3==0{64}else{0}}).unwrap();}
        if tick%7==0{w.despawn(ids[19]).unwrap();ids[19]=spawn(&mut w,32);}
        let events=phase(&mut w);f.capture(&w,&events).unwrap();
        for r in 0..3{let x=-1300+r*640+(tick%4)*32;let v=ViewRect{min:p(x),max:p(x+640)};
            let expected:Vec<_>=events.iter().filter(|e|w.position(e.attacker).is_ok_and(|p|v.contains(p))&&w.position(e.target).is_ok_and(|p|v.contains(p))).map(|e|feedback(e,w.tick())).collect();
            let mut states:Vec<_>=w.motion_states().filter(|s|v.contains(s.position)).map(|s|VitalState{entity:s.entity,health:w.health(s.entity).ok(),windup:w.attack_state(s.entity).ok().and_then(|s|s.windup).filter(|a|w.position(a.target).is_ok_and(|p|v.contains(p))).map(|a|WindupView{target:a.target,started_at:a.started_at,hit_at:a.hit_at})}).collect();states.sort_by_key(|s|(s.entity.index,s.entity.generation));
            let mut sizes=[0;2];for mode in 0..2{let i=r as usize*2+mode;let report=senders[i].observe_report(&f,v).unwrap();assert_eq!(report.queued_events,expected.len());assert_eq!(report.filtered_events+report.queued_events,events.len());
                let(bytes,actual)=drain(&mut senders[i],&mut clients[i]);sizes[mode]=bytes;assert_eq!(actual,expected);assert_eq!(clients[i].states(),states);}
            assert!(sizes[1]<=sizes[0]);v2_bytes+=sizes[0];v3_bytes+=sizes[1];
        }
    }assert!(v3_bytes<v2_bytes);
}
fn var(mut value:u64)->Vec<u8>{let mut b=Vec::new();loop{let low=(value&127)as u8;value>>=7;b.push(low|if value==0{0}else{128});if value==0{return b;}}}
fn packet(version:u8,sequence:u64,now:u64,events:&[Vec<u8>])->Vec<u8>{
    let mut b=vec![b'I',b'C',version,u8::from(sequence==1)];for v in [17,sequence,if sequence==1{0}else{sequence-1},now]{b.extend(v.to_le_bytes());}
    b.extend(0u16.to_le_bytes());b.extend((events.len()as u16).to_le_bytes());b.push(0);for e in events{b.extend(e);}b
}
fn reject(c:&mut CombatReplica,b:&[u8]){let states=c.states().to_vec();let seq=c.sequence();assert!(c.apply(b).is_err());assert_eq!(c.sequence(),seq);assert_eq!(c.states(),states);assert_eq!(c.drain_feedback().count(),0);}
#[test]
fn independent_v3_tags_and_malformed_packets_are_atomic(){
    // First event: tick11, attacker1/generation0, target2/generation0.
    let first=vec![2,1,1,0,2,0,3,97];let seed=packet(2,1,10,&[]);
    for reuse in 0..8u8{let tag=2|(reuse<<2);let mut next=vec![tag];
        if reuse&1==0{next.push(1);}if reuse&2==0{next.extend([1,0]);}if reuse&4==0{next.extend([2,0]);}next.extend([3,94]);
        let good=packet(3,2,12,&[first.clone(),next]);let mut c=CombatReplica::new(17);c.apply(&seed).unwrap();
        for len in 0..good.len(){reject(&mut c,&good[..len]);}
        c.apply(&good).unwrap();let e:Vec<_>=c.drain_feedback().collect();assert_eq!(e.len(),2);assert_eq!((e[1].tick,e[1].attacker,e[1].target),(Tick(11),EntityId{index:1,generation:0},EntityId{index:2,generation:0}));assert_eq!(e[1].kind,CombatFeedbackKind::Hit{damage:3,remaining:94});
    }
    let mut c=CombatReplica::new(17);c.apply(&seed).unwrap();
    for bit in [4,8,16,32,64,128]{let mut bad=first.clone();bad[0]|=bit;reject(&mut c,&packet(3,2,12,&[bad]));}
    for bad in [vec![30,0,1],vec![30,128,0,1],vec![30,255,255,255,255,16,1],vec![26,13,3,94]]{reject(&mut c,&packet(3,2,12,&[first.clone(),bad]));}
    let mut trailing=packet(3,2,12,&[first.clone()]);trailing.push(0);reject(&mut c,&trailing);
    c.apply(&packet(3,2,12,&[first])).unwrap();c.drain_feedback().for_each(drop);
    // Reuse must not refer to the last event in a previous packet.
    reject(&mut c,&packet(3,3,13,&[vec![30,1,93]]));
    c.apply(&packet(2,3,13,&[vec![2,0,1,0,2,0,1,93]])).unwrap();assert_eq!(c.drain_feedback().count(),1);
}
#[test]
fn v3_all_kinds_and_maximum_handles_survive_buffer_flush_and_packet_limits(){
    let mut first=vec![0];first.extend(var(0));first.extend(var(u32::MAX.into()));first.extend(var(u32::MAX.into()));first.extend(var(0));first.extend(var(u32::MAX.into()));first.extend(var(0));
    let mut events=vec![first];
    for i in 1..256{let mut e=vec![28+(i%4)as u8];match i%4{0=>e.extend(var(0)),1=>e.extend(var(u64::MAX)),2=>{e.extend(var(u32::MAX.into()));e.extend(var(u32::MAX.into()));},_=>e.push(4)}events.push(e);}
    let mut c=CombatReplica::new(17);let bytes=packet(3,1,u64::MAX,&events);c.apply(&bytes).unwrap();let e:Vec<_>=c.drain_feedback().collect();assert_eq!(e.len(),256);
    for e in e{assert_eq!(e.tick,Tick(u64::MAX));assert_eq!(e.attacker,EntityId{index:u32::MAX,generation:u32::MAX});assert_eq!(e.target,EntityId{index:0,generation:u32::MAX});}
    events.push(vec![31,4]);let mut fresh=CombatReplica::new(17);reject(&mut fresh,&packet(3,1,u64::MAX,&events));
    // Actual state+event encoder crosses its stack-buffer boundary.
    let mut w=World::default();let a=spawn(&mut w,0);let b=spawn(&mut w,1);for _ in 0..512{spawn(&mut w,0);}w.advance_tick().unwrap();
    let input:Vec<_>=(0..256).map(|_|CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::HealthUnavailable)}).collect();
    let mut f=CombatFrame::new(n(64));f.capture(&w,&input).unwrap();let mut s=CombatSyncSender::new_batched(17,CombatSyncConfig::default()).unwrap();s.observe(&f,view()).unwrap();let mut c=CombatReplica::new(17);assert_eq!(drain(&mut s,&mut c).1,input.iter().map(|e|feedback(e,w.tick())).collect::<Vec<_>>());
}
#[test]
fn v3_ack_retries_and_both_backpressure_paths_preserve_history(){
    let mut w=World::default();let b=spawn(&mut w,10);for _ in 0..3{let a=spawn(&mut w,0);w.start_attack(a,b,AttackProfile{range:128,damage:n(1),cooldown_ticks:NonZeroU16::new(1).unwrap(),windup_ticks:0}).unwrap();}
    let mut f=CombatFrame::new(n(64));let mut s=CombatSyncSender::new_batched(17,CombatSyncConfig{pending_events:6,events_per_packet:2}).unwrap();let mut c=CombatReplica::new(17);let mut expected=Vec::new();
    let e=phase(&mut w);expected.extend(e.iter().map(|e|feedback(e,w.tick())));f.capture(&w,&e).unwrap();s.observe(&f,view()).unwrap();let first=s.prepare().unwrap().unwrap().to_vec();let ack=c.apply(&first).unwrap().acknowledgement();let mut actual=vec![c.drain_feedback().next().unwrap()];
    assert!(c.apply(&first).unwrap().duplicate);assert_eq!(s.request_full(),Err(SyncError::WaitingForAck.into()));let mut wrong=ack;wrong[12]^=64;assert!(s.acknowledge_packet(&wrong).is_err());
    for tick in 2..=3{let e=phase(&mut w);expected.extend(e.iter().map(|e|feedback(e,w.tick())));f.capture(&w,&e).unwrap();if tick==2{s.observe(&f,view()).unwrap();}else{assert_eq!(s.observe(&f,view()),Err(CombatSyncError::EventBackpressure));}assert_eq!(s.pending_packet().unwrap(),first);}
    s.acknowledge_packet(&ack).unwrap();assert_eq!(s.observe(&f,view()),Err(CombatSyncError::EventBackpressure));let second=s.prepare().unwrap().unwrap().to_vec();assert_eq!(c.apply(&second),Err(CombatSyncError::EventBackpressure));actual.extend(c.drain_feedback());let ack=c.apply(&second).unwrap().acknowledgement();actual.extend(c.drain_feedback());s.acknowledge_packet(&ack).unwrap();
    s.observe(&f,view()).unwrap();actual.extend(drain(&mut s,&mut c).1);assert_eq!(actual,expected);assert_eq!(c.states().iter().find(|s|s.entity==b).unwrap().health,w.health(b).ok());assert_eq!(s.queued_events(),0);
    s.request_full().unwrap();drain(&mut s,&mut c);assert!(c.apply(&first).is_err());
}
#[test]
fn event_blocks_preserve_boundary_order_invalid_handles_and_captured_positions(){
    let mut w=World::default();let a=spawn(&mut w,0);let b=spawn(&mut w,1);let far=spawn(&mut w,10000);let far_target=spawn(&mut w,10001);let gone=spawn(&mut w,0);w.despawn(gone).unwrap();let replacement=spawn(&mut w,0);assert_eq!(gone.index,replacement.index);assert_ne!(gone.generation,replacement.generation);
    let events:Vec<_>=(0..97).map(|i|CombatEvent{attacker:if i<32{far}else if i<64{gone}else{a},target:if i<32{far_target}else{b},kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}).collect();
    let mut f=CombatFrame::new(n(64));w.advance_tick().unwrap();f.capture(&w,&events).unwrap();w.set_position(a,p(10000)).unwrap();
    let mut s=CombatSyncSender::new_batched(17,CombatSyncConfig::default()).unwrap();let r=s.observe_report(&f,view()).unwrap();assert_eq!((r.captured_events,r.queued_events,r.filtered_events),(97,33,64));assert_eq!((r.skipped_blocks,r.checked_events),(2,33));
    let mut c=CombatReplica::new(17);assert_eq!(drain(&mut s,&mut c).1,events[64..].iter().map(|e|feedback(e,w.tick())).collect::<Vec<_>>());
}
#[test]
fn event_block_extreme_integer_edges_and_queued_events_after_leaving_view(){
    let mut w=World::default();let a=spawn(&mut w,i32::MIN);let b=spawn(&mut w,i32::MIN+1);let c=spawn(&mut w,i32::MAX-1);let d=spawn(&mut w,i32::MAX);
    let input:Vec<_>=(0..64).map(|i|CombatEvent{attacker:if i<32{a}else{c},target:if i<32{b}else{d},kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}).collect();
    let mut f=CombatFrame::new(n(u32::MAX));w.advance_tick().unwrap();f.capture(&w,&input).unwrap();let mut s=CombatSyncSender::new_batched(17,CombatSyncConfig{events_per_packet:7,..CombatSyncConfig::default()}).unwrap();let mut client=CombatReplica::new(17);
    let r=s.observe_report(&f,ViewRect{min:p(i32::MIN),max:p(i32::MIN+1)}).unwrap();assert_eq!((r.queued_events,r.skipped_blocks),(32,1));let first=s.prepare().unwrap().unwrap().to_vec();let ack=client.apply(&first).unwrap().acknowledgement();let mut actual:Vec<_>=client.drain_feedback().collect();
    w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();s.observe(&f,view()).unwrap();assert_eq!(s.pending_packet().unwrap(),first);s.acknowledge_packet(&ack).unwrap();actual.extend(drain(&mut s,&mut client).1);assert!(client.states().is_empty());assert_eq!(actual,input[..32].iter().map(|e|feedback(e,Tick(1))).collect::<Vec<_>>());
}
