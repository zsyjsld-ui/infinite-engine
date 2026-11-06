//! Finite loopback TCP demonstration. Two processes, no external service.
//! Default injects one dropped ACK and one ACK split across a retry timeout.
//! Pass --clean for the same battle without these transport faults.
//! --compact selects IC v2; --batched selects IC v3; default remains IC v1.
use infinite_core::*;
use std::{io::{self,Read,Write},net::{TcpListener,TcpStream,SocketAddr,Shutdown},
    num::{NonZeroU16,NonZeroU32},process::{Child,Command,Stdio},time::{Duration,Instant}};
type Result<T> = std::result::Result<T,Box<dyn std::error::Error>>;
fn invalid(message:&str)->io::Error{io::Error::new(io::ErrorKind::InvalidData,message)}
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn configure(socket:&TcpStream)->io::Result<()>{
    // Windows accepted sockets can inherit the listener's nonblocking mode.
    // ACK polling here uses blocking reads with a real timeout, not WouldBlock spin.
    socket.set_nonblocking(false)?;
    socket.set_nodelay(true)?;socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))
}

fn write_frame(socket:&mut impl Write,bytes:&[u8])->io::Result<()>{
    if !(COMBAT_SYNC_HEADER_BYTES..=MAX_COMBAT_SYNC_PACKET_BYTES).contains(&bytes.len()){return Err(invalid("frame length"));}
    let prefix=(bytes.len()as u32).to_le_bytes();
    // Deliberately separate writes: TCP is a byte stream, not packet messages.
    socket.write_all(&prefix[..1])?;socket.write_all(&prefix[1..])?;
    for part in bytes.chunks(7){socket.write_all(part)?;}Ok(())
}
fn read_frame(socket:&mut impl Read,bytes:&mut Vec<u8>)->io::Result<bool>{
    let mut prefix=[0;4];let mut at=0;
    while at<4{match socket.read(&mut prefix[at..]){
        Ok(0)if at==0=>return Ok(false),
        Ok(0)=>return Err(io::Error::new(io::ErrorKind::UnexpectedEof,"partial length prefix")),
        Ok(count)=>at+=count,Err(e)if e.kind()==io::ErrorKind::Interrupted=>{},Err(e)=>return Err(e),
    }}
    let length=u32::from_le_bytes(prefix)as usize;
    if !(COMBAT_SYNC_HEADER_BYTES..=MAX_COMBAT_SYNC_PACKET_BYTES).contains(&length){return Err(invalid("frame length"));}
    bytes.resize(length,0);socket.read_exact(bytes)?;Ok(true)
}

