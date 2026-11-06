//! Bounded, nonblocking multi-recipient combat transmission. No socket ownership.
use crate::{CombatFrame,CombatSyncConfig,CombatSyncError,CombatSyncSender,ViewRect};
use crate::network_window::{SendWindow,RetainedCombatSender,MAX_COMBAT_WINDOW_PACKETS};
use crate::network_motion::MotionPeer;
use crate::network_component::ComponentPeer;

pub const MAX_NETWORK_CONNECTIONS:usize=4096;
pub const MAX_NETWORK_WORK:usize=65_536;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
pub struct ConnectionId {pub index:u32,pub generation:u32}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct NetworkConfig {
    pub connections:usize,pub quantum_bytes:usize,pub retry_ms:u64,
    pub ack_timeout_ms:u64,pub blocked_timeout_ms:u64,pub combat:CombatSyncConfig,
}
impl Default for NetworkConfig {fn default()->Self{Self{connections:256,quantum_bytes:16*1024,retry_ms:200,ack_timeout_ms:5000,blocked_timeout_ms:5000,combat:CombatSyncConfig::default()}}}
/// Share this object across polls/channels in a host service interval. A visit
/// includes at most one encode and one nonblocking write; bytes count prefix too.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct NetworkBudget {pub work:usize,pub bytes:usize}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum NetworkError {InvalidConfig,InvalidBudget,Full,InvalidConnection,TimeReversed,StreamExhausted,StillAttached,Unacknowledged,Sync(CombatSyncError),Component(crate::ComponentError)}
impl From<CombatSyncError> for NetworkError {fn from(e:CombatSyncError)->Self{Self::Sync(e)}}
impl From<crate::ComponentError> for NetworkError {fn from(e:crate::ComponentError)->Self{Self::Component(e)}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum WriteProgress {Written(usize),WouldBlock,Disconnected}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum NetworkNoticeKind {Disconnected,AckTimeout,BlockedTimeout,InvalidWriter,EncodeFailed(CombatSyncError),MotionEncodeFailed(crate::SyncError),ComponentEncodeFailed(crate::ComponentError)}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct NetworkNotice {pub connection:ConnectionId,pub kind:NetworkNoticeKind}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct NetworkReport {pub visited:usize,pub prepared:usize,pub writes:usize,pub bytes:usize,pub packets:usize,pub retries:usize,pub waiting:usize,pub blocked:usize,pub detached:usize}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum AckDisposition {Committed,DeferredUntilFrameEnd,Duplicate}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ConnectionStatus {pub stream:u64,pub attached:bool,pub queued_events:usize,pub pending_bytes:usize,pub written_bytes:usize,pub waiting_ack:bool,pub blocked_since:Option<u64>,pub inflight_packets:usize}
struct Peer {
    stream:u64,sender:CombatSyncSender,attached:bool,observed:bool,dirty:bool,
    offset:usize,sent_once:bool,first_sent:Option<u64>,last_sent:Option<u64>,
    blocked_since:Option<u64>,confirmed:u64,deferred_ack:Option<[u8;20]>,
    window:Option<SendWindow>,
    motion:Option<MotionPeer>,prefer_motion:bool,
    components:Option<ComponentPeer>,next_channel:u8,
}
impl Peer {
    fn total(&self)->usize{self.sender.pending_packet().map_or(0,|p|p.len()+4)}
    fn reset_write(&mut self){self.offset=0;self.sent_once=false;self.first_sent=None;self.last_sent=None;self.blocked_since=None;self.deferred_ack=None;}
    fn commit_ack(&mut self,bytes:&[u8;20]){
        self.sender.acknowledge_packet(bytes).expect("validated pending ACK");
        self.confirmed=u64::from_le_bytes(bytes[12..20].try_into().unwrap());self.reset_write();self.dirty=true;
    }
    fn detach(&mut self){
        if let Some(motion)=&mut self.motion{motion.detach();}
        if let Some(components)=&mut self.components{components.detach();}
        if let Some(window)=&mut self.window{window.detach();}
        self.attached=false;self.offset=0;self.blocked_since=None;
        // A partial retry belonged to the old transport and is discarded by
        // that transport's decoder. Its already-valid ACK can now be committed.
        if let Some(ack)=self.deferred_ack{self.commit_ack(&ack);}
    }
}
struct Slot {generation:u32,peer:Option<Peer>}
/// Streams are monotonically issued from first_stream. The host supplies a new
/// namespace after process restart and authenticates every connection/ACK.
/// observe must admit every CombatFrame in order, including during disconnect;
/// on backpressure the host retains/retries that frame or starts explicit recovery.
pub struct CombatConnections {config:NetworkConfig,slots:Vec<Slot>,free:Vec<u32>,cursor:usize,next_stream:Option<u64>,now:u64,len:usize,window_packets:usize}
impl CombatConnections {
    pub fn new(first_stream:u64,config:NetworkConfig)->Result<Self,NetworkError>{
        if config.connections==0||config.connections>MAX_NETWORK_CONNECTIONS||config.quantum_bytes==0||config.quantum_bytes>crate::MAX_COMBAT_SYNC_PACKET_BYTES+4||config.retry_ms==0||config.ack_timeout_ms<config.retry_ms||config.blocked_timeout_ms==0{return Err(NetworkError::InvalidConfig);}
        if config.combat.pending_events==0||config.combat.pending_events>crate::MAX_PENDING_COMBAT_EVENTS||config.combat.events_per_packet==0||config.combat.events_per_packet>crate::MAX_COMBAT_EVENTS_PER_PACKET||config.combat.events_per_packet>config.combat.pending_events{return Err(NetworkError::InvalidConfig);}
        Ok(Self{config,slots:Vec::new(),free:Vec::new(),cursor:0,next_stream:Some(first_stream),now:0,len:0,window_packets:1})
    }
    /// Explicit IC v5 opt-in when packets > 1. Requires new_windowed replicas;
    /// new() and a one-packet window retain IC v4 behavior.
    pub fn with_window(first_stream:u64,config:NetworkConfig,packets:usize)->Result<Self,NetworkError>{if packets==0||packets>MAX_COMBAT_WINDOW_PACKETS{return Err(NetworkError::InvalidConfig);}let mut net=Self::new(first_stream,config)?;net.window_packets=packets;Ok(net)}
    pub fn len(&self)->usize{self.len}
    pub fn is_empty(&self)->bool{self.len==0}
    /// Movement-channel retained buffer/cache payload; excludes combat and IO.
    pub fn motion_retained_payload_capacity_bytes(&self)->usize{self.slots.iter().filter_map(|s|s.peer.as_ref()).filter_map(|p|p.motion.as_ref()).map(|m|m.sender.retained_payload_capacity_bytes()).sum()}
    pub fn retained_packet_capacity_bytes(&self,id:ConnectionId)->Result<usize,NetworkError>{Ok(self.peer(id)?.window.as_ref().map_or(0,SendWindow::capacity_bytes))}
    fn peer(&self,id:ConnectionId)->Result<&Peer,NetworkError>{self.slots.get(id.index as usize).filter(|s|s.generation==id.generation).and_then(|s|s.peer.as_ref()).ok_or(NetworkError::InvalidConnection)}
    fn peer_mut(&mut self,id:ConnectionId)->Result<&mut Peer,NetworkError>{self.slots.get_mut(id.index as usize).filter(|s|s.generation==id.generation).and_then(|s|s.peer.as_mut()).ok_or(NetworkError::InvalidConnection)}
    pub fn connect(&mut self)->Result<ConnectionId,NetworkError>{
        if self.len==self.config.connections||(self.free.is_empty()&&self.slots.len()==self.config.connections){return Err(NetworkError::Full);}
        let stream=self.next_stream.ok_or(NetworkError::StreamExhausted)?;
        let sender=CombatSyncSender::new_shielded(stream,self.config.combat)?;
        let index=if let Some(i)=self.free.pop(){i}else{let i=self.slots.len()as u32;self.slots.push(Slot{generation:0,peer:None});i};
        self.slots[index as usize].peer=Some(Peer{stream,sender,attached:true,observed:false,dirty:false,offset:0,sent_once:false,first_sent:None,last_sent:None,blocked_since:None,confirmed:0,deferred_ack:None,window:(self.window_packets>1).then(||SendWindow::new(self.window_packets)),motion:None,prefer_motion:false,components:None,next_channel:0});
        self.next_stream=stream.checked_add(1);self.len+=1;Ok(ConnectionId{index,generation:self.slots[index as usize].generation})
    }
    /// Hand ownership of undelivered events to the host; never silently discard.
    pub fn remove(&mut self,id:ConnectionId)->Result<RetainedCombatSender,NetworkError>{
        self.peer(id)?;let slot=&mut self.slots[id.index as usize];let peer=slot.peer.take().unwrap();
        if let Some(generation)=slot.generation.checked_add(1){slot.generation=generation;self.free.push(id.index);}self.len-=1;Ok(RetainedCombatSender{sender:peer.sender,packets:peer.window.map_or_else(Vec::new,SendWindow::into_packets),motion:peer.motion.map(|m|m.sender),components:peer.components.map(|c|c.sender)})
    }
    pub fn status(&self,id:ConnectionId)->Result<ConnectionStatus,NetworkError>{let p=self.peer(id)?;Ok(ConnectionStatus{stream:p.stream,attached:p.attached,queued_events:p.sender.queued_events()+p.window.as_ref().map_or(0,SendWindow::events),pending_bytes:p.window.as_ref().map_or_else(||p.total(),SendWindow::bytes),written_bytes:p.window.as_ref().map_or(p.offset,SendWindow::offset),waiting_ack:p.window.as_ref().map_or(p.sent_once,SendWindow::waiting),blocked_since:p.blocked_since,inflight_packets:p.window.as_ref().map_or(usize::from(p.sender.pending_packet().is_some()),SendWindow::len)})}
    pub fn observe(&mut self,id:ConnectionId,frame:&CombatFrame,view:ViewRect)->Result<(),NetworkError>{let p=self.peer_mut(id)?;p.sender.observe_with_inflight(frame,view,p.window.as_ref().map_or(0,SendWindow::events))?;p.observed=true;p.dirty=true;Ok(())}
    /// Enable/update the replaceable movement channel on the same transport.
    /// It uses the existing IS/IA wire format and this connection's stream ID.
    pub fn set_motion_view(&mut self,id:ConnectionId,view:ViewRect)->Result<(),NetworkError>{
        if view.min.x>view.max.x||view.min.y>view.max.y{return Err(CombatSyncError::from(crate::SyncError::InvalidView).into());}let p=self.peer_mut(id)?;if let Some(m)=&mut p.motion{m.set_view(view);}else{p.motion=Some(MotionPeer::new(p.stream,view));}Ok(())
    }
    pub fn motion_status(&self,id:ConnectionId)->Result<Option<crate::MotionConnectionStatus>,NetworkError>{Ok(self.peer(id)?.motion.as_ref().map(MotionPeer::status))}
    /// Explicit IP/IQ v2 opt-in. The authenticated owner is immutable for this
    /// subscription; a new character needs a new connection/stream identity.
    pub fn set_component_view(&mut self,id:ConnectionId,owner:Option<crate::EntityId>,view:ViewRect)->Result<(),NetworkError>{
        if view.min.x>view.max.x||view.min.y>view.max.y{return Err(crate::ComponentError::State(crate::SyncError::InvalidView).into());}
        let p=self.peer_mut(id)?;if let Some(c)=&mut p.components{if c.owner!=owner{return Err(NetworkError::InvalidConfig);}c.set_view(view);}else{p.components=Some(ComponentPeer::new(p.stream,owner,view));}Ok(())
    }
    pub fn component_status(&self,id:ConnectionId)->Result<Option<crate::ComponentConnectionStatus>,NetworkError>{Ok(self.peer(id)?.components.as_ref().map(ComponentPeer::status))}
    pub fn detach(&mut self,id:ConnectionId)->Result<(),NetworkError>{self.peer_mut(id)?.detach();Ok(())}
    /// Resume with the SAME replica/baseline. Reset the old transport's partial
    /// frame decoder first. The outstanding packet restarts from its prefix.
    pub fn resume(&mut self,id:ConnectionId)->Result<(),NetworkError>{
        let p=self.peer_mut(id)?;if p.attached{return Err(NetworkError::StillAttached);}p.attached=true;p.offset=0;p.first_sent=None;p.last_sent=None;p.blocked_since=None;if let Some(window)=&mut p.window{window.resume();}if let Some(m)=&mut p.motion{m.resume();}if let Some(c)=&mut p.components{c.resume();}Ok(())
    }
    /// New replica only after all events/packets are acknowledged. Otherwise
    /// preserve the sender via remove and use an application recovery policy.
    pub fn reset_replica(&mut self,id:ConnectionId)->Result<u64,NetworkError>{
        let p=self.peer(id)?;if p.sender.pending_packet().is_some()||p.sender.queued_events()>0||p.window.as_ref().is_some_and(|w|w.len()>0){return Err(NetworkError::Unacknowledged);}if p.attached{return Err(NetworkError::StillAttached);}
        let stream=self.next_stream.ok_or(NetworkError::StreamExhausted)?;let sender=CombatSyncSender::new_shielded(stream,self.config.combat)?;
        let capacity=self.window_packets;let p=self.peer_mut(id)?;p.sender=sender;p.stream=stream;p.confirmed=0;p.observed=false;p.dirty=false;p.reset_write();p.window=(capacity>1).then(||SendWindow::new(capacity));p.motion=p.motion.take().map(|m|MotionPeer::new(stream,m.view));p.prefer_motion=false;p.components=p.components.take().map(|c|ComponentPeer::new(stream,c.owner,c.view));p.next_channel=0;self.next_stream=stream.checked_add(1);Ok(stream)
    }
    pub fn acknowledge(&mut self,id:ConnectionId,bytes:&[u8])->Result<AckDisposition,NetworkError>{
        if bytes.starts_with(b"IQ"){let(_,stream,_)=crate::component_wire::decode_component_ack(bytes)?;let p=self.peer_mut(id)?;if stream!=p.stream{return Err(crate::ComponentError::State(crate::SyncError::WrongStream).into());}return p.components.as_mut().ok_or(NetworkError::InvalidConfig)?.acknowledge(bytes);}
        if bytes.starts_with(b"IA"){let(stream,_)=crate::sync_wire::decode_ack(bytes).map_err(CombatSyncError::from)?;let p=self.peer_mut(id)?;if stream!=p.stream{return Err(CombatSyncError::from(crate::SyncError::WrongStream).into());}return p.motion.as_mut().ok_or(NetworkError::InvalidConfig)?.acknowledge(bytes);}
        let(stream,sequence)=crate::combat_sync_wire::decode_ack(bytes).map_err(CombatSyncError::from)?;let p=self.peer_mut(id)?;
        if stream!=p.stream{return Err(CombatSyncError::from(crate::SyncError::WrongStream).into());}
        if let Some(window)=&mut p.window{let result=window.acknowledge(sequence)?;if result==AckDisposition::Committed{p.dirty=true;}return Ok(result);}
        if sequence==p.confirmed&&p.confirmed!=0{return Ok(AckDisposition::Duplicate);}
        if !p.sent_once||p.sender.pending_header().is_none_or(|h|h.sequence!=sequence){return Err(CombatSyncError::from(crate::SyncError::OutOfOrder).into());}
        let ack:[u8;20]=bytes.try_into().unwrap();
        if p.offset>0&&p.offset<p.total(){p.deferred_ack=Some(ack);Ok(AckDisposition::DeferredUntilFrameEnd)}
        else{p.commit_ack(&ack);Ok(AckDisposition::Committed)}
    }
    /// Callback must be nonblocking and report exactly the accepted byte count.
    /// Payload is framed by a four-byte LE length. No assumption that a write is
    /// a packet. A partial retry must finish even if its ACK arrives midway.
    pub fn poll(&mut self,now:u64,budget:&mut NetworkBudget,notices:&mut Vec<NetworkNotice>,mut write:impl FnMut(ConnectionId,&[u8])->WriteProgress)->Result<NetworkReport,NetworkError>{
        self.poll_channels(now,None,None,budget,notices,&mut write)
    }
    /// Supply the latest shared movement frame. Movement state may coalesce
    /// while ACK is pending; ordered combat events remain on their own channel.
    /// Channel turns alternate at complete frame boundaries, never within one.
    pub fn poll_with_motion(&mut self,now:u64,frame:&crate::SpatialFrame,budget:&mut NetworkBudget,notices:&mut Vec<NetworkNotice>,mut write:impl FnMut(ConnectionId,&[u8])->WriteProgress)->Result<NetworkReport,NetworkError>{
        self.poll_channels(now,Some(frame),None,budget,notices,&mut write)
    }
    /// Shares the same byte/work budget and writer across IC, IS and IP. A
    /// started frame keeps exclusive ownership until its final byte, including
    /// when an ACK arrives during a retransmission. Unstarted ready channels
    /// rotate, including an encode that discovers no changed snapshot.
    pub fn poll_with_components(&mut self,now:u64,frame:&crate::ComponentFrame,budget:&mut NetworkBudget,notices:&mut Vec<NetworkNotice>,mut write:impl FnMut(ConnectionId,&[u8])->WriteProgress)->Result<NetworkReport,NetworkError>{
        self.poll_channels(now,Some(frame.motion_frame()),Some(frame),budget,notices,&mut write)
    }
    fn poll_channels(&mut self,now:u64,frame:Option<&crate::SpatialFrame>,components:Option<&crate::ComponentFrame>,budget:&mut NetworkBudget,notices:&mut Vec<NetworkNotice>,write:&mut impl FnMut(ConnectionId,&[u8])->WriteProgress)->Result<NetworkReport,NetworkError>{
        if now<self.now{return Err(NetworkError::TimeReversed);}if budget.work>MAX_NETWORK_WORK{return Err(NetworkError::InvalidBudget);}self.now=now;notices.clear();let mut r=NetworkReport::default();
        // One tour at most: repeated host polls share the same budget. The
        // persistent cursor prevents small budgets from restarting at slot zero.
        let visits=budget.work.min(self.slots.len());
        for _ in 0..visits{
            if r.bytes>0&&budget.bytes==0{break;}
            let index=self.cursor%self.slots.len();self.cursor=(index+1)%self.slots.len();budget.work-=1;r.visited+=1;
            let slot=&mut self.slots[index];let id=ConnectionId{index:index as u32,generation:slot.generation};let Some(p)=slot.peer.as_mut()else{continue;};if !p.attached{continue;}
            let combat_partial=p.window.as_ref().map_or_else(||p.offset>0&&p.offset<p.total(),|w|w.offset()>0);
            let motion_timeout=p.motion.as_ref().and_then(|m|m.timeout(now,self.config));
            let combat_timeout=p.window.as_ref().map_or_else(||p.first_sent.is_some_and(|t|now.saturating_sub(t)>=self.config.ack_timeout_ms),|w|w.timed_out(now,self.config.ack_timeout_ms));
            let component_timeout=p.components.as_ref().and_then(|c|c.timeout(now,self.config));
            let channel_timeout=component_timeout.or(motion_timeout).or_else(||combat_timeout.then_some(NetworkNoticeKind::AckTimeout)).or_else(||p.blocked_since.is_some_and(|t|now.saturating_sub(t)>=self.config.blocked_timeout_ms).then_some(NetworkNoticeKind::BlockedTimeout));
            if let Some(kind)=channel_timeout{p.detach();r.detached+=1;notices.push(NetworkNotice{connection:id,kind});continue;}
            let combat_ready=p.window.as_ref().map_or_else(||if p.sender.pending_packet().is_some(){p.offset<p.total()||p.last_sent.is_none_or(|t|now.saturating_sub(t)>=self.config.retry_ms)}else{p.observed&&p.dirty},|w|w.ready(now,self.config.retry_ms)||(!w.is_full()&&p.observed&&(p.dirty||p.sender.queued_events()>0)));
            let selected=if p.components.is_some(){
                let motion_partial=p.motion.as_ref().is_some_and(MotionPeer::partial);let component_partial=p.components.as_ref().is_some_and(ComponentPeer::partial);
                let ready=[combat_ready,p.motion.as_ref().is_some_and(|m|m.ready(frame,now,self.config.retry_ms)),p.components.as_ref().is_some_and(|c|c.ready(components,now,self.config.retry_ms))];
                if combat_partial{Some(0)}else if motion_partial{Some(1)}else if component_partial{Some(2)}else{(0..3).map(|n|(p.next_channel+n)%3).find(|&channel|ready[channel as usize])}
            }else{None};
            if p.components.is_some()&&selected.is_none(){continue;}
            if selected==Some(2){if let Some(kind)=p.components.as_mut().unwrap().poll(components,now,self.config,budget,&mut r,|part|write(id,part)){p.detach();r.detached+=1;notices.push(NetworkNotice{connection:id,kind});}if !p.components.as_ref().unwrap().partial(){p.next_channel=0;}continue;}
            let motion_turn=if p.components.is_some(){selected==Some(1)}else{!combat_partial&&p.motion.as_ref().is_some_and(|m|m.partial()||(m.ready(frame,now,self.config.retry_ms)&&(p.prefer_motion||!combat_ready)))};
            if motion_turn{let before=r.packets;if let Some(kind)=p.motion.as_mut().unwrap().poll(frame,now,self.config,budget,&mut r,|part|write(id,part)){p.detach();r.detached+=1;notices.push(NetworkNotice{connection:id,kind});}if r.packets>before{p.prefer_motion=false;}if !p.motion.as_ref().unwrap().partial(){p.next_channel=2;}continue;}
            p.next_channel=1;
            if p.window.is_some(){let before=r.packets;poll_window(p,id,now,self.config,budget,&mut r,notices,write);if r.packets>before{p.prefer_motion=true;}continue;}
            let reason=if p.first_sent.is_some_and(|t|now.saturating_sub(t)>=self.config.ack_timeout_ms){Some(NetworkNoticeKind::AckTimeout)}
                else if p.blocked_since.is_some_and(|t|now.saturating_sub(t)>=self.config.blocked_timeout_ms){Some(NetworkNoticeKind::BlockedTimeout)}else{None};
            if let Some(kind)=reason{p.detach();r.detached+=1;notices.push(NetworkNotice{connection:id,kind});continue;}
            if budget.bytes==0{continue;}
            if p.sender.pending_packet().is_none(){
                if !p.observed||!p.dirty{continue;}r.prepared+=1;
                match p.sender.prepare(){Ok(Some(_))=>{p.dirty=false;p.reset_write();},Ok(None)=>{p.dirty=false;continue;},Err(error)=>{p.detach();r.detached+=1;notices.push(NetworkNotice{connection:id,kind:NetworkNoticeKind::EncodeFailed(error)});continue;}}
            }
            let total=p.total();if p.offset==total{
                if p.last_sent.is_some_and(|t|now.saturating_sub(t)<self.config.retry_ms){r.waiting+=1;continue;}
                p.offset=0;r.retries+=1;
            }
            let packet=p.sender.pending_packet().unwrap();let prefix=(packet.len()as u32).to_le_bytes();
            let part=if p.offset<4{&prefix[p.offset..]}else{&packet[p.offset-4..]};let part=&part[..part.len().min(self.config.quantum_bytes).min(budget.bytes)];
            match write(id,part){
                WriteProgress::Written(n)if n>0&&n<=part.len()=>{
                    budget.bytes-=n;r.bytes+=n;r.writes+=1;p.offset+=n;p.blocked_since=None;
                    if p.offset==total{r.packets+=1;p.prefer_motion=true;p.sent_once=true;p.first_sent.get_or_insert(now);p.last_sent=Some(now);if let Some(ack)=p.deferred_ack{p.commit_ack(&ack);}}
                },
                WriteProgress::WouldBlock|WriteProgress::Written(0)=>{r.blocked+=1;p.blocked_since.get_or_insert(now);},
                result=>{p.detach();r.detached+=1;let kind=if result==WriteProgress::Disconnected{NetworkNoticeKind::Disconnected}else{NetworkNoticeKind::InvalidWriter};notices.push(NetworkNotice{connection:id,kind});},
            }
        }Ok(r)
    }
}

fn poll_window(p:&mut Peer,id:ConnectionId,now:u64,config:NetworkConfig,budget:&mut NetworkBudget,r:&mut NetworkReport,notices:&mut Vec<NetworkNotice>,write:&mut impl FnMut(ConnectionId,&[u8])->WriteProgress){
    let window=p.window.as_mut().unwrap();let reason=if window.timed_out(now,config.ack_timeout_ms){Some(NetworkNoticeKind::AckTimeout)}else if p.blocked_since.is_some_and(|t|now.saturating_sub(t)>=config.blocked_timeout_ms){Some(NetworkNoticeKind::BlockedTimeout)}else{None};
    if let Some(kind)=reason{p.detach();r.detached+=1;notices.push(NetworkNotice{connection:id,kind});return;}if budget.bytes==0{return;}
    if p.observed&&(p.dirty||p.sender.queued_events()>0)&&!window.is_full(){r.prepared+=1;match window.stage(&mut p.sender){Ok(_)=>p.dirty=false,Err(error)=>{p.detach();r.detached+=1;notices.push(NetworkNotice{connection:id,kind:NetworkNoticeKind::EncodeFailed(error)});return;}}}
    r.retries+=usize::from(window.select(now,config.retry_ms));let Some(packet)=window.packet()else{r.waiting+=usize::from(window.len()>0);return;};let offset=window.offset();let prefix=(packet.len()as u32).to_le_bytes();let part=if offset<4{&prefix[offset..]}else{&packet[offset-4..]};let part=&part[..part.len().min(config.quantum_bytes).min(budget.bytes)];
    match write(id,part){WriteProgress::Written(n)if n>0&&n<=part.len()=>{budget.bytes-=n;r.bytes+=n;r.writes+=1;p.blocked_since=None;r.packets+=usize::from(window.advance(n,now));},WriteProgress::WouldBlock|WriteProgress::Written(0)=>{r.blocked+=1;p.blocked_since.get_or_insert(now);},result=>{p.detach();r.detached+=1;notices.push(NetworkNotice{connection:id,kind:if result==WriteProgress::Disconnected{NetworkNoticeKind::Disconnected}else{NetworkNoticeKind::InvalidWriter}});}}
}
