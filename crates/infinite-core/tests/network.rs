use infinite_core::*;
use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn view()->ViewRect{ViewRect{min:GridPosition{x:-100,y:-100},max:GridPosition{x:100,y:100}}}
fn setup(config:NetworkConfig)->(World,EntityId,EntityId,CombatFrame,CombatConnections,ConnectionId){
 let mut w=World::default();let a=w.spawn(GridPosition::default()).unwrap();let b=w.spawn(GridPosition{x:16,y:0}).unwrap();for e in [a,b]{w.configure_health(e,100,n(100)).unwrap();}
 let mut frame=CombatFrame::new(n(64));frame.capture(&w,&[]).unwrap();let mut net=CombatConnections::new(100,config).unwrap();let id=net.connect().unwrap();net.observe(id,&frame,view()).unwrap();(w,a,b,frame,net,id)
}
fn send(net:&mut CombatConnections,id:ConnectionId,now:u64)->Vec<u8>{
 let mut out=Vec::new();for _ in 0..10000{net.poll(now,&mut NetworkBudget{work:4096,bytes:4096},&mut Vec::new(),|peer,data|{assert_eq!(peer,id);out.extend_from_slice(data);WriteProgress::Written(data.len())}).unwrap();let s=net.status(id).unwrap();if s.pending_bytes>0&&s.written_bytes==s.pending_bytes{assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap())as usize+4,out.len());return out;}}panic!("bounded delivery");
}
fn apply(replica:&mut CombatReplica,bytes:&[u8])->[u8;20]{let r=replica.apply(&bytes[4..]).unwrap();replica.drain_feedback().for_each(drop);r.acknowledgement()}
fn capture_event(w:&mut World,a:EntityId,b:EntityId,f:&mut CombatFrame){w.advance_tick().unwrap();let event=CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};f.capture(w,&[event]).unwrap();}

#[test]
fn hard_byte_budget_round_robin_and_partial_writes_preserve_frames(){
 let(_,_,_,f,mut net,a)=setup(NetworkConfig{quantum_bytes:7,..NetworkConfig::default()});let b=net.connect().unwrap();let c=net.connect().unwrap();for id in [b,c]{net.observe(id,&f,view()).unwrap();}
 let ids=[a,b,c];let mut bytes=[Vec::new(),Vec::new(),Vec::new()];let mut visits=Vec::new();
 for _ in 0..500{let mut budget=NetworkBudget{work:2,bytes:3};let report=net.poll(0,&mut budget,&mut Vec::new(),|id,data|{visits.push(id);let count=data.len().min(2);bytes[id.index as usize].extend_from_slice(&data[..count]);WriteProgress::Written(count)}).unwrap();assert!(report.bytes<=3&&report.visited<=2);assert_eq!(report.bytes+budget.bytes,3);assert_eq!(report.visited+budget.work,2);if ids.iter().all(|&id|{let s=net.status(id).unwrap();s.waiting_ack}){break;}}
 assert!(visits.windows(3).take(10).all(|v|v.contains(&a)&&v.contains(&b)&&v.contains(&c)));
 for id in ids{let s=net.status(id).unwrap();assert_eq!(s.written_bytes,bytes[id.index as usize].len());let mut replica=CombatReplica::new(s.stream);let ack=apply(&mut replica,&bytes[id.index as usize]);assert_eq!(replica.states().len(),2);assert_eq!(net.acknowledge(id,&ack).unwrap(),AckDisposition::Committed);}
}

