//! Bounded ACK-based lossless movement-state compression. This transports the
//! exact NM02 state; it does not quantize positions or omit route progress.
use std::{collections::VecDeque,sync::Arc};
use crate::wire::{MAX_STATE,STATE_HEADER};
pub const HISTORY:usize=128;
const HEADER:usize=40;
fn q(b:&[u8],at:usize)->u64{u64::from_le_bytes(b[at..at+8].try_into().unwrap())}
fn d(b:&[u8],at:usize)->u32{u32::from_le_bytes(b[at..at+4].try_into().unwrap())}
fn identity(b:&[u8])->Result<(u64,u64),&'static str>{
    if b.len()<STATE_HEADER||b.len()>MAX_STATE||b.len()%4!=0||&b[..8]!=b"NM02\x81\0\0\0"||q(b,8)==0{return Err("full state shape");}Ok((q(b,8),q(b,16)))
}
#[derive(Default)]
pub struct StateHistory{states:VecDeque<Arc<[u8]>>}
impl StateHistory{
    pub fn clear(&mut self){self.states.clear();}
    pub fn retained_bytes(&self)->usize{self.states.iter().map(|s|s.len()).sum()}
    pub fn len(&self)->usize{self.states.len()}
    pub fn is_empty(&self)->bool{self.states.is_empty()}
    fn find(&self,epoch:u64,tick:u64)->Option<&Arc<[u8]>>{self.states.iter().rev().find(|s|q(s,8)==epoch&&q(s,16)==tick)}
    /// Commit only after a successful send or validated replica application.
    pub fn commit(&mut self,state:Arc<[u8]>)->Result<(),&'static str>{
        let(epoch,tick)=identity(&state)?;
        if let Some(last)=self.states.back(){if epoch!=q(last,8)||tick<q(last,16){return Err("state history order");}if tick==q(last,16){return if last.as_ref()==state.as_ref(){Ok(())}else{Err("state history conflict")};}}
        if self.states.len()==HISTORY{self.states.pop_front();}self.states.push_back(state);Ok(())
    }
    pub fn expand(&self,packet:&[u8])->Result<Arc<[u8]>,&'static str>{
        if packet.get(4)==Some(&129){identity(packet)?;return Ok(packet.into());}
        if packet.len()<HEADER||packet.len()>MAX_STATE||&packet[..8]!=b"NM02\x83\0\0\0"{return Err("delta header");}
        let(epoch,base_tick,tick)=(q(packet,8),q(packet,16),q(packet,24));
        let len=d(packet,32)as usize;let words=u16::from_le_bytes(packet[36..38].try_into().unwrap())as usize;
        let mask_len=u16::from_le_bytes(packet[38..40].try_into().unwrap())as usize;
        if len<STATE_HEADER||len>MAX_STATE||len%4!=0||words!=len/4||mask_len!=(words+7)/8||packet.len()<HEADER+mask_len||tick<=base_tick{return Err("delta shape");}
        let mask=&packet[HEADER..HEADER+mask_len];if words%8!=0&&mask[mask_len-1]>>(words%8)!=0{return Err("delta mask padding");}
        let changed:usize=mask.iter().map(|v|v.count_ones()as usize).sum();if packet.len()!=HEADER+mask_len+changed*4{return Err("delta body length");}
        let base=self.find(epoch,base_tick).ok_or("delta baseline missing")?;let mut full=vec![0;len];let copy=len.min(base.len());full[..copy].copy_from_slice(&base[..copy]);let mut at=HEADER+mask_len;
        for i in 0..words{if mask[i/8]&(1<<(i%8))!=0{full[i*4..i*4+4].copy_from_slice(&packet[at..at+4]);at+=4;}else if i*4>=base.len(){return Err("delta extension missing");}}
        if identity(&full)?!=(epoch,tick){return Err("delta identity");}Ok(full.into())
    }
}
#[derive(Default)]
pub struct StateEncoder{history:StateHistory,ack:Option<(u64,u64)>}
impl StateEncoder{
    pub fn retained_bytes(&self)->usize{self.history.retained_bytes()}
    pub fn acknowledge(&mut self,epoch:u64,tick:u64)->Result<(),&'static str>{
        if self.ack==Some((epoch,tick)){return Ok(());}
        if self.ack.is_some_and(|(e,t)|e!=epoch||tick<t)||self.history.find(epoch,tick).is_none(){return Err("delta ACK not retained/sent");}
        self.ack=Some((epoch,tick));Ok(())
    }
    pub fn commit_sent(&mut self,state:Arc<[u8]>)->Result<(),&'static str>{self.history.commit(state)}
    /// An evicted/unconfirmed baseline always falls back to a standalone state.
    pub fn encode(&self,full:&[u8])->Result<Vec<u8>,&'static str>{
        let(epoch,tick)=identity(full)?;
        let Some((ack_epoch,base_tick))=self.ack else{return Ok(full.to_vec());};
        let Some(base)=self.history.find(ack_epoch,base_tick)else{return Ok(full.to_vec());};
        if ack_epoch!=epoch||base_tick>=tick{return Ok(full.to_vec());}
        let words=full.len()/4;let mask_len=(words+7)/8;let mut out=vec![0;HEADER+mask_len];out[..8].copy_from_slice(b"NM02\x83\0\0\0");
        out[8..16].copy_from_slice(&epoch.to_le_bytes());out[16..24].copy_from_slice(&base_tick.to_le_bytes());out[24..32].copy_from_slice(&tick.to_le_bytes());out[32..36].copy_from_slice(&(full.len()as u32).to_le_bytes());out[36..38].copy_from_slice(&(words as u16).to_le_bytes());out[38..40].copy_from_slice(&(mask_len as u16).to_le_bytes());
        for (i,word)in full.chunks_exact(4).enumerate(){if base.get(i*4..i*4+4)!=Some(word){out[HEADER+i/8]|=1<<(i%8);out.extend_from_slice(word);}}
        Ok(if out.len()<full.len(){out}else{full.to_vec()})
    }
}
