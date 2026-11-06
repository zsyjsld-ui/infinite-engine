//! Simulation-owned spatial capture and existing fair movement scheduler.
//! The socket boundary is a bounded queue of complete messages, never World.
use std::{collections::HashMap,num::NonZeroU32};
use infinite_core::{CombatConnections,ConnectionId,NetworkConfig,NetworkBudget,NetworkError,NetworkNotice,NetworkReport,SpatialFrame,ViewRect,World,WriteProgress,MAX_SYNC_PACKET_BYTES};
use tokio::sync::mpsc;
struct Writer{tx:mpsc::Sender<Vec<u8>>,prefix:Vec<u8>,body:Vec<u8>,length:Option<usize>}
impl Writer{
 fn write(&mut self,part:&[u8])->WriteProgress{
  if self.tx.is_closed(){return WriteProgress::Disconnected;}
  if self.length.is_none(){if self.prefix.len()+part.len()>4{return WriteProgress::Disconnected;}self.prefix.extend_from_slice(part);if self.prefix.len()==4{let len=u32::from_le_bytes(self.prefix.as_slice().try_into().unwrap())as usize;if len>MAX_SYNC_PACKET_BYTES||len<40{return WriteProgress::Disconnected;}self.length=Some(len);self.body.clear();}return WriteProgress::Written(part.len());}
  let len=self.length.unwrap();if self.body.len()+part.len()>len{return WriteProgress::Disconnected;}
  if self.body.len()+part.len()==len{
   // Reserve before accepting the final chunk: a full queue cannot falsely
   // advance the scheduler, commit a packet or interleave the next frame.
   let permit=match self.tx.try_reserve(){Ok(p)=>p,Err(mpsc::error::TrySendError::Full(_))=>return WriteProgress::WouldBlock,Err(_)=>return WriteProgress::Disconnected};
   self.body.extend_from_slice(part);permit.send(std::mem::take(&mut self.body));self.prefix.clear();self.length=None;
  }else{self.body.extend_from_slice(part);}
  WriteProgress::Written(part.len())
 }
}
pub struct Visibility{frame:SpatialFrame,net:CombatConnections,writers:HashMap<ConnectionId,Writer>,notices:Vec<NetworkNotice>}
impl Visibility{
 pub fn new(first_stream:u64,connections:usize)->Result<Self,NetworkError>{Ok(Self{frame:SpatialFrame::new(NonZeroU32::new(4096).unwrap()),net:CombatConnections::new(first_stream,NetworkConfig{connections,quantum_bytes:16384,..NetworkConfig::default()})?,writers:HashMap::new(),notices:Vec::new()})}
 pub fn attach(&mut self,view:ViewRect,tx:mpsc::Sender<Vec<u8>>)->Result<(ConnectionId,u64),NetworkError>{let id=self.net.connect()?;self.net.set_motion_view(id,view)?;let stream=self.net.status(id)?.stream;self.writers.insert(id,Writer{tx,prefix:Vec::with_capacity(4),body:Vec::new(),length:None});Ok((id,stream))}
 pub fn remove(&mut self,id:ConnectionId)->Result<(),NetworkError>{self.net.remove(id)?;self.writers.remove(&id);Ok(())}
 pub fn view(&mut self,id:ConnectionId,view:ViewRect)->Result<(),NetworkError>{self.net.set_motion_view(id,view)}
 pub fn acknowledge(&mut self,id:ConnectionId,ack:&[u8])->Result<(),NetworkError>{self.net.acknowledge(id,ack).map(|_|())}
 pub fn capture(&mut self,world:&World)->Result<(),infinite_core::SyncError>{self.frame.capture(world)}
 pub fn poll(&mut self,now_ms:u64,budget:&mut NetworkBudget)->Result<NetworkReport,NetworkError>{
  let writers=&mut self.writers;self.net.poll_with_motion(now_ms,&self.frame,budget,&mut self.notices,|id,part|writers.get_mut(&id).map_or(WriteProgress::Disconnected,|w|w.write(part)))
 }
 pub fn notices(&self)->&[NetworkNotice]{&self.notices}
 /// Spatial and sender cache capacity plus in-progress writer buffers. Queued
 /// complete packets, receiver replicas and allocator overhead are separate.
 pub fn retained_payload_capacity_bytes(&self)->usize{self.frame.retained_payload_capacity_bytes()+self.net.motion_retained_payload_capacity_bytes()+self.writers.values().map(|w|w.prefix.capacity()+w.body.capacity()).sum::<usize>()}
}
