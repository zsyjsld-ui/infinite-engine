use infinite_core::*;use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn view()->ViewRect{ViewRect{min:GridPosition{x:-64,y:-64},max:GridPosition{x:64,y:64}}}
fn setup(cap:usize)->(World,EntityId,EntityId,CombatFrame,CombatConnections,[ConnectionId;2]){
 let mut w=World::default();let a=w.spawn(GridPosition::default()).unwrap();let b=w.spawn(GridPosition{x:16,y:0}).unwrap();for e in [a,b]{w.configure_health(e,100,n(100)).unwrap();}
 let mut net=CombatConnections::new(10,NetworkConfig{combat:CombatSyncConfig{pending_events:cap,events_per_packet:1},..NetworkConfig::default()}).unwrap();let ids=[net.connect().unwrap(),net.connect().unwrap()];(w,a,b,CombatFrame::new(n(32)),net,ids)
}
fn targets(ids:&[ConnectionId])->Vec<ObservationTarget>{ids.iter().map(|&connection|ObservationTarget{connection,view:view()}).collect()}
fn capture(w:&mut World,a:EntityId,b:EntityId,f:&mut CombatFrame,event:bool){w.advance_tick().unwrap();let e=CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};f.capture(w,if event{std::slice::from_ref(&e)}else{&[]}).unwrap();}
fn deliver(net:&mut CombatConnections,id:ConnectionId,replica:&mut CombatReplica)->Vec<CombatFeedback>{
 let mut bytes=Vec::new();for _ in 0..100{net.poll(0,&mut NetworkBudget{work:4096,bytes:65536},&mut Vec::new(),|peer,p|if peer==id{bytes.extend_from_slice(p);WriteProgress::Written(p.len())}else{WriteProgress::WouldBlock}).unwrap();let s=net.status(id).unwrap();if s.waiting_ack&&s.written_bytes==s.pending_bytes{let r=replica.apply(&bytes[4..]).unwrap();let events=replica.drain_feedback().collect();net.acknowledge(id,&r.acknowledgement()).unwrap();return events;}}panic!("delivery bounded");
}

#[test]
fn retained_frames_are_immutable_and_shared_budget_preserves_per_peer_order(){
 let(mut w,a,b,mut f,mut net,ids)=setup(10);let mut q=ObservationQueue::new(2).unwrap();let mut clients=[CombatReplica::new(10),CombatReplica::new(11)];let t=targets(&ids);
 capture(&mut w,a,b,&mut f,false);q.enqueue(&net,&f,&t).unwrap();w.apply_damage(b,3).unwrap();w.set_position(b,GridPosition{x:200,y:0}).unwrap();capture(&mut w,a,b,&mut f,false);q.enqueue(&net,&f,&t).unwrap();assert_eq!((q.pending(),q.retained_frames()),(4,2));
 let mut failures=Vec::new();for (i,&id)in ids.iter().enumerate(){let mut budget=NetworkBudget{work:1,bytes:999};let report=q.poll(&mut net,&mut budget,&mut failures).unwrap();assert_eq!((report.observed,budget.work,budget.bytes),(1,0,999));assert!(failures.is_empty());let mut writes=0;net.poll(0,&mut budget,&mut Vec::new(),|_,_|{writes+=1;panic!()}).unwrap();assert_eq!(writes,0);deliver(&mut net,id,&mut clients[i]);assert_eq!(clients[i].states().len(),2);assert_eq!(clients[i].states().iter().find(|s|s.entity==b).unwrap().health.unwrap().current,100);}
 assert_eq!(q.retained_frames(),1);for(i,&id)in ids.iter().enumerate(){q.poll(&mut net,&mut NetworkBudget{work:1,bytes:0},&mut failures).unwrap();deliver(&mut net,id,&mut clients[i]);assert_eq!(clients[i].states().iter().map(|s|s.entity).collect::<Vec<_>>(),vec![a]);}assert_eq!((q.pending(),q.retained_frames()),(0,0));
}

#[test]
fn blocked_subscriber_does_not_reorder_or_block_others_and_extraction_retains_events(){
 let(mut w,a,b,mut f,mut net,ids)=setup(1);let mut q=ObservationQueue::new(2).unwrap();let mut slow=CombatReplica::new(10);let mut fast=CombatReplica::new(11);let mut failures=Vec::new();let all=targets(&ids);
 capture(&mut w,a,b,&mut f,true);q.enqueue(&net,&f,&all).unwrap();q.poll(&mut net,&mut NetworkBudget{work:2,bytes:0},&mut failures).unwrap();assert_eq!(deliver(&mut net,ids[1],&mut fast)[0].tick,Tick(1));
 for tick in [2,3]{capture(&mut w,a,b,&mut f,true);q.enqueue(&net,&f,&all).unwrap();let r=q.poll(&mut net,&mut NetworkBudget{work:2,bytes:0},&mut failures).unwrap();assert_eq!((r.observed,r.backpressured),(1,1));assert_eq!(failures[0].connection,ids[0]);assert_eq!(deliver(&mut net,ids[1],&mut fast)[0].tick,Tick(tick));}
 capture(&mut w,a,b,&mut f,true);assert_eq!(q.enqueue(&net,&f,&all),Err(ObservationError::Full));assert_eq!((q.pending(),q.retained_frames()),(2,2));let mut recovery=q.extract(ids[0]).unwrap();assert_eq!((recovery.len(),recovery.first_serial()),(2,Some(2)));assert_eq!((q.pending(),q.retained_frames()),(0,0));
 q.enqueue(&net,&f,&targets(&ids[1..])).unwrap();q.poll(&mut net,&mut NetworkBudget{work:1,bytes:0},&mut failures).unwrap();assert_eq!(deliver(&mut net,ids[1],&mut fast)[0].tick,Tick(4));
 assert_eq!(deliver(&mut net,ids[0],&mut slow)[0].tick,Tick(1));assert_eq!(recovery.replay(&mut net,&mut NetworkBudget{work:1,bytes:0}).unwrap(),1);assert_eq!(deliver(&mut net,ids[0],&mut slow)[0].tick,Tick(2));assert_eq!(recovery.replay(&mut net,&mut NetworkBudget{work:1,bytes:0}).unwrap(),1);assert_eq!(deliver(&mut net,ids[0],&mut slow)[0].tick,Tick(3));assert!(recovery.is_empty());
}

