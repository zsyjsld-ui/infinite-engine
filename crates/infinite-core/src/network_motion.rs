//! Latest-state movement channel. Frames share the combat connection writer,
//! but a partially written frame always retains exclusive ownership.
use crate::{AckDisposition,CombatSyncError,NetworkError,NetworkConfig,NetworkBudget,NetworkReport,NetworkNoticeKind,SpatialFrame,SyncSender,SyncError,Tick,ViewRect,WriteProgress};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct MotionConnectionStatus {pub pending_bytes:usize,pub written_bytes:usize,pub waiting_ack:bool,pub last_prepared_tick:Option<Tick>}
pub(crate) struct MotionPeer {
 pub(crate) sender:SyncSender,pub(crate) view:ViewRect,view_dirty:bool,
 offset:usize,sent:bool,first:Option<u64>,last:Option<u64>,blocked:Option<u64>,
 confirmed:u64,deferred:Option<[u8;20]>,prepared:Option<Tick>,
}
impl MotionPeer {
 pub(crate) fn new(stream:u64,view:ViewRect)->Self{Self{sender:SyncSender::new(stream),view,view_dirty:true,offset:0,sent:false,first:None,last:None,blocked:None,confirmed:0,deferred:None,prepared:None}}
 pub(crate) fn set_view(&mut self,view:ViewRect){self.view_dirty|=self.view!=view;self.view=view;}
 fn total(&self)->usize{self.sender.pending_packet().map_or(0,|p|p.len()+4)}
 pub(crate) fn partial(&self)->bool{self.offset>0&&self.offset<self.total()}
 pub(crate) fn status(&self)->MotionConnectionStatus{MotionConnectionStatus{pending_bytes:self.total(),written_bytes:self.offset,waiting_ack:self.sent,last_prepared_tick:self.prepared}}
 pub(crate) fn ready(&self,frame:Option<&SpatialFrame>,now:u64,retry:u64)->bool{
  if self.sender.pending_packet().is_some(){return self.offset<self.total()||self.last.is_none_or(|t|now.saturating_sub(t)>=retry);}
  frame.is_some_and(|f|f.tick().is_some_and(|tick|self.view_dirty||self.prepared.is_none_or(|old|tick!=old)))
 }
 fn reset_write(&mut self){self.offset=0;self.sent=false;self.first=None;self.last=None;self.blocked=None;self.deferred=None;}
 fn commit(&mut self,ack:&[u8;20]){self.sender.acknowledge_packet(ack).expect("validated motion ACK");self.confirmed=u64::from_le_bytes(ack[12..].try_into().unwrap());self.reset_write();}
 pub(crate) fn acknowledge(&mut self,bytes:&[u8])->Result<AckDisposition,NetworkError>{
  let(stream,sequence)=crate::sync_wire::decode_ack(bytes).map_err(CombatSyncError::from)?;
  let expected=self.sender.pending_header().map(|h|h.stream);
  // The outer peer authenticates stream even when no packet is pending.
  if expected.is_some_and(|s|s!=stream){return Err(CombatSyncError::from(SyncError::WrongStream).into());}
  if sequence!=0&&sequence==self.confirmed{return Ok(AckDisposition::Duplicate);}
  if !self.sent||self.sender.pending_header().is_none_or(|h|h.sequence!=sequence){return Err(CombatSyncError::from(SyncError::OutOfOrder).into());}
  let ack=bytes.try_into().unwrap();if self.partial(){self.deferred=Some(ack);Ok(AckDisposition::DeferredUntilFrameEnd)}else{self.commit(&ack);Ok(AckDisposition::Committed)}
 }
 pub(crate) fn detach(&mut self){self.offset=0;self.blocked=None;if let Some(ack)=self.deferred{self.commit(&ack);}}
 pub(crate) fn resume(&mut self){self.offset=0;self.first=None;self.last=None;self.blocked=None;}
 pub(crate) fn timeout(&self,now:u64,c:NetworkConfig)->Option<NetworkNoticeKind>{if self.first.is_some_and(|t|now.saturating_sub(t)>=c.ack_timeout_ms){Some(NetworkNoticeKind::AckTimeout)}else if self.blocked.is_some_and(|t|now.saturating_sub(t)>=c.blocked_timeout_ms){Some(NetworkNoticeKind::BlockedTimeout)}else{None}}
 pub(crate) fn poll(&mut self,frame:Option<&SpatialFrame>,now:u64,c:NetworkConfig,b:&mut NetworkBudget,r:&mut NetworkReport,mut write:impl FnMut(&[u8])->WriteProgress)->Option<NetworkNoticeKind>{
  if b.bytes==0{return None;}
  if self.sender.pending_packet().is_none(){let frame=frame?;r.prepared+=1;match self.sender.prepare(frame,self.view){Ok(packet)=>{self.prepared=frame.tick();self.view_dirty=false;if packet.is_none(){return None;}self.reset_write();},Err(e)=>return Some(NetworkNoticeKind::MotionEncodeFailed(e))}}
  let total=self.total();if self.offset==total{if self.last.is_some_and(|t|now.saturating_sub(t)<c.retry_ms){r.waiting+=1;return None;}self.offset=0;r.retries+=1;}
  let packet=self.sender.pending_packet().unwrap();let prefix=(packet.len()as u32).to_le_bytes();let part=if self.offset<4{&prefix[self.offset..]}else{&packet[self.offset-4..]};let part=&part[..part.len().min(c.quantum_bytes).min(b.bytes)];
  match write(part){WriteProgress::Written(n)if n>0&&n<=part.len()=>{b.bytes-=n;r.bytes+=n;r.writes+=1;self.offset+=n;self.blocked=None;if self.offset==total{r.packets+=1;self.sent=true;self.first.get_or_insert(now);self.last=Some(now);if let Some(ack)=self.deferred{self.commit(&ack);}}},WriteProgress::WouldBlock|WriteProgress::Written(0)=>{r.blocked+=1;self.blocked.get_or_insert(now);},WriteProgress::Disconnected=>return Some(NetworkNoticeKind::Disconnected),_=>return Some(NetworkNoticeKind::InvalidWriter)}None
 }
}
