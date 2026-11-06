use infinite_core::*;
use std::collections::VecDeque;
use std::time::Instant;
use std::{io::{Read,Write,ErrorKind},net::{TcpListener,TcpStream}};
use super::simulation::{Simulation,full_view};
use super::oracle::{self,Snapshot};
struct Chunk{due:u64,bytes:Vec<u8>,at:usize}
fn delay(rtt:u64,jitter:bool,now:u64,index:usize)->u64{rtt/2+if jitter{(now/5+index as u64*17)%5*5}else{0}}
struct Link{
    chunks:VecDeque<Chunk>,buffered:usize,decoder:Vec<u8>,acks:VecDeque<(u64,[u8;20])>,
    combat:CombatReplica,motion:Replica,components:ComponentReplica,
    expected:VecDeque<CombatFeedback>,events:usize,packets:usize,
    sockets:Option<(TcpStream,TcpStream)>,ack_offset:usize,ack_decoder:Vec<u8>,
    last_write:Option<u64>,max_write_gap:u64,
}
fn socket_pair()->(TcpStream,TcpStream){
    let listener=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).unwrap();
    let client=TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let(server,_)=listener.accept().unwrap();
    for socket in [&server,&client]{socket.set_nodelay(true).unwrap();socket.set_nonblocking(true).unwrap();}
    (server,client)
}
impl Link{
    fn new(stream:u64,tcp:bool)->Self{Self{chunks:VecDeque::new(),buffered:0,decoder:Vec::new(),acks:VecDeque::new(),
        combat:CombatReplica::new_windowed(stream),motion:Replica::new(stream),components:ComponentReplica::new_with_definitions(stream),
        expected:VecDeque::new(),events:0,packets:0,sockets:tcp.then(socket_pair),ack_offset:0,ack_decoder:Vec::new(),last_write:None,max_write_gap:0}}
}
#[derive(Default,Debug)]
pub struct Traffic{pub bytes:usize,pub ack_bytes:usize,pub retries:usize,pub blocked:usize,
    pub lost_acks:usize,pub duplicates:usize,pub delivered:usize,pub max_event_age:u64,
    pub peak_expected:usize,pub peak_transport:usize,
    pub sender_ms:f64,pub ack_ms:f64,pub observe_ms:f64,pub client_model_ms:f64,pub oracle_capture_ms:f64,
    pub socket_data_bytes:usize,pub socket_ack_bytes:usize}