#[test]
fn invalid_admission_and_removed_connections_preserve_pending_work(){
 let(mut w,a,b,mut f,mut net,ids)=setup(4);let mut q=ObservationQueue::new(1).unwrap();assert_eq!(q.enqueue(&net,&f,&targets(&ids)),Err(ObservationError::NoFrame));capture(&mut w,a,b,&mut f,true);
 assert_eq!(q.enqueue(&net,&f,&targets(&[ids[0],ids[0]])),Err(ObservationError::DuplicateConnection));assert_eq!(q.pending(),0);q.enqueue(&net,&f,&targets(&ids)).unwrap();let retained=net.remove(ids[0]).unwrap();assert_eq!(retained.queued_events(),0);let mut failures=Vec::new();let r=q.poll(&mut net,&mut NetworkBudget{work:2,bytes:0},&mut failures).unwrap();assert_eq!((r.observed,r.failed,r.pending),(1,1,1));assert_eq!(failures[0].error,NetworkError::InvalidConnection);assert_eq!(q.extract(ids[0]).unwrap().len(),1);assert_eq!(q.retained_frames(),0);
 assert_eq!(q.enqueue(&net,&f,&[]),Err(ObservationError::FrameGap));assert!(matches!(ObservationQueue::new(17),Err(ObservationError::InvalidCapacity)));
}

#[test]
fn isolated_subscription_drains_before_explicit_new_stream_and_snapshot(){
 let(mut w,a,b,mut f,mut net,ids)=setup(1);let mut q=ObservationQueue::new(1).unwrap();let mut slow=CombatReplica::new(10);let mut fast=CombatReplica::new(11);let mut failures=Vec::new();
 for tick in [1,2]{capture(&mut w,a,b,&mut f,true);q.enqueue(&net,&f,&targets(&ids)).unwrap();q.poll(&mut net,&mut NetworkBudget{work:2,bytes:0},&mut failures).unwrap();assert_eq!(deliver(&mut net,ids[1],&mut fast)[0].tick,Tick(tick));}
 let recovery=q.extract(ids[0]).unwrap();assert_eq!(recovery.last_reserved_serial(),2);
 let(mut recovery,error)=recovery.try_restart(&mut net).err().unwrap();assert_eq!(error,ObservationError::RecoveryPending);
 // Healthy subscriber keeps receiving every event while the old subscription
 // has an explicit boundary at frame 2. Its retained prefix stays intact.
 for tick in 3..=20{w.apply_damage(b,1).unwrap();capture(&mut w,a,b,&mut f,true);q.enqueue(&net,&f,&targets(&ids[1..])).unwrap();q.poll(&mut net,&mut NetworkBudget{work:2,bytes:0},&mut failures).unwrap();assert_eq!(deliver(&mut net,ids[1],&mut fast)[0].tick,Tick(tick));assert_eq!(q.retained_frames(),0);}
 assert_eq!(deliver(&mut net,ids[0],&mut slow)[0].tick,Tick(1));recovery.replay(&mut net,&mut NetworkBudget{work:1,bytes:0}).unwrap();
 let(recovery,error)=recovery.try_restart(&mut net).err().unwrap();assert_eq!(error,ObservationError::Network(NetworkError::Unacknowledged));assert_eq!(deliver(&mut net,ids[0],&mut slow)[0].tick,Tick(2));
 let(recovery,error)=recovery.try_restart(&mut net).err().unwrap();assert_eq!(error,ObservationError::Network(NetworkError::StillAttached));net.detach(ids[0]).unwrap();let restart=recovery.try_restart(&mut net).ok().unwrap();assert_eq!((restart.last_reserved_serial,restart.old_stream,restart.new_stream),(2,10,12));
 // The host communicates that frames 3..20 were outside this subscription,
 // clears the old transport decoder, and starts a different client replica.
 let mut fresh=CombatReplica::new(restart.new_stream);net.resume(ids[0]).unwrap();capture(&mut w,a,b,&mut f,true);q.enqueue(&net,&f,&targets(&ids)).unwrap();q.poll(&mut net,&mut NetworkBudget{work:2,bytes:0},&mut failures).unwrap();assert_eq!(deliver(&mut net,ids[0],&mut fresh)[0].tick,Tick(21));assert_eq!(fresh.states().iter().find(|s|s.entity==b).unwrap().health.unwrap().current,82);assert_eq!(deliver(&mut net,ids[1],&mut fast)[0].tick,Tick(21));
}