#[test]
fn ack_mid_partial_retry_is_deferred_until_frame_end(){
 let(_,_,_,_,mut net,id)=setup(NetworkConfig{quantum_bytes:7,retry_ms:10,..NetworkConfig::default()});let initial=send(&mut net,id,0);let mut replica=CombatReplica::new(100);let ack=apply(&mut replica,&initial);
 let mut retry=Vec::new();net.poll(10,&mut NetworkBudget{work:1,bytes:2},&mut Vec::new(),|_,p|{retry.extend_from_slice(p);WriteProgress::Written(p.len())}).unwrap();assert_eq!(retry.len(),2);
 assert_eq!(net.acknowledge(id,&ack).unwrap(),AckDisposition::DeferredUntilFrameEnd);assert!(net.status(id).unwrap().pending_bytes>0);
 for _ in 0..100{net.poll(10,&mut NetworkBudget{work:1,bytes:1000},&mut Vec::new(),|_,p|{retry.extend_from_slice(p);WriteProgress::Written(p.len())}).unwrap();if net.status(id).unwrap().pending_bytes==0{break;}}
 assert_eq!(retry,initial);let r=replica.apply(&retry[4..]).unwrap();assert!(r.duplicate);assert_eq!(replica.drain_feedback().count(),0);assert_eq!(net.acknowledge(id,&ack).unwrap(),AckDisposition::Duplicate);
}

#[test]
fn stalled_peer_does_not_block_other_connection_and_timeouts_retain_packet(){
 let(_,_,_,f,mut net,a)=setup(NetworkConfig{blocked_timeout_ms:10,ack_timeout_ms:20,retry_ms:5,..NetworkConfig::default()});let b=net.connect().unwrap();net.observe(b,&f,view()).unwrap();let mut good=Vec::new();
 for now in 0..4{net.poll(now,&mut NetworkBudget{work:2,bytes:1000},&mut Vec::new(),|id,p|if id==a{WriteProgress::WouldBlock}else{good.extend_from_slice(p);WriteProgress::Written(p.len())}).unwrap();}
 assert!(net.status(b).unwrap().waiting_ack);let mut replica=CombatReplica::new(101);let ack=apply(&mut replica,&good);net.acknowledge(b,&ack).unwrap();
 let mut notices=Vec::new();net.poll(10,&mut NetworkBudget{work:2,bytes:0},&mut notices,|_,_|panic!()).unwrap();assert_eq!(notices,vec![NetworkNotice{connection:a,kind:NetworkNoticeKind::BlockedTimeout}]);assert!(!net.status(a).unwrap().attached);assert!(net.status(a).unwrap().pending_bytes>0);
 net.resume(a).unwrap();let packet=send(&mut net,a,11);assert!(!packet.is_empty());net.poll(31,&mut NetworkBudget{work:2,bytes:0},&mut notices,|_,_|panic!()).unwrap();assert_eq!(notices[0].kind,NetworkNoticeKind::AckTimeout);assert!(net.status(a).unwrap().pending_bytes>0);
}

#[test]
fn disconnected_connection_resumes_exact_packet_and_fresh_replica_requires_ack(){
 let(_,_,_,frame,mut net,id)=setup(NetworkConfig::default());let mut prefix=Vec::new();net.poll(0,&mut NetworkBudget{work:1,bytes:2},&mut Vec::new(),|_,p|{prefix.extend_from_slice(p);WriteProgress::Written(p.len())}).unwrap();assert_eq!(net.reset_replica(id),Err(NetworkError::Unacknowledged));
 net.detach(id).unwrap();net.resume(id).unwrap();assert_eq!(net.resume(id),Err(NetworkError::StillAttached));let packet=send(&mut net,id,1);assert_eq!(&packet[..2],&prefix);
 let mut replica=CombatReplica::new(100);let ack=apply(&mut replica,&packet);net.acknowledge(id,&ack).unwrap();net.detach(id).unwrap();assert_eq!(net.reset_replica(id).unwrap(),101);assert_eq!(net.acknowledge(id,&ack),Err(NetworkError::Sync(CombatSyncError::State(SyncError::WrongStream))));net.resume(id).unwrap();
 net.observe(id,&frame,view()).unwrap();let packet=send(&mut net,id,2);let mut fresh=CombatReplica::new(101);let ack=apply(&mut fresh,&packet);assert_eq!(fresh.states(),replica.states());net.acknowledge(id,&ack).unwrap();
}