pub struct Transport{
    net:CombatConnections,connections:Vec<ConnectionId>,links:Vec<Link>,owners:Vec<EntityId>,
    view:ViewRect,notices:Vec<NetworkNotice>,rtt:u64,loss_every:usize,pub traffic:Traffic,history:VecDeque<Snapshot>,
    pub jitter:bool,
}
impl Transport{
    pub fn new(sim:&Simulation,peers:usize,first_stream:u64,rtt:u64,loss_every:usize,tcp:bool)->Self{
        let mut net=CombatConnections::with_window(first_stream,NetworkConfig{connections:peers,quantum_bytes:2048,
            retry_ms:250,ack_timeout_ms:20000,blocked_timeout_ms:20000,
            combat:CombatSyncConfig{pending_events:8192,events_per_packet:128}},4).unwrap();
        let view=full_view();let mut connections=Vec::new();let mut owners=Vec::new();
        for i in 0..peers{let id=net.connect().unwrap();let owner=sim.ids[i%sim.ids.len()];
            net.set_motion_view(id,view).unwrap();net.set_component_view(id,Some(owner),view).unwrap();
            connections.push(id);owners.push(owner);}
        Self{net,connections,owners,links:(0..peers).map(|i|Link::new(first_stream+i as u64,tcp)).collect(),
            view,notices:Vec::new(),rtt,loss_every,traffic:Traffic::default(),history:VecDeque::new(),jitter:false}
    }
    pub fn observe(&mut self,sim:&Simulation){
        let mut view=full_view();view.max.x=if sim.world.tick().0%32<16{448}else{1024};
        if self.view!=view{
            self.view=view;
            for (index,&id) in self.connections.iter().enumerate(){
                self.net.set_motion_view(id,view).unwrap();self.net.set_component_view(id,Some(self.owners[index]),view).unwrap();
            }
        }
        let start=Instant::now();let expected=sim.expected(self.view);
        self.history.push_back(Snapshot::capture(sim,self.view));if self.history.len()>512{self.history.pop_front();}
        self.traffic.oracle_capture_ms+=start.elapsed().as_secs_f64()*1000.;
        for (index,&id) in self.connections.iter().enumerate(){
            let start=Instant::now();
            self.net.observe(id,&sim.combat,self.view).unwrap();
            self.traffic.observe_ms+=start.elapsed().as_secs_f64()*1000.;
            let start=Instant::now();
            self.links[index].expected.extend(expected.iter().copied());
            self.traffic.peak_expected=self.traffic.peak_expected.max(self.links[index].expected.len());
            assert!(self.links[index].expected.len()<=8192+4*128);
            self.traffic.oracle_capture_ms+=start.elapsed().as_secs_f64()*1000.;
        }
    }
    pub fn disconnect_and_resume(&mut self){
        for (index,&id) in self.connections.iter().enumerate(){
            self.net.detach(id).unwrap();let link=&mut self.links[index];
            link.chunks.clear();link.decoder.clear();link.buffered=0;link.acks.clear();
            link.ack_offset=0;link.ack_decoder.clear();if link.sockets.is_some(){link.sockets=Some(socket_pair());}
            self.net.resume(id).unwrap();
        }
    }
    pub fn poll(&mut self,sim:&Simulation,now:u64,work:usize,bytes:usize,slow:bool)->NetworkReport{
        for (index,link) in self.links.iter_mut().enumerate(){
            let start=Instant::now();
            if let Some((server,client))=&mut link.sockets{
                while link.acks.front().is_some_and(|a|a.0<=now){
                    let ack=&link.acks.front().unwrap().1;
                    match client.write(&ack[link.ack_offset..]){
                        Ok(0)=>panic!("TCP ACK writer closed"),
                        Ok(n)=>{link.ack_offset+=n;self.traffic.socket_ack_bytes+=n;if link.ack_offset==20{link.acks.pop_front();link.ack_offset=0;}},
                        Err(e)if e.kind()==ErrorKind::WouldBlock=>break,Err(e)=>panic!("TCP ACK write: {e}"),
                    }
                }
                let mut bytes=[0u8;512];loop{match server.read(&mut bytes){
                    Ok(0)=>panic!("TCP ACK reader closed"),Ok(n)=>link.ack_decoder.extend_from_slice(&bytes[..n]),
                    Err(e)if e.kind()==ErrorKind::WouldBlock=>break,Err(e)=>panic!("TCP ACK read: {e}"),
                }}
                while link.ack_decoder.len()>=20{self.net.acknowledge(self.connections[index],&link.ack_decoder[..20]).unwrap();link.ack_decoder.drain(..20);}
            }else{
                while link.acks.front().is_some_and(|a|a.0<=now){let(_,ack)=link.acks.pop_front().unwrap();self.net.acknowledge(self.connections[index],&ack).unwrap();}
            }
            self.traffic.ack_ms+=start.elapsed().as_secs_f64()*1000.;
            if slow&&index==0&&(500..1000).contains(&now){continue;}
            let start=Instant::now();
            let mut read=if slow&&index%8==0{1024}else{16384};
            if let Some((_,client))=&mut link.sockets{
                let mut bytes=[0u8;16384];match client.read(&mut bytes[..read]){
                    Ok(0)=>panic!("TCP data reader closed"),
                    Ok(n)=>link.chunks.push_back(Chunk{due:now+delay(self.rtt,self.jitter,now,index),bytes:bytes[..n].to_vec(),at:0}),
                    Err(e)if e.kind()==ErrorKind::WouldBlock=>{},Err(e)=>panic!("TCP data read: {e}"),
                }
            }
            while read>0&&link.chunks.front().is_some_and(|chunk|chunk.due<=now){
                let chunk=link.chunks.front_mut().unwrap();let len=read.min(chunk.bytes.len()-chunk.at);
                link.decoder.extend_from_slice(&chunk.bytes[chunk.at..chunk.at+len]);
                read-=len;chunk.at+=len;link.buffered-=len;
                if chunk.at==chunk.bytes.len(){link.chunks.pop_front();}
            }
            loop{
                if link.decoder.len()<4{break;}
                let len=u32::from_le_bytes(link.decoder[..4].try_into().unwrap())as usize;
                assert!(len<=MAX_COMPONENT_PACKET_BYTES.max(MAX_COMBAT_SYNC_PACKET_BYTES));
                if link.decoder.len()<len+4{break;}
                let packet=&link.decoder[4..len+4];
                let(ack,duplicate)=match &packet[..2]{
                    b"IC"=>{let report=link.combat.apply(packet).unwrap();
                        if !report.duplicate{let snapshot=self.history.iter().find(|s|s.tick==report.tick).expect("retained authority snapshot");assert_eq!(oracle::vital(link.combat.states()),snapshot.vital,"vital frame {:?}",report.tick);}
                        for event in link.combat.drain_feedback(){
                        assert_eq!(Some(event),link.expected.pop_front(),"ordered authority feedback at {now}");
                        link.events+=1;self.traffic.delivered+=1;
                        self.traffic.max_event_age=self.traffic.max_event_age.max(now-event.tick.0*50);
                    }(report.acknowledgement(),report.duplicate)},
                    b"IS"=>{let report=link.motion.apply(packet).unwrap();if !report.duplicate{let snapshot=self.history.iter().find(|s|s.tick==report.tick).expect("retained motion snapshot");assert_eq!(oracle::motion(link.motion.states()),snapshot.motion,"motion frame {:?}",report.tick);}(report.acknowledgement(),report.duplicate)},
                    b"IP"=>{let report=link.components.apply(packet).unwrap();if !report.duplicate{let snapshot=self.history.iter().find(|s|s.tick==report.tick).expect("retained component snapshot");assert_eq!(oracle::components(link.components.states()),snapshot.components(self.owners[index]),"component frame {:?}",report.tick);assert_eq!(link.components.definition(1).unwrap().bytes.as_ref(),&[(report.tick.0/40)as u8]);}(report.acknowledgement(),report.duplicate)},
                    _=>panic!("unknown channel"),
                };
                self.traffic.duplicates+=usize::from(duplicate);link.packets+=1;
                if self.loss_every>0&&link.packets%self.loss_every==0{self.traffic.lost_acks+=1;}
                else{link.acks.push_back((now+delay(self.rtt,self.jitter,now,index),ack));self.traffic.ack_bytes+=20;}
                link.decoder.drain(..len+4);
            }
            self.traffic.client_model_ms+=start.elapsed().as_secs_f64()*1000.;
        }
        let mut budget=NetworkBudget{work,bytes};let links=&mut self.links;let rtt=self.rtt;let jitter=self.jitter;
        let start=Instant::now();
        let mut socket_bytes=0;
        let report=self.net.poll_with_components(now,&sim.components,&mut budget,&mut self.notices,|id,part|{
            let link=&mut links[id.index as usize];let len=part.len().min(32768-link.buffered);
            if len==0{return WriteProgress::WouldBlock;}
            if let Some((server,_))=&mut link.sockets{
                return match server.write(&part[..len]){
                    Ok(0)=>WriteProgress::Disconnected,Ok(n)=>{link.buffered+=n;socket_bytes+=n;if let Some(previous)=link.last_write{link.max_write_gap=link.max_write_gap.max(now-previous);}link.last_write=Some(now);WriteProgress::Written(n)},
                    Err(e)if e.kind()==ErrorKind::WouldBlock=>WriteProgress::WouldBlock,Err(_)=>WriteProgress::Disconnected,
                };
            }
            link.chunks.push_back(Chunk{due:now+delay(rtt,jitter,now,id.index as usize),bytes:part[..len].to_vec(),at:0});link.buffered+=len;
            if let Some(previous)=link.last_write{link.max_write_gap=link.max_write_gap.max(now-previous);}link.last_write=Some(now);
            WriteProgress::Written(len)
        }).unwrap();
        self.traffic.sender_ms+=start.elapsed().as_secs_f64()*1000.;
        self.traffic.socket_data_bytes+=socket_bytes;
        assert!(self.notices.is_empty(),"{now}: {:?}",self.notices);
        assert_eq!(budget.work+report.visited,work);assert_eq!(budget.bytes+report.bytes,bytes);
        self.traffic.bytes+=report.bytes;self.traffic.retries+=report.retries;self.traffic.blocked+=report.blocked;
        self.traffic.peak_transport=self.traffic.peak_transport.max(self.links.iter().map(|l|l.buffered+l.decoder.len()).sum());
        report
    }
    pub fn drained(&self,tick:Tick)->bool{
        self.connections.iter().enumerate().all(|(i,&id)|{
            let c=self.net.status(id).unwrap();let m=self.net.motion_status(id).unwrap().unwrap();let p=self.net.component_status(id).unwrap().unwrap();
            c.pending_bytes==0&&c.queued_events==0&&c.inflight_packets==0&&m.pending_bytes==0&&m.last_prepared_tick==Some(tick)
                &&p.pending_bytes==0&&p.last_prepared_tick==Some(tick)&&self.links[i].chunks.is_empty()
                &&self.links[i].buffered==0&&self.links[i].decoder.is_empty()&&self.links[i].acks.is_empty()
                &&self.links[i].ack_decoder.is_empty()&&self.links[i].expected.is_empty()
        })
    }
    pub fn verify(&self,sim:&Simulation){
        let motion=sim.components.motion_frame().visible(self.view).unwrap();
        for (index,link) in self.links.iter().enumerate(){
            assert_eq!(link.motion.states(),motion);
            let components=ComponentSelector::new(Some(self.owners[index])).select(&sim.components,self.view).unwrap().to_vec();
            assert_eq!(link.components.states(),components);
            for state in link.combat.states(){
                assert_eq!(state.health,sim.world.health(state.entity).ok());
                let windup=sim.world.attack_state(state.entity).unwrap().windup
                    .filter(|w|sim.world.position(w.target).ok().is_some_and(|p|self.view.contains(p)))
                    .map(|w|WindupView{target:w.target,started_at:w.started_at,hit_at:w.hit_at});
                assert_eq!(state.windup,windup);
            }
            assert_eq!(link.combat.states().len(),motion.len());assert!(link.expected.is_empty());
        }
    }
    /// Includes idle periods, intentional pauses and ACK waits; not an active
    /// queue starvation bound. Report each connection rather than just a mean.
    pub fn write_gaps(&self)->Vec<u64>{self.links.iter().map(|link|link.max_write_gap).collect()}
}
