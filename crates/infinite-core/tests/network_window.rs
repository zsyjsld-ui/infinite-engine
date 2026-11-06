use infinite_core::*;use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn view()->ViewRect{ViewRect{min:GridPosition{x:-64,y:-64},max:GridPosition{x:64,y:64}}}
fn setup(pending:usize)->(World,[EntityId;2],CombatFrame,CombatConnections,ConnectionId){
 let mut w=World::default();let a=w.spawn(GridPosition::default()).unwrap();let b=w.spawn(GridPosition{x:16,y:0}).unwrap();for e in [a,b]{w.configure_health(e,100,n(100)).unwrap();}let mut net=CombatConnections::with_window(100,NetworkConfig{retry_ms:10,combat:CombatSyncConfig{pending_events:pending,events_per_packet:1},..NetworkConfig::default()},4).unwrap();let id=net.connect().unwrap();(w,[a,b],CombatFrame::new(n(32)),net,id)
}
fn observe(w:&mut World,ids:[EntityId;2],f:&mut CombatFrame,net:&mut CombatConnections,id:ConnectionId)->Result<(),NetworkError>{w.advance_tick().unwrap();w.apply_damage(ids[1],1).unwrap();let e=CombatEvent{attacker:ids[0],target:ids[1],kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};f.capture(w,&[e]).unwrap();net.observe(id,f,view())}
fn pump(net:&mut CombatConnections,now:u64,bytes:&mut Vec<u8>,packets:&mut Vec<Vec<u8>>){net.poll(now,&mut NetworkBudget{work:16,bytes:4096},&mut Vec::new(),|_,p|{bytes.extend_from_slice(p);WriteProgress::Written(p.len())}).unwrap();loop{if bytes.len()<4{break;}let n=u32::from_le_bytes(bytes[..4].try_into().unwrap())as usize;if bytes.len()<n+4{break;}packets.push(bytes[4..n+4].to_vec());bytes.drain(..n+4);}}
fn collect(net:&mut CombatConnections,count:usize)->Vec<Vec<u8>>{let mut bytes=Vec::new();let mut packets=Vec::new();for _ in 0..100{pump(net,0,&mut bytes,&mut packets);if packets.len()==count{assert!(bytes.is_empty());return packets;}}panic!("bounded packets");}

#[test]
fn pipelined_packets_preserve_events_and_cumulative_ack_releases_whole_prefix(){
 let(mut w,ids,mut f,mut net,id)=setup(8);for _ in 0..3{observe(&mut w,ids,&mut f,&mut net,id).unwrap();}let packets=collect(&mut net,3);let mut replica=CombatReplica::new_windowed(100);let mut last=None;
 for(i,packet)in packets.iter().enumerate(){assert_eq!(packet[2],5);let r=replica.apply(packet).unwrap();let events:Vec<_>=replica.drain_feedback().collect();assert_eq!(events.iter().map(|e|e.tick).collect::<Vec<_>>(),vec![Tick(i as u64+1)]);assert_eq!(r.sequence,i as u64+1);last=Some(r.acknowledgement());}
 assert_eq!(replica.states().iter().find(|s|s.entity==ids[1]).unwrap().health.unwrap().current,97);assert_eq!(net.status(id).unwrap().inflight_packets,3);assert_eq!(net.status(id).unwrap().queued_events,3);assert!(net.retained_packet_capacity_bytes(id).unwrap()>0);assert_eq!(net.acknowledge(id,&last.unwrap()).unwrap(),AckDisposition::Committed);assert_eq!(net.status(id).unwrap().inflight_packets,0);assert_eq!(net.status(id).unwrap().queued_events,0);
 assert_eq!(CombatReplica::new(100).apply(&packets[0]),Err(CombatSyncError::State(SyncError::Malformed)));
}

#[test]
fn inflight_events_still_count_against_backpressure_and_invalid_ack_cannot_release_them(){
 let(mut w,ids,mut f,mut net,id)=setup(4);for _ in 0..4{observe(&mut w,ids,&mut f,&mut net,id).unwrap();}let packets=collect(&mut net,4);assert_eq!(observe(&mut w,ids,&mut f,&mut net,id),Err(NetworkError::Sync(CombatSyncError::EventBackpressure)));assert_eq!(net.status(id).unwrap().queued_events,4);
 let mut replica=CombatReplica::new_windowed(100);let mut ack=[0;20];for p in &packets[..2]{let r=replica.apply(p).unwrap();replica.drain_feedback().for_each(drop);ack=r.acknowledgement();}let mut invalid=ack;invalid[12..].copy_from_slice(&5u64.to_le_bytes());assert!(net.acknowledge(id,&invalid).is_err());assert_eq!(net.status(id).unwrap().queued_events,4);
 net.acknowledge(id,&ack).unwrap();assert_eq!(net.status(id).unwrap().queued_events,2);net.observe(id,&f,view()).unwrap();assert_eq!(net.status(id).unwrap().queued_events,3);
}

