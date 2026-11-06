use crate::{MotionState, SyncChange, SyncHeader, SyncError, MovementStatus, GridPosition, EntityId, Tick, MAX_VISIBLE_ENTITIES};
use crate::sync::key;
pub const SYNC_HEADER_BYTES: usize = 40;
pub const MAX_SYNC_PACKET_BYTES: usize = SYNC_HEADER_BYTES + MAX_VISIBLE_ENTITIES * 27;
const X:u8=1;const Y:u8=2;const STATUS:u8=4;const ENTER:u8=8;const LEAVE:u8=16;const DX:u8=32;const DY:u8=64;

pub(crate) fn status_code(status:MovementStatus)->u8 {
    match status {MovementStatus::Idle=>0,MovementStatus::Moving=>1,MovementStatus::Arrived=>2,MovementStatus::Stopped=>3,MovementStatus::Blocked=>4}
}
pub(crate) fn encode(h:SyncHeader,changes:&[SyncChange],out:&mut Vec<u8>) {
    out.clear();out.extend_from_slice(b"IS");out.push(1);out.push(u8::from(h.reset));
    for n in [h.stream,h.sequence,h.base,h.tick.0] {out.extend_from_slice(&n.to_le_bytes());}
    out.extend_from_slice(&(changes.len() as u16).to_le_bytes());out.extend_from_slice(&[0,0]);
    for &change in changes {
        let id=change.entity();out.extend_from_slice(&id.index.to_le_bytes());out.extend_from_slice(&id.generation.to_le_bytes());
        match change {
            SyncChange::Enter(s)=>{out.push(ENTER|X|Y|STATUS);out.extend_from_slice(&s.position.x.to_le_bytes());out.extend_from_slice(&s.position.y.to_le_bytes());out.push(status_code(s.movement));},
            SyncChange::Leave(_)=>out.push(LEAVE),
            SyncChange::Update{x,y,movement,..}=>{
                let mut mask=0;if x.is_some(){mask|=X;}if y.is_some(){mask|=Y;}if movement.is_some(){mask|=STATUS;}
                if x.is_some_and(|c|c.delta.is_some()){mask|=DX;}if y.is_some_and(|c|c.delta.is_some()){mask|=DY;}out.push(mask);
                for axis in [x,y].into_iter().flatten(){if let Some(delta)=axis.delta{out.extend_from_slice(&delta.to_le_bytes());}else{out.extend_from_slice(&axis.value.to_le_bytes());}}
                if let Some(status)=movement{out.push(status_code(status));}
            },
        }
    }
}
struct Reader<'a>{bytes:&'a[u8],at:usize}
impl<'a> Reader<'a>{
    fn take<const N:usize>(&mut self)->Result<[u8;N],SyncError>{let s=self.bytes.get(self.at..self.at+N).ok_or(SyncError::Malformed)?;self.at+=N;Ok(s.try_into().unwrap())}
    fn byte(&mut self)->Result<u8,SyncError>{Ok(self.take::<1>()?[0])}
    fn u64(&mut self)->Result<u64,SyncError>{Ok(u64::from_le_bytes(self.take()?))}
    fn status(&mut self)->Result<MovementStatus,SyncError>{match self.byte()?{0=>Ok(MovementStatus::Idle),1=>Ok(MovementStatus::Moving),2=>Ok(MovementStatus::Arrived),3=>Ok(MovementStatus::Stopped),4=>Ok(MovementStatus::Blocked),_=>Err(SyncError::Malformed)}}
    fn coordinate(&mut self,old:i32,delta:bool)->Result<i32,SyncError>{if delta{old.checked_add(i32::from(i16::from_le_bytes(self.take()?))).ok_or(SyncError::Malformed)}else{Ok(i32::from_le_bytes(self.take()?))}}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ApplyReport{pub stream:u64,pub sequence:u64,pub tick:Tick,pub duplicate:bool}
impl ApplyReport {
    /// Fixed 20-byte application acknowledgement; transport framing is separate.
    pub fn acknowledgement(self) -> [u8;20] {
        let mut out=[0u8;20];out[..4].copy_from_slice(b"IA\x01\x00");
        out[4..12].copy_from_slice(&self.stream.to_le_bytes());out[12..20].copy_from_slice(&self.sequence.to_le_bytes());out
    }
}
pub(crate) fn decode_ack(bytes:&[u8])->Result<(u64,u64),SyncError>{
    if bytes.len()!=20||&bytes[..4]!=b"IA\x01\x00"{return Err(SyncError::Malformed);}
    Ok((u64::from_le_bytes(bytes[4..12].try_into().unwrap()),u64::from_le_bytes(bytes[12..20].try_into().unwrap())))
}
/// Motion replica bound to one authenticated subscription. A fresh stream id
/// and new Replica are required when switching worlds/connections.
pub struct Replica{stream:u64,sequence:u64,tick:Tick,states:Vec<MotionState>,scratch:Vec<MotionState>,last_packet:Vec<u8>}
impl Replica{
    pub fn new(stream:u64)->Self{Self{stream,sequence:0,tick:Tick(0),states:Vec::new(),scratch:Vec::new(),last_packet:Vec::new()}}
    pub fn states(&self)->&[MotionState]{&self.states}
    pub fn sequence(&self)->u64{self.sequence}
    pub fn apply(&mut self,bytes:&[u8])->Result<ApplyReport,SyncError>{
        if bytes.len()<SYNC_HEADER_BYTES||bytes.len()>MAX_SYNC_PACKET_BYTES{return Err(SyncError::Malformed);}
        let mut r=Reader{bytes,at:0};if r.take::<2>()?!=*b"IS"||r.byte()?!=1{return Err(SyncError::Malformed);}
        let flags=r.byte()?;if flags>1{return Err(SyncError::Malformed);}let reset=flags==1;
        let stream=r.u64()?;let sequence=r.u64()?;let base=r.u64()?;let tick=Tick(r.u64()?);
        let count=u16::from_le_bytes(r.take()?) as usize;if r.take::<2>()?!=[0,0]||count>MAX_VISIBLE_ENTITIES*2{return Err(SyncError::Malformed);}
        if stream!=self.stream{return Err(SyncError::WrongStream);}
        if sequence==self.sequence&&self.last_packet==bytes{return Ok(ApplyReport{stream,sequence,tick,duplicate:true});}
        if sequence<=self.sequence||(!reset&&(self.sequence==0||base!=self.sequence))||(reset&&base!=0){return Err(SyncError::OutOfOrder);}
        if tick.0<self.tick.0{return Err(SyncError::StaleTick);}
        let old: &[MotionState]=if reset{&[]}else{&self.states};let mut cursor=0;let mut previous=None;
        self.scratch.clear();
        for _ in 0..count{
            let entity=EntityId{index:u32::from_le_bytes(r.take()?),generation:u32::from_le_bytes(r.take()?)};let k=key(entity);
            if previous.is_some_and(|p|p>=k){return Err(SyncError::DuplicateEntity);}previous=Some(k);
            let mask=r.byte()?;if mask==0||mask&128!=0||(mask&DX!=0&&mask&X==0)||(mask&DY!=0&&mask&Y==0){return Err(SyncError::Malformed);}
            while cursor<old.len()&&key(old[cursor].entity)<k{self.scratch.push(old[cursor]);cursor+=1;}
            let existing=old.get(cursor).filter(|s|s.entity==entity).copied();
            if reset&&mask!=(ENTER|X|Y|STATUS){return Err(SyncError::Malformed);}
            if mask&LEAVE!=0{if mask!=LEAVE{return Err(SyncError::Malformed);}existing.ok_or(SyncError::MissingEntity)?;cursor+=1;}
            else if mask&ENTER!=0{
                if mask!=(ENTER|X|Y|STATUS){return Err(SyncError::Malformed);}if existing.is_some(){return Err(SyncError::DuplicateEntity);}
                let position=GridPosition{x:r.coordinate(0,false)?,y:r.coordinate(0,false)?};let movement=r.status()?;
                self.scratch.push(MotionState{entity,position,movement});
            }else{
                let mut state=existing.ok_or(SyncError::MissingEntity)?;cursor+=1;
                if mask&X!=0{state.position.x=r.coordinate(state.position.x,mask&DX!=0)?;}
                if mask&Y!=0{state.position.y=r.coordinate(state.position.y,mask&DY!=0)?;}
                if mask&STATUS!=0{state.movement=r.status()?;}self.scratch.push(state);
            }
            if self.scratch.len()>MAX_VISIBLE_ENTITIES{return Err(SyncError::LimitExceeded);}
        }
        self.scratch.extend_from_slice(&old[cursor..]);
        if r.at!=bytes.len(){return Err(SyncError::Malformed);}
        if self.scratch.len()>MAX_VISIBLE_ENTITIES{return Err(SyncError::LimitExceeded);}
        if self.scratch.windows(2).any(|s|s[0].entity.index==s[1].entity.index){return Err(SyncError::DuplicateEntity);}
        std::mem::swap(&mut self.states,&mut self.scratch);self.sequence=sequence;self.tick=tick;
        self.last_packet.clear();self.last_packet.extend_from_slice(bytes);
        Ok(ApplyReport{stream,sequence,tick,duplicate:false})
    }
}
