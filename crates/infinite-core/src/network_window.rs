//! IC v5: bounded pipelined deltas with a cumulative ACK frontier.
use std::collections::VecDeque;
use crate::{AckDisposition,CombatSyncSender,NetworkError,CombatSyncError,SyncError};
pub const MAX_COMBAT_WINDOW_PACKETS:usize=16;
pub struct RetainedCombatPacket {pub sequence:u64,pub events:usize,pub bytes:Vec<u8>}
/// Ownership transfer includes every unacknowledged wire packet. The sender's
/// internal baseline may already include these packets; it is not an ACK proof.
pub struct RetainedCombatSender {pub sender:CombatSyncSender,pub packets:Vec<RetainedCombatPacket>,pub motion:Option<crate::SyncSender>,pub components:Option<crate::ComponentSender>}
impl RetainedCombatSender {
    pub fn queued_events(&self)->usize{self.sender.queued_events()+self.packets.iter().map(|p|p.events).sum::<usize>()}
    pub fn pending_packet(&self)->Option<&[u8]>{self.sender.pending_packet().or_else(||self.packets.first().map(|p|p.bytes.as_slice()))}
}
struct Flight {packet:RetainedCombatPacket,ever_sent:bool,resend:bool,first_sent:Option<u64>,last_sent:Option<u64>}
pub(crate) struct SendWindow {capacity:usize,packets:VecDeque<Flight>,writing:Option<u64>,offset:usize,confirmed:u64,highest_sent:u64,deferred:Option<u64>,events:usize}
impl SendWindow {
    pub(crate) fn new(capacity:usize)->Self{Self{capacity,packets:VecDeque::new(),writing:None,offset:0,confirmed:0,highest_sent:0,deferred:None,events:0}}
    pub(crate) fn events(&self)->usize{self.events}
    pub(crate) fn len(&self)->usize{self.packets.len()}
    pub(crate) fn bytes(&self)->usize{self.packets.iter().map(|p|p.packet.bytes.len()+4).sum()}
    pub(crate) fn offset(&self)->usize{self.offset}
    pub(crate) fn waiting(&self)->bool{self.packets.iter().any(|p|p.ever_sent)}
    pub(crate) fn is_full(&self)->bool{self.packets.len()==self.capacity}
    pub(crate) fn ready(&self,now:u64,retry:u64)->bool{self.writing.is_some()||self.packets.iter().any(|p|!p.ever_sent||p.resend||p.last_sent.is_some_and(|t|now.saturating_sub(t)>=retry))}
    pub(crate) fn timed_out(&self,now:u64,limit:u64)->bool{self.packets.front().is_some_and(|p|p.first_sent.is_some_and(|t|now.saturating_sub(t)>=limit))}
    pub(crate) fn stage(&mut self,sender:&mut CombatSyncSender)->Result<bool,CombatSyncError>{
        if self.is_full(){return Ok(false);}if sender.prepare()?.is_none(){return Ok(false);}
        let header=sender.pending_header().unwrap();let events=sender.pending_event_count();let mut bytes=sender.pending_packet().unwrap().to_vec();bytes[2]=5;
        self.packets.push_back(Flight{packet:RetainedCombatPacket{sequence:header.sequence,events,bytes},ever_sent:false,resend:false,first_sent:None,last_sent:None});self.events+=events;sender.stage_inflight();Ok(true)
    }
    fn commit(&mut self,sequence:u64){while self.packets.front().is_some_and(|p|p.packet.sequence<=sequence){self.events-=self.packets.pop_front().unwrap().packet.events;}self.confirmed=sequence;}
    pub(crate) fn acknowledge(&mut self,sequence:u64)->Result<AckDisposition,NetworkError>{
        if sequence!=0&&sequence<=self.confirmed{return Ok(AckDisposition::Duplicate);}
        if sequence==0||sequence>self.highest_sent{return Err(NetworkError::Sync(CombatSyncError::State(SyncError::OutOfOrder)));}
        if self.writing.is_some_and(|s|s<=sequence)&&self.offset>0{self.deferred=Some(self.deferred.map_or(sequence,|old|old.max(sequence)));return Ok(AckDisposition::DeferredUntilFrameEnd);}
        if self.writing.is_some_and(|s|s<=sequence){self.writing=None;self.offset=0;}
        self.commit(sequence);Ok(AckDisposition::Committed)
    }
    // Return true when starting an actual retransmission, never for a first send.
    pub(crate) fn select(&mut self,now:u64,retry:u64)->bool{
        if self.writing.is_some(){return false;}
        let index=self.packets.iter().position(|p|p.resend||p.last_sent.is_some_and(|t|now.saturating_sub(t)>=retry)).or_else(||self.packets.iter().position(|p|!p.ever_sent));
        if let Some(index)=index{let p=&self.packets[index];self.writing=Some(p.packet.sequence);self.offset=0;p.ever_sent}else{false}
    }
    pub(crate) fn packet(&self)->Option<&[u8]>{let seq=self.writing?;Some(&self.packets.iter().find(|p|p.packet.sequence==seq).unwrap().packet.bytes)}
    pub(crate) fn advance(&mut self,bytes:usize,now:u64)->bool{
        self.offset+=bytes;let seq=self.writing.unwrap();let p=self.packets.iter_mut().find(|p|p.packet.sequence==seq).unwrap();if self.offset<p.packet.bytes.len()+4{return false;}
        assert_eq!(self.offset,p.packet.bytes.len()+4);p.ever_sent=true;p.resend=false;p.first_sent.get_or_insert(now);p.last_sent=Some(now);self.highest_sent=self.highest_sent.max(seq);self.writing=None;self.offset=0;
        if let Some(ack)=self.deferred.take(){self.commit(ack);}true
    }
    pub(crate) fn detach(&mut self){self.writing=None;self.offset=0;if let Some(ack)=self.deferred.take(){self.commit(ack);}for p in &mut self.packets{p.resend=p.ever_sent;}}
    pub(crate) fn resume(&mut self){for p in &mut self.packets{p.first_sent=None;p.last_sent=None;p.resend=p.ever_sent;}}
    pub(crate) fn into_packets(self)->Vec<RetainedCombatPacket>{self.packets.into_iter().map(|f|f.packet).collect()}
    pub(crate) fn capacity_bytes(&self)->usize{self.packets.capacity()*std::mem::size_of::<Flight>()+self.packets.iter().map(|p|p.packet.bytes.capacity()).sum::<usize>()}
}
