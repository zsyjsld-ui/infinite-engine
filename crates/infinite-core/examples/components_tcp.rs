//! IC/IS/IP on the same real loopback TCP connection. Two authenticated host
//! subscriptions share budgets; one drops its first component ACK and reads slowly.
use infinite_core::*;
use std::{io::{self,Read,Write},net::{TcpListener,TcpStream,Shutdown},num::{NonZeroU16,NonZeroU32},thread,time::{Duration,Instant}};
type Result<T>=std::result::Result<T,Box<dyn std::error::Error>>;
fn error(e:impl std::fmt::Debug)->io::Error{io::Error::other(format!("{e:?}"))}
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
struct Received {events:Vec<CombatFeedback>,components:Vec<VisibleComponent>,motion:Vec<MotionState>,hp:u32,definition:u32,duplicates:usize,component_packets:usize}
fn client(mut socket:TcpStream,stream:u64,slow:bool)->io::Result<Received>{
 socket.set_read_timeout(Some(Duration::from_secs(5)))?;socket.set_write_timeout(Some(Duration::from_secs(5)))?;socket.set_nodelay(true)?;
 let mut combat=CombatReplica::new_windowed(stream);let mut motion=Replica::new(stream);let mut components=ComponentReplica::new_with_definitions(stream);let(mut events,mut duplicates,mut component_packets)=(Vec::new(),0,0);
 loop{
  let mut prefix=[0;4];match socket.read(&mut prefix[..1]){Ok(0)=>break,Ok(_)=>{},Err(e)=>return Err(e)}socket.read_exact(&mut prefix[1..])?;let length=u32::from_le_bytes(prefix)as usize;if !(40..=MAX_COMPONENT_PACKET_BYTES.max(MAX_COMBAT_SYNC_PACKET_BYTES)).contains(&length){return Err(error("invalid length"));}let mut bytes=vec![0;length];socket.read_exact(&mut bytes)?;
  let ack=match &bytes[..2]{
   b"IC"=>{let r=combat.apply(&bytes).map_err(error)?;duplicates+=usize::from(r.duplicate);events.extend(combat.drain_feedback());r.acknowledgement()},
   b"IS"=>{let r=motion.apply(&bytes).map_err(error)?;duplicates+=usize::from(r.duplicate);r.acknowledgement()},
   b"IP"=>{let r=components.apply(&bytes).map_err(error)?;component_packets+=1;duplicates+=usize::from(r.duplicate);r.acknowledgement()},
   _=>return Err(error("interleaved/unknown frame")),
  };
  if slow&&bytes.starts_with(b"IP")&&component_packets==1{continue;}
  if slow{thread::sleep(Duration::from_millis(8));}socket.write_all(&ack[..3])?;if slow{thread::sleep(Duration::from_millis(1));}socket.write_all(&ack[3..])?;
 }
 let hp=combat.states().iter().find(|s|s.entity.index==1).and_then(|s|s.health).ok_or_else(||error("missing HP"))?.current;
 Ok(Received{events,components:components.states().to_vec(),motion:motion.states().to_vec(),hp,definition:components.definition(1).ok_or_else(||error("missing definition"))?.revision,duplicates,component_packets})
}
fn main()->Result<()>{
 let listener=TcpListener::bind("127.0.0.1:0")?;let(mut sockets,mut clients)=(Vec::new(),Vec::new());for i in 0..2{let client_socket=TcpStream::connect(listener.local_addr()?)?;let(server,_)=listener.accept()?;server.set_nonblocking(true)?;server.set_nodelay(true)?;sockets.push(server);clients.push(thread::spawn(move||client(client_socket,900+i,i==1)));}
 let mut w=World::default();let a=w.spawn(GridPosition::default()).map_err(error)?;let b=w.spawn(GridPosition{x:16,y:0}).map_err(error)?;for id in[a,b]{w.configure_health(id,1000,n(1000)).map_err(error)?;}let mover=w.spawn(GridPosition::default()).map_err(error)?;let drop=w.spawn(GridPosition{x:1000,y:0}).map_err(error)?;
 w.define_public_component(1,b"example-public-definition-v1").map_err(error)?;
 let mut dropped=GroundItemComponent{definition:1,count:n(2),position:GridPosition{x:1000,y:0},height:0,source:a,from_monster:true,exclusive_owner:Some(42),exclusive_until:Tick(100),expires_at:Tick(200)}.record();dropped.fields.set_reference(0,1).map_err(error)?;w.set_component(drop,dropped).map_err(error)?;w.set_component(drop,ground_members_component(Some(&[42,43])).map_err(error)?).map_err(error)?;
 w.learn_skill(a,SkillSpec{id:SkillId(2),resource_cost:0,cooldown_ticks:8,windup_ticks:2,recovery:None,ingress_grace_ticks:0}).map_err(error)?;w.start_cast(CastRequest{caster:a,request_id:1,skill:SkillId(2),target:CastTarget::Entity(b)}).map_err(error)?;
 w.start_attack(a,b,AttackProfile{range:32,damage:n(1),cooldown_ticks:NonZeroU16::new(1).unwrap(),windup_ticks:1}).map_err(error)?;
 let mut net=CombatConnections::with_window(900,NetworkConfig{connections:2,quantum_bytes:64,retry_ms:25,ack_timeout_ms:2000,..NetworkConfig::default()},4).map_err(error)?;let ids=[net.connect().map_err(error)?,net.connect().map_err(error)?];let view=ViewRect{min:GridPosition{x:-100,y:-100},max:GridPosition{x:100,y:100}};
 for(index,id)in ids.into_iter().enumerate(){net.set_motion_view(id,view).map_err(error)?;net.set_component_view(id,Some(if index==0{a}else{b}),view).map_err(error)?;}
 let mut frame=CombatFrame::new(n(64));let mut components=ComponentFrame::new(n(64));let mut observations=ObservationQueue::new(4).map_err(error)?;let targets=ids.map(|connection|ObservationTarget{connection,view});let(mut failures,mut notices,mut expected,mut skills)=(Vec::new(),Vec::new(),Vec::new(),Vec::new());let mut acks=[[0u8;20];2];let mut filled=[0usize;2];
 let mut reference=CombatSyncSender::new_shielded(999,CombatSyncConfig::default()).map_err(error)?;let mut oracle=CombatReplica::new(999);let start=Instant::now();let(mut ticks,mut next_tick,mut bytes,mut retries,mut ready)=(0,0,0,0,0);
 loop{let now=start.elapsed().as_millis()as u64;if now>5000{return Err(error("finite TCP example timeout").into());}
  if ticks<20&&now>=next_tick{
   w.advance_tick().map_err(error)?;let h=w.health(b).map_err(error)?;let mut events=[CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}];let mut changes=[HealthChange{entity:b,before:h,after:h}];let report=w.resolve_combat(&mut events,&mut changes).map_err(error)?;
   skills.clear();w.resolve_casts(16,&mut skills).map_err(error)?;ready+=skills.iter().filter(|s|matches!(s,SkillEvent::Ready(_))).count();w.publish_skill_component(a,SkillId(2)).map_err(error)?;
   let mut appearance=AppearanceComponent{model:1,weapon:ticks+1,armor:3,palette:0,style:0}.record();appearance.fields.set_reference(0,1).map_err(error)?;w.set_component(a,appearance).map_err(error)?;
   if ticks==9{w.define_public_component(1,b"example-public-definition-v2").map_err(error)?;}if ticks==11{w.despawn(drop).map_err(error)?;}w.set_position(mover,GridPosition{x:(ticks+1)as i32,y:0}).map_err(error)?;components.capture(&w).map_err(error)?;
   frame.capture(&w,&events[..report.events_written]).map_err(error)?;reference.observe(&frame,view).map_err(error)?;if let Some(packet)=reference.prepare().map_err(error)?{let r=oracle.apply(packet).map_err(error)?;expected.extend(oracle.drain_feedback());reference.acknowledge_packet(&r.acknowledgement()).map_err(error)?;}observations.enqueue(&net,&frame,&targets).map_err(error)?;ticks+=1;next_tick=now+5;
  }
  for i in 0..2{loop{match sockets[i].read(&mut acks[i][filled[i]..]){Ok(0)=>return Err(error("unexpected EOF").into()),Ok(count)=>{filled[i]+=count;if filled[i]==20{net.acknowledge(ids[i],&acks[i]).map_err(error)?;filled[i]=0;}},Err(e)if e.kind()==io::ErrorKind::WouldBlock=>break,Err(e)if e.kind()==io::ErrorKind::Interrupted=>continue,Err(e)=>return Err(e.into())}}}
  let mut budget=NetworkBudget{work:4,bytes:93};let observed=observations.poll(&mut net,&mut budget,&mut failures).map_err(error)?;if !failures.is_empty(){return Err(error(&failures).into());}
  let report=net.poll_with_components(now,&components,&mut budget,&mut notices,|id,p|match sockets[id.index as usize].write(p){Ok(n)=>WriteProgress::Written(n),Err(e)if matches!(e.kind(),io::ErrorKind::WouldBlock|io::ErrorKind::Interrupted)=>WriteProgress::WouldBlock,Err(_)=>WriteProgress::Disconnected}).map_err(error)?;
  if !notices.is_empty(){return Err(error(&notices).into());}assert!(observed.visited+report.visited<=4);assert_eq!(report.bytes+budget.bytes,93);bytes+=report.bytes;retries+=report.retries;
  if ticks==20&&observations.pending()==0&&ids.iter().all(|&id|{let s=net.status(id).unwrap();let m=net.motion_status(id).unwrap().unwrap();let c=net.component_status(id).unwrap().unwrap();s.pending_bytes==0&&s.queued_events==0&&m.pending_bytes==0&&c.pending_bytes==0&&m.last_prepared_tick==Some(w.tick())&&c.last_prepared_tick==Some(w.tick())}){break;}thread::sleep(Duration::from_millis(1));
 }
 for socket in &sockets{socket.shutdown(Shutdown::Write)?;}let mut results=Vec::new();for(index,client)in clients.into_iter().enumerate(){let result=client.join().map_err(|_|error("client panic"))??;assert_eq!(result.events,expected);assert_eq!(result.hp,990);assert_eq!(result.definition,2);assert_eq!(result.motion,components.motion_frame().visible(view).map_err(error)?);let mut selector=ComponentSelector::new(Some(if index==0{a}else{b}));assert_eq!(result.components,selector.select(&components,view).map_err(error)?);if index==1{assert!(result.components.iter().all(|r|r.record.scope!=ComponentScope::Owner));}assert!(!result.components.iter().any(|r|r.entity==drop));results.push(result);}
 assert_eq!(ready,1);assert_eq!(expected.len(),20);assert!(results[1].duplicates>=1);assert!(retries>=1);println!("connections=2 ticks=20 combat_events_each=20 hp=990 skill_ready=1 definitions_revision=2 private_isolated=true drop_removed=true bytes_with_prefix={bytes} retries={retries} component_packets={:?} duplicate_packets={:?}",results.iter().map(|r|r.component_packets).collect::<Vec<_>>(),results.iter().map(|r|r.duplicates).collect::<Vec<_>>());Ok(())
}