#[test]
fn latest_snapshot_coalesces_but_events_survive_ack_wait_and_backpressure_is_atomic(){
 let(mut w,a,b,mut f,mut net,id)=setup(NetworkConfig{combat:CombatSyncConfig{pending_events:2,events_per_packet:1},..NetworkConfig::default()});let mut replica=CombatReplica::new(100);let first=send(&mut net,id,0);let initial=apply(&mut replica,&first);
 for damage in [1,2]{w.apply_damage(b,damage).unwrap();capture_event(&mut w,a,b,&mut f);net.observe(id,&f,view()).unwrap();}
 capture_event(&mut w,a,b,&mut f);assert_eq!(net.observe(id,&f,view()),Err(NetworkError::Sync(CombatSyncError::EventBackpressure)));assert_eq!(net.status(id).unwrap().queued_events,2);net.acknowledge(id,&initial).unwrap();
 let packet=send(&mut net,id,1);let result=replica.apply(&packet[4..]).unwrap();let events:Vec<_>=replica.drain_feedback().collect();assert_eq!(events.len(),1);assert_eq!(events[0].tick,Tick(1));assert_eq!(replica.states().iter().find(|s|s.entity==b).unwrap().health.unwrap().current,97);net.acknowledge(id,&result.acknowledgement()).unwrap();
 net.observe(id,&f,view()).unwrap();for tick in [2,3]{let packet=send(&mut net,id,2);let r=replica.apply(&packet[4..]).unwrap();let events:Vec<_>=replica.drain_feedback().collect();assert_eq!(events.iter().map(|e|e.tick).collect::<Vec<_>>(),vec![Tick(tick)]);net.acknowledge(id,&r.acknowledgement()).unwrap();}assert_eq!(net.status(id).unwrap().queued_events,0);
}

#[test]
fn invalid_ack_writer_time_and_stale_connection_are_explicit(){
 let(_,_,_,_,mut net,id)=setup(NetworkConfig{connections:1,..NetworkConfig::default()});assert_eq!(net.connect(),Err(NetworkError::Full));assert!(net.acknowledge(id,&[0;20]).is_err());let mut notices=Vec::new();
 net.poll(1,&mut NetworkBudget{work:1,bytes:100},&mut notices,|_,p|WriteProgress::Written(p.len()+1)).unwrap();assert_eq!(notices[0].kind,NetworkNoticeKind::InvalidWriter);assert!(!net.status(id).unwrap().attached);
 assert_eq!(net.poll(0,&mut NetworkBudget{work:1,bytes:1},&mut notices,|_,_|panic!()),Err(NetworkError::TimeReversed));let retained=net.remove(id).unwrap();assert!(retained.pending_packet().is_some());let new=net.connect().unwrap();assert_eq!(new.index,id.index);assert_ne!(new.generation,id.generation);assert_eq!(net.detach(id),Err(NetworkError::InvalidConnection));
}

#[test]
fn premature_ack_stream_exhaustion_and_invalid_configuration_do_not_commit(){
 let(_,_,_,_,mut net,id)=setup(NetworkConfig::default());let mut forged=[0;20];forged[..4].copy_from_slice(b"CA\x01\x00");forged[4..12].copy_from_slice(&100u64.to_le_bytes());forged[12..20].copy_from_slice(&1u64.to_le_bytes());
 net.poll(0,&mut NetworkBudget{work:1,bytes:2},&mut Vec::new(),|_,p|WriteProgress::Written(p.len())).unwrap();assert_eq!(net.acknowledge(id,&forged),Err(NetworkError::Sync(CombatSyncError::State(SyncError::OutOfOrder))));assert!(net.status(id).unwrap().pending_bytes>0);
 let mut full=CombatConnections::new(u64::MAX,NetworkConfig::default()).unwrap();full.connect().unwrap();assert_eq!(full.connect(),Err(NetworkError::StreamExhausted));assert_eq!(full.len(),1);
 for config in [NetworkConfig{connections:0,..NetworkConfig::default()},NetworkConfig{quantum_bytes:0,..NetworkConfig::default()},NetworkConfig{retry_ms:0,..NetworkConfig::default()}]{assert!(matches!(CombatConnections::new(1,config),Err(NetworkError::InvalidConfig)));}
 let mut budget=NetworkBudget{work:MAX_NETWORK_WORK+1,bytes:100};assert_eq!(net.poll(1,&mut budget,&mut Vec::new(),|_,_|panic!()),Err(NetworkError::InvalidBudget));assert_eq!(budget.bytes,100);
}
