//! Two real loopback TCP recipients using the shared bounded writer. One drops
//! its first ACK and delays later ACKs. Both must receive the same combat events.
use infinite_core::*;
use std::{io::{self,Read,Write},net::{TcpListener,TcpStream,Shutdown},num::{NonZeroU16,NonZeroU32},thread,time::{Duration,Instant}};
type Result<T>=std::result::Result<T,Box<dyn std::error::Error>>;
fn error(e:impl std::fmt::Debug)->io::Error{io::Error::other(format!("{e:?}"))}
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
struct ClientResult {events:Vec<CombatFeedback>,duplicates:usize,hp:u32,packets:usize,motion_packets:usize,position:Option<GridPosition>}
fn client(mut socket:TcpStream,stream:u64,slow:bool,windowed:bool,moving:bool)->io::Result<ClientResult>{
 socket.set_read_timeout(Some(Duration::from_secs(5)))?;socket.set_write_timeout(Some(Duration::from_secs(5)))?;socket.set_nodelay(true)?;
 let mut replica=if windowed{CombatReplica::new_windowed(stream)}else{CombatReplica::new(stream)};let mut motion=Replica::new(stream);let mut output=ClientResult{events:Vec::new(),duplicates:0,hp:0,packets:0,motion_packets:0,position:None};
 loop{let mut prefix=[0;4];match socket.read(&mut prefix[..1]){Ok(0)=>break,Ok(_)=>{},Err(e)=>return Err(e)}socket.read_exact(&mut prefix[1..])?;let len=u32::from_le_bytes(prefix)as usize;
  if !(COMBAT_SYNC_HEADER_BYTES.min(SYNC_HEADER_BYTES)..=MAX_COMBAT_SYNC_PACKET_BYTES.max(MAX_SYNC_PACKET_BYTES)).contains(&len){return Err(error("invalid frame size"));}let mut bytes=vec![0;len];socket.read_exact(&mut bytes)?;
  let ack=if moving&&bytes.starts_with(b"IS"){let r=motion.apply(&bytes).map_err(error)?;output.motion_packets+=1;output.duplicates+=usize::from(r.duplicate);r.acknowledgement()}else{let report=replica.apply(&bytes).map_err(error)?;output.packets+=1;output.duplicates+=usize::from(report.duplicate);output.events.extend(replica.drain_feedback());report.acknowledgement()};
  if slow&&output.packets+output.motion_packets==1{continue;}if slow{thread::sleep(Duration::from_millis(8));}socket.write_all(&ack[..3])?;if slow{thread::sleep(Duration::from_millis(1));}socket.write_all(&ack[3..])?;
 }
 output.hp=replica.states().iter().find(|s|s.entity.index==1).and_then(|s|s.health).ok_or_else(||error("missing final entity"))?.current;output.position=motion.states().iter().find(|s|s.entity.index==2).map(|s|s.position);Ok(output)
}
fn main()->Result<()>{
 let windowed=std::env::args().any(|s|s=="--window");
 let moving=std::env::args().any(|s|s=="--motion");
 let listener=TcpListener::bind("127.0.0.1:0")?;let mut sockets=Vec::new();let mut clients=Vec::new();
 for i in 0..2{let client_socket=TcpStream::connect(listener.local_addr()?)?;let(server,_)=listener.accept()?;server.set_nonblocking(true)?;server.set_nodelay(true)?;sockets.push(server);clients.push(thread::spawn(move||client(client_socket,700+i,i==1,windowed,moving)));}
 let mut w=World::default();let a=w.spawn(GridPosition::default()).map_err(error)?;let b=w.spawn(GridPosition{x:16,y:0}).map_err(error)?;for e in [a,b]{w.configure_health(e,1000,n(1000)).map_err(error)?;}
 let mover=if moving{Some(w.spawn(GridPosition::default()).map_err(error)?)}else{None};
 w.start_attack(a,b,AttackProfile{range:32,damage:n(1),cooldown_ticks:NonZeroU16::new(1).unwrap(),windup_ticks:1}).map_err(error)?;
 let mut net=CombatConnections::with_window(700,NetworkConfig{connections:2,quantum_bytes:64,retry_ms:25,ack_timeout_ms:2000,..NetworkConfig::default()},if windowed{4}else{1}).map_err(error)?;
 let ids=[net.connect().map_err(error)?,net.connect().map_err(error)?];let view=ViewRect{min:GridPosition{x:-100,y:-100},max:GridPosition{x:100,y:100}};
 let mut spatial=SpatialFrame::new(n(64));if moving{for id in ids{net.set_motion_view(id,view).map_err(error)?;}}
 let mut frame=CombatFrame::new(n(64));let mut observations=ObservationQueue::new(4).map_err(error)?;let targets=ids.map(|connection|ObservationTarget{connection,view});let mut failures=Vec::new();let mut acks=[[0u8;20];2];let mut filled=[0usize;2];let mut notices=Vec::new();let mut expected=Vec::new();let mut reference=CombatSyncSender::new_shielded(999,CombatSyncConfig::default()).map_err(error)?;let mut oracle=CombatReplica::new(999);
 let start=Instant::now();let mut ticks=0;let mut next_tick=0;let(mut bytes,mut retries)=(0,0);
 loop{let now=start.elapsed().as_millis()as u64;if now>5000{return Err(error("finite example timeout").into());}
  if ticks<20&&now>=next_tick{w.advance_tick().map_err(error)?;let h=w.health(b).map_err(error)?;let mut ev=[CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)}];let mut changes=[HealthChange{entity:b,before:h,after:h}];let report=w.resolve_combat(&mut ev,&mut changes).map_err(error)?;
   if let Some(mover)=mover{w.set_position(mover,GridPosition{x:(ticks+1)as i32,y:0}).map_err(error)?;spatial.capture(&w).map_err(error)?;}frame.capture(&w,&ev[..report.events_written]).map_err(error)?;reference.observe(&frame,view).map_err(error)?;if let Some(packet)=reference.prepare().map_err(error)?{let r=oracle.apply(packet).map_err(error)?;expected.extend(oracle.drain_feedback());reference.acknowledge_packet(&r.acknowledgement()).map_err(error)?;}
   observations.enqueue(&net,&frame,&targets).map_err(error)?;ticks+=1;next_tick=now+5;
  }
  for i in 0..2{loop{match sockets[i].read(&mut acks[i][filled[i]..]){
   Ok(0)=>return Err(error("unexpected EOF").into()),Ok(count)=>{filled[i]+=count;if filled[i]==20{net.acknowledge(ids[i],&acks[i]).map_err(error)?;filled[i]=0;}},
   Err(e)if e.kind()==io::ErrorKind::WouldBlock=>break,Err(e)if e.kind()==io::ErrorKind::Interrupted=>continue,Err(e)=>return Err(e.into()),
  }}}
  let mut budget=NetworkBudget{work:4,bytes:93};let observed=observations.poll(&mut net,&mut budget,&mut failures).map_err(error)?;if !failures.is_empty(){return Err(error(&failures).into());}
  let write=|id:ConnectionId,p:&[u8]|match sockets[id.index as usize].write(p){Ok(n)=>WriteProgress::Written(n),Err(e)if matches!(e.kind(),io::ErrorKind::WouldBlock|io::ErrorKind::Interrupted)=>WriteProgress::WouldBlock,Err(_)=>WriteProgress::Disconnected};let report=if moving{net.poll_with_motion(now,&spatial,&mut budget,&mut notices,write)}else{net.poll(now,&mut budget,&mut notices,write)}.map_err(error)?;assert!(observed.visited+report.visited<=4);
  if !notices.is_empty(){return Err(error(&notices).into());}bytes+=report.bytes;retries+=report.retries;assert!(report.bytes<=93&&report.visited<=2);
  if ticks==20&&observations.pending()==0&&ids.iter().all(|&id|{let s=net.status(id).unwrap();s.pending_bytes==0&&s.queued_events==0&&(!moving||net.motion_status(id).unwrap().is_some_and(|m|m.pending_bytes==0&&m.last_prepared_tick==Some(w.tick())))}){break;}thread::sleep(Duration::from_millis(1));
 }
 for socket in &sockets{socket.shutdown(Shutdown::Write)?;}let hp=w.health(b).map_err(error)?.current;assert_eq!(hp,990);assert_eq!(expected.len(),20);let mut results=Vec::new();for c in clients{let result=c.join().map_err(|_|error("client panic"))??;assert_eq!(result.events,expected);assert_eq!(result.hp,hp);if moving{assert_eq!(result.position,Some(GridPosition{x:20,y:0}));assert!(result.motion_packets>0);}results.push(result);}
 if !windowed{assert!(results[1].duplicates>=1);assert!(retries>=1);}println!("windowed={windowed} moving={moving} connections=2 ticks={ticks} events_each={} hp={hp} bytes_with_prefix={bytes} retries={retries} packets={:?} motion_packets={:?} duplicates={:?}",expected.len(),results.iter().map(|r|r.packets).collect::<Vec<_>>(),results.iter().map(|r|r.motion_packets).collect::<Vec<_>>(),results.iter().map(|r|r.duplicates).collect::<Vec<_>>());Ok(())
}