#[test]
fn cumulative_ack_mid_retry_finishes_old_frame_and_replay_does_not_duplicate_feedback(){
 let(mut w,ids,mut f,mut net,id)=setup(8);for _ in 0..3{observe(&mut w,ids,&mut f,&mut net,id).unwrap();}let packets=collect(&mut net,3);let mut replica=CombatReplica::new_windowed(100);let mut ack=[0;20];for p in &packets{let r=replica.apply(p).unwrap();replica.drain_feedback().for_each(drop);ack=r.acknowledgement();}
 let mut bytes=Vec::new();net.poll(10,&mut NetworkBudget{work:1,bytes:2},&mut Vec::new(),|_,p|{bytes.extend_from_slice(p);WriteProgress::Written(p.len())}).unwrap();assert_eq!(bytes.len(),2);assert_eq!(net.acknowledge(id,&ack).unwrap(),AckDisposition::DeferredUntilFrameEnd);assert_eq!(net.status(id).unwrap().inflight_packets,3);let mut retried=Vec::new();for _ in 0..10{pump(&mut net,10,&mut bytes,&mut retried);if !retried.is_empty(){break;}}
 assert_eq!(retried,vec![packets[0].clone()]);assert_eq!(net.status(id).unwrap().inflight_packets,0);let result=replica.apply(&retried[0]).unwrap();assert!(result.duplicate);assert_eq!(result.sequence,3);assert_eq!(replica.drain_feedback().count(),0);assert_eq!(net.acknowledge(id,&result.acknowledgement()).unwrap(),AckDisposition::Duplicate);
 let mut changed=packets[0].clone();*changed.last_mut().unwrap()^=1;assert!(replica.apply(&changed).is_err());assert_eq!(replica.sequence(),3);
}

#[test]
fn disconnect_mid_frame_retains_unsent_suffix_and_resumes_same_replica(){
 let(mut w,ids,mut f,mut net,id)=setup(8);for _ in 0..2{observe(&mut w,ids,&mut f,&mut net,id).unwrap();}let packets=collect(&mut net,2);let mut replica=CombatReplica::new_windowed(100);let mut ack=[0;20];for p in &packets{let r=replica.apply(p).unwrap();replica.drain_feedback().for_each(drop);ack=r.acknowledgement();}
 observe(&mut w,ids,&mut f,&mut net,id).unwrap();net.poll(0,&mut NetworkBudget{work:1,bytes:2},&mut Vec::new(),|_,p|WriteProgress::Written(p.len())).unwrap();net.detach(id).unwrap();net.acknowledge(id,&ack).unwrap();assert_eq!(net.status(id).unwrap().queued_events,1);assert_eq!(net.reset_replica(id),Err(NetworkError::Unacknowledged));net.resume(id).unwrap();let third=collect(&mut net,1);let r=replica.apply(&third[0]).unwrap();assert_eq!(replica.drain_feedback().next().unwrap().tick,Tick(3));net.acknowledge(id,&r.acknowledgement()).unwrap();net.detach(id).unwrap();let stream=net.reset_replica(id).unwrap();assert_ne!(stream,100);
}

#[test]
fn timeout_and_remove_transfer_every_unacknowledged_packet_without_dropping_events(){
 let(mut w,ids,mut f,mut net,id)=setup(8);for _ in 0..4{observe(&mut w,ids,&mut f,&mut net,id).unwrap();}let packets=collect(&mut net,4);let mut notices=Vec::new();net.poll(5000,&mut NetworkBudget{work:1,bytes:0},&mut notices,|_,_|panic!()).unwrap();assert_eq!(notices[0].kind,NetworkNoticeKind::AckTimeout);assert!(!net.status(id).unwrap().attached);let retained=net.remove(id).unwrap();assert_eq!(retained.queued_events(),4);assert_eq!(retained.packets.iter().map(|p|p.bytes.clone()).collect::<Vec<_>>(),packets);assert!(net.is_empty());
}



#[test]
fn replay_history_is_bounded_and_feedback_still_requires_ordered_drain(){
 let(mut w,ids,mut f,mut net,id)=setup(8);let mut replica=CombatReplica::new_windowed(100);let mut first=Vec::new();let mut previous=Vec::new();
 for tick in 1..=20{observe(&mut w,ids,&mut f,&mut net,id).unwrap();let packets=collect(&mut net,1);if tick==1{first=packets[0].clone();}let result=replica.apply(&packets[0]).unwrap();if tick>1{let duplicate=replica.apply(&previous).unwrap();assert!(duplicate.duplicate);assert_eq!(duplicate.sequence,tick);}assert_eq!(replica.drain_feedback().count(),1);net.acknowledge(id,&result.acknowledgement()).unwrap();previous=packets[0].clone();}
 assert_eq!(replica.apply(&first),Err(CombatSyncError::State(SyncError::OutOfOrder)));assert_eq!(replica.sequence(),20);assert!(CombatConnections::with_window(1,NetworkConfig::default(),17).is_err());
}