#[derive(Default)]
struct AckReader { bytes:[u8;20],filled:usize }
impl AckReader{
    /// Retain the prefix on timeout. Restarting read_exact at byte zero would
    /// misinterpret a partial ACK's suffix after retransmission.
    fn poll(&mut self,socket:&mut impl Read)->io::Result<Option<[u8;20]>>{
        while self.filled<20{match socket.read(&mut self.bytes[self.filled..]){
            Ok(0)=>return Err(io::Error::new(io::ErrorKind::UnexpectedEof,"connection closed before ACK")),
            Ok(count)=>self.filled+=count,
            Err(e)if e.kind()==io::ErrorKind::Interrupted=>{},
            Err(e)if matches!(e.kind(),io::ErrorKind::TimedOut|io::ErrorKind::WouldBlock)=>return Ok(None),
            Err(e)=>return Err(e),
        }}
        self.filled=0;Ok(Some(self.bytes))
    }
}
#[derive(Default)]
struct TransportStats { writes:usize,retries:usize,timeouts:usize,payload_bytes:usize }
fn transmit(socket:&mut TcpStream,bytes:&[u8],stats:&mut TransportStats,retry:bool)->io::Result<()>{
    write_frame(socket,bytes)?;stats.writes+=1;stats.retries+=usize::from(retry);stats.payload_bytes+=bytes.len();Ok(())
}
fn await_ack(socket:&mut TcpStream,reader:&mut AckReader,sender:&mut CombatSyncSender,stats:&mut TransportStats)->Result<()>{
    let deadline=Instant::now()+Duration::from_secs(5);let mut retries=0;
    while sender.pending_packet().is_some(){
        if Instant::now()>=deadline{return Err(io::Error::new(io::ErrorKind::TimedOut,"ACK deadline").into());}
        if let Some(ack)=reader.poll(socket)?{sender.acknowledge_packet(&ack).map_err(|e|invalid(&format!("ACK: {e:?}")))?;}
        else{
            stats.timeouts+=1;retries+=1;if retries>3{return Err(invalid("ACK retry limit").into());}
            println!("server timeout retained_ack_bytes={}",reader.filled);
            transmit(socket,sender.pending_packet().unwrap(),stats,true)?;
        }
    }Ok(())
}
struct OwnedChild(Option<Child>);
impl Drop for OwnedChild{fn drop(&mut self){if let Some(mut child)=self.0.take(){let _=child.kill();if let Ok(output)=child.wait_with_output(){eprint!("{}{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));}}}}

fn client(address:SocketAddr,faults:bool)->Result<()>{
    if !address.ip().is_loopback(){return Err(invalid("loopback address required").into());}
    let mut socket=TcpStream::connect_timeout(&address,Duration::from_secs(5))?;configure(&socket)?;
    let mut replica=CombatReplica::new(17);let mut bytes=Vec::new();let mut first=Vec::new();
    let(mut packets,mut duplicates,mut dropped,mut fragments)=(0,0,0,0);let mut feedback=Vec::new();let mut split_ack=None;
    while read_frame(&mut socket,&mut bytes)?{
        packets+=1;let report=replica.apply(&bytes).map_err(|e|invalid(&format!("replica: {e:?}")))?;
        let events:Vec<_>=replica.drain_feedback().collect();
        if report.duplicate{duplicates+=1;assert!(events.is_empty());assert_eq!(bytes,first,"retry must preserve exact bytes");}
        for e in events{
            assert_eq!(e.attacker,EntityId{index:0,generation:0});assert_eq!(e.target,EntityId{index:1,generation:0});
            let code=match e.kind{CombatFeedbackKind::WindupStarted{..}=>0,CombatFeedbackKind::WindupCancelled{..}=>1,CombatFeedbackKind::Hit{..}=>2,CombatFeedbackKind::Ended(_)=>3};
            feedback.push((e.tick.0,code));println!("client event tick={} kind={code}",e.tick.0);
        }
        assert_eq!(replica.states().len(),2);assert!(replica.states().iter().all(|s|s.entity.index<2));
        println!("client packet={} tick={} duplicate={} events={}",report.sequence,report.tick.0,report.duplicate,report.events);
        let ack=report.acknowledgement();
        if faults&&report.sequence==1{
            if !report.duplicate{first=bytes.clone();dropped+=1;println!("client fault=drop_ack");continue;}
            if split_ack.is_none()&&fragments==0{socket.write_all(&ack[..7])?;split_ack=Some(ack);fragments+=1;println!("client fault=partial_ack bytes=7");continue;}
            if let Some(saved)=split_ack.take(){socket.write_all(&saved[7..])?;fragments+=1;println!("client partial_ack_completed bytes=13");continue;}
        }
        socket.write_all(&ack)?;
    }
    assert_eq!(feedback,[(1,0),(3,2),(5,0),(6,1),(9,0),(13,0),(15,2)]);
    let target=replica.states().iter().find(|s|s.entity.index==1).unwrap();assert_eq!(target.health.unwrap().current,30);
    assert!(replica.states().iter().all(|s|s.windup.is_none()));
    assert_eq!((duplicates,dropped,fragments),if faults{(2,1,2)}else{(0,0,0)});
    println!("CLIENT_OK packets={packets} duplicates={duplicates} dropped_ack={dropped} ack_fragments={fragments} events={} final_hp=30",feedback.len());Ok(())
}

fn server(faults:bool,compact:bool,batched:bool)->Result<()>{
    let listener=TcpListener::bind("127.0.0.1:0")?;let address=listener.local_addr()?;listener.set_nonblocking(true)?;
    let mut command=Command::new(std::env::current_exe()?);
    command.arg("--client").arg(address.to_string()).arg(if faults{"faults"}else{"clean"}).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)] {use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
    let mut child=OwnedChild(Some(command.spawn()?));let deadline=Instant::now()+Duration::from_secs(5);
    let(mut socket,_)=loop{match listener.accept(){Ok(pair)=>break pair,Err(e)if e.kind()==io::ErrorKind::WouldBlock=>{
        if Instant::now()>=deadline{return Err(io::Error::new(io::ErrorKind::TimedOut,"accept deadline").into());}
        std::thread::sleep(Duration::from_millis(5));},Err(e)=>return Err(e.into())}};
    configure(&socket)?;if faults{socket.set_read_timeout(Some(Duration::from_millis(250)))?;}
    println!("server listening={address} mode={} wire=IC{}",if faults{"faults"}else{"clean"},if batched{3}else if compact{2}else{1});
    let mut world=World::default();let a=world.spawn(p(0)).unwrap();let b=world.spawn(p(64)).unwrap();let hidden=world.spawn(p(10000)).unwrap();
    for id in [a,b,hidden]{world.configure_health(id,50,n(50)).unwrap();}
    let attack=AttackProfile{range:96,damage:n(10),cooldown_ticks:NonZeroU16::new(2).unwrap(),windup_ticks:2};world.start_attack(a,b,attack).unwrap();
    let mut frame=CombatFrame::new(n(512));let config=CombatSyncConfig{events_per_packet:2,..CombatSyncConfig::default()};
    let mut sender=if batched{CombatSyncSender::new_batched(17,config)}else if compact{CombatSyncSender::new_compact(17,config)}else{CombatSyncSender::new(17,config)}.unwrap();
    let view=ViewRect{min:p(-512),max:p(512)};let h=world.health(b).unwrap();
    let mut events=[CombatEvent{attacker:a,target:b,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};4];let mut changes=[HealthChange{entity:b,before:h,after:h};4];
    let mut reader=AckReader::default();let mut stats=TransportStats::default();let mut saved=Vec::new();
    for step in 1..=16{
        if step==6{world.set_position(b,p(256)).unwrap();}if step==8{world.set_position(b,p(64)).unwrap();}
        if step==10{world.stop_attack(a).unwrap();}if step==11{world.start_attack(a,b,attack).unwrap();}
        world.advance_tick().unwrap();let r=world.resolve_combat(&mut events,&mut changes).unwrap();frame.capture(&world,&events[..r.events_written]).unwrap();sender.observe(&frame,view).unwrap();
        if faults&&step>1{assert_eq!(sender.pending_packet().unwrap(),saved);continue;}
        if let Some(bytes)=sender.prepare().unwrap(){transmit(&mut socket,bytes,&mut stats,false)?;}
        if faults{
            saved=sender.pending_packet().unwrap().to_vec();assert!(reader.poll(&mut socket)?.is_none());assert_eq!(reader.filled,0);stats.timeouts+=1;
            println!("server timeout retained_ack_bytes=0; observing ticks 2..16 before retry");
        }else if sender.pending_packet().is_some(){await_ack(&mut socket,&mut reader,&mut sender,&mut stats)?;}
    }
    if faults{
        assert_eq!(sender.queued_events(),7);assert_eq!(sender.pending_packet().unwrap(),saved);
        println!("server accumulated_events=7 latest_tick=16");transmit(&mut socket,sender.pending_packet().unwrap(),&mut stats,true)?;
        await_ack(&mut socket,&mut reader,&mut sender,&mut stats)?;
    }
    while let Some(bytes)=sender.prepare().unwrap(){transmit(&mut socket,bytes,&mut stats,false)?;await_ack(&mut socket,&mut reader,&mut sender,&mut stats)?;}
    assert_eq!(sender.queued_events(),0);assert_eq!(world.health(b).unwrap().current,30);
    socket.shutdown(Shutdown::Write)?;drop(socket);
    let output=child.0.take().unwrap().wait_with_output()?;
    print!("{}",String::from_utf8(output.stdout)?);if !output.status.success(){return Err(invalid(&String::from_utf8_lossy(&output.stderr)).into());}
    assert!(output.stderr.is_empty());assert_eq!((stats.retries,stats.timeouts),if faults{(2,2)}else{(0,0)});
    println!("SERVER_OK writes={} retries={} timeouts={} payload_bytes={} framing_bytes={} pending_events=0 final_hp=30",stats.writes,stats.retries,stats.timeouts,stats.payload_bytes,stats.writes*4);Ok(())
}
fn main()->Result<()>{
    let args:Vec<_>=std::env::args().collect();
    match args.get(1).map(String::as_str){
        Some("--client")if args.len()==4=>client(args[2].parse()?,args[3]=="faults"),
        _ if args[1..].iter().all(|a|a=="--clean"||a=="--compact"||a=="--batched")=>server(!args.iter().any(|a|a=="--clean"),args.iter().any(|a|a=="--compact"),args.iter().any(|a|a=="--batched")),
        _=>Err(invalid("usage: combat_sync_tcp [--clean] [--compact|--batched]").into()),
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    use std::collections::VecDeque;
    struct Scripted { chunks:VecDeque<io::Result<Vec<u8>>> }
    impl Read for Scripted{fn read(&mut self,out:&mut[u8])->io::Result<usize>{
        let Some(part)=self.chunks.pop_front()else{return Ok(0)};let mut part=part?;let count=part.len().min(out.len());out[..count].copy_from_slice(&part[..count]);
        if part.len()>count{part.drain(..count);self.chunks.push_front(Ok(part));}Ok(count)
    }}
    #[test]
    fn partial_ack_survives_timeouts_and_consecutive_acks(){
        let a=[11;20];let b=[22;20];let mut chunks=VecDeque::new();chunks.push_back(Ok(a[..7].to_vec()));chunks.push_back(Err(io::Error::from(io::ErrorKind::TimedOut)));chunks.push_back(Ok([a[7..].to_vec(),b.to_vec()].concat()));
        let mut socket=Scripted{chunks};let mut reader=AckReader::default();assert_eq!(reader.poll(&mut socket).unwrap(),None);assert_eq!(reader.filled,7);assert_eq!(reader.poll(&mut socket).unwrap(),Some(a));assert_eq!(reader.poll(&mut socket).unwrap(),Some(b));
        assert_eq!(reader.poll(&mut socket).unwrap_err().kind(),io::ErrorKind::UnexpectedEof);
    }
    #[test]
    fn framed_reads_handle_short_chunks_eof_and_bounds(){
        let body=vec![7;40];let mut wire=Vec::new();write_frame(&mut wire,&body).unwrap();let mut socket=Scripted{chunks:wire.chunks(3).map(|c|Ok(c.to_vec())).collect()};let mut bytes=Vec::new();assert!(read_frame(&mut socket,&mut bytes).unwrap());assert_eq!(bytes,body);assert!(!read_frame(&mut socket,&mut bytes).unwrap());
        for length in 1..wire.len(){assert_eq!(read_frame(&mut &wire[..length],&mut bytes).unwrap_err().kind(),io::ErrorKind::UnexpectedEof);}
        for length in [0,39,MAX_COMBAT_SYNC_PACKET_BYTES as u32+1]{assert_eq!(read_frame(&mut &length.to_le_bytes()[..],&mut bytes).unwrap_err().kind(),io::ErrorKind::InvalidData);}
    }
}
