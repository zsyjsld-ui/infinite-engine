use std::{num::NonZeroU32,collections::VecDeque};
use crate::{CombatEndReason,CombatFeedback,CombatFeedbackKind,CombatSyncError,EntityId,HealthState,
    SyncError,SyncHeader,Tick,VitalState,WindupView,MAX_VISIBLE_ENTITIES,MAX_COMBAT_EVENTS_PER_PACKET};
use crate::{combat_sync::VitalChange,sync::key};
pub const COMBAT_SYNC_HEADER_BYTES:usize=40;
const LEGACY_PACKET_LIMIT:usize=COMBAT_SYNC_HEADER_BYTES+MAX_VISIBLE_ENTITIES*51+MAX_COMBAT_EVENTS_PER_PACKET*41;
pub const MAX_COMBAT_SYNC_PACKET_BYTES:usize=COMBAT_SYNC_HEADER_BYTES+5+MAX_VISIBLE_ENTITIES*63+MAX_COMBAT_EVENTS_PER_PACKET*41;
const HP:u8=1;const MAX:u8=2;const WINDUP:u8=4;const ENTER:u8=8;const LEAVE:u8=16;const CLEAR:u8=32;
const COMMON_MAX:u8=64;const FULL_HP:u8=128;
mod compact;
pub(crate) use compact::{encode_compact,encode_batched};
fn id(out:&mut Vec<u8>,id:EntityId){out.extend_from_slice(&id.index.to_le_bytes());out.extend_from_slice(&id.generation.to_le_bytes());}
fn tick(out:&mut Vec<u8>,tick:Tick){out.extend_from_slice(&tick.0.to_le_bytes());}
fn reason_code(reason:CombatEndReason)->u8{match reason{
    CombatEndReason::AttackerDepleted=>0,CombatEndReason::TargetDepleted=>1,CombatEndReason::TargetGone=>2,
    CombatEndReason::HealthUnavailable=>3,CombatEndReason::CooldownOverflow=>4,
}}
pub(crate) fn encode(h:SyncHeader,changes:&[VitalChange],baseline:&[VitalState],latest:&[VitalState],events:&[CombatFeedback],out:&mut Vec<u8>){
    out.clear();out.extend_from_slice(b"IC\x01");out.push(u8::from(h.reset));
    for n in [h.stream,h.sequence,h.base,h.tick.0]{out.extend_from_slice(&n.to_le_bytes());}
    out.extend_from_slice(&(changes.len()as u16).to_le_bytes());out.extend_from_slice(&(events.len()as u16).to_le_bytes());
    for change in changes{
        let(entity,before,after)=change.resolve(baseline,latest);
        id(out,entity);let Some(after)=after else{out.push(LEAVE);continue;};
        let old_health=before.and_then(|s|s.health);let mut mask=if before.is_none(){ENTER}else{0};
        if let Some(health)=after.health{
            if old_health.is_none_or(|old|old.current!=health.current){mask|=HP;}
            if old_health.is_none_or(|old|old.maximum!=health.maximum){mask|=MAX;}
        }else if before.is_none()||old_health.is_some(){mask|=CLEAR;}
        if before.is_none_or(|old|old.windup!=after.windup){mask|=WINDUP;}
        out.push(mask);
        if mask&HP!=0{out.extend_from_slice(&after.health.unwrap().current.to_le_bytes());}
        if mask&MAX!=0{out.extend_from_slice(&after.health.unwrap().maximum.get().to_le_bytes());}
        if mask&WINDUP!=0{out.push(u8::from(after.windup.is_some()));if let Some(w)=after.windup{id(out,w.target);tick(out,w.started_at);tick(out,w.hit_at);}}
    }
    for event in events{
        out.push(match event.kind{CombatFeedbackKind::WindupStarted{..}=>0,CombatFeedbackKind::WindupCancelled{..}=>1,CombatFeedbackKind::Hit{..}=>2,CombatFeedbackKind::Ended(_)=>3});
        tick(out,event.tick);id(out,event.attacker);id(out,event.target);
        match event.kind{
            CombatFeedbackKind::WindupStarted{started_at,hit_at}=>{tick(out,started_at);tick(out,hit_at);},
            CombatFeedbackKind::WindupCancelled{started_at}=>tick(out,started_at),
            CombatFeedbackKind::Hit{damage,remaining}=>{out.extend_from_slice(&damage.to_le_bytes());out.extend_from_slice(&remaining.to_le_bytes());},
            CombatFeedbackKind::Ended(reason)=>out.push(reason_code(reason)),
        }
    }
}
struct Reader<'a,const COMPACT:bool>{bytes:&'a[u8],at:usize}
impl<const COMPACT:bool> Reader<'_,COMPACT>{
    fn take<const N:usize>(&mut self)->Result<[u8;N],SyncError>{let value=self.bytes.get(self.at..self.at+N).ok_or(SyncError::Malformed)?;self.at+=N;Ok(value.try_into().unwrap())}
    #[inline]
    fn byte(&mut self)->Result<u8,SyncError>{Ok(self.take::<1>()?[0])}
    fn u32(&mut self)->Result<u32,SyncError>{Ok(u32::from_le_bytes(self.take()?))}
    fn u64(&mut self)->Result<u64,SyncError>{Ok(u64::from_le_bytes(self.take()?))}
    #[inline]
    fn var(&mut self)->Result<u64,SyncError>{
        let first=self.byte()?;if first<128{return Ok(first.into());}
        let second=self.byte()?;if second<128{if second==0{return Err(SyncError::Malformed);}return Ok(u64::from(first&127)|(u64::from(second)<<7));}
        let third=self.byte()?;let low=u64::from(first&127)|(u64::from(second&127)<<7);
        if third<128{if third==0{return Err(SyncError::Malformed);}return Ok(low|(u64::from(third)<<14));}
        let mut value=low|(u64::from(third&127)<<14);for i in 3..10{let b=self.byte()?;if i==9&&b>1{return Err(SyncError::Malformed);}
            value|=u64::from(b&127)<<(i*7);if b<128{if i>0&&b==0{return Err(SyncError::Malformed);}return Ok(value);}}
        Err(SyncError::Malformed)
    }
    #[inline]
    fn value(&mut self)->Result<u32,SyncError>{if COMPACT{u32::try_from(self.var()?).map_err(|_|SyncError::Malformed)}else{self.u32()}}
    #[inline]
    fn id(&mut self)->Result<EntityId,SyncError>{
        if COMPACT{if let Some(b)=self.bytes.get(self.at..self.at+3){
            if b[0]<128&&b[1]<128{self.at+=2;return Ok(EntityId{index:u32::from(b[0]),generation:u32::from(b[1])});}
            if b[0]>=128&&b[1]>0&&b[1]<128&&b[2]<128{self.at+=3;return Ok(EntityId{index:u32::from(b[0]&127)|(u32::from(b[1])<<7),generation:u32::from(b[2])});}
        }}
        Ok(EntityId{index:self.value()?,generation:self.value()?})
    }
    #[inline]
    fn state_head(&mut self)->Result<(EntityId,u8),SyncError>{
        if COMPACT{if let Some(b)=self.bytes.get(self.at..self.at+3){if b[0]<128&&b[1]<128{
            self.at+=3;return Ok((EntityId{index:u32::from(b[0]),generation:u32::from(b[1])},b[2]));
        }}}
        Ok((self.id()?,self.byte()?))
    }
    #[inline]
    fn ago(&mut self,now:Tick)->Result<Tick,SyncError>{now.0.checked_sub(self.var()?).map(Tick).ok_or(SyncError::Malformed)}
    #[inline]
    fn later(&mut self,start:Tick)->Result<Tick,SyncError>{start.0.checked_add(self.var()?).map(Tick).ok_or(SyncError::Malformed)}
    fn windup(&mut self,now:Tick)->Result<Option<WindupView>,SyncError>{
        match self.byte()?{0=>Ok(None),1=>{let target=self.id()?;let started_at=if COMPACT{self.ago(now)?}else{Tick(self.u64()?)};
            let hit_at=if COMPACT{self.later(started_at)?}else{Tick(self.u64()?)};let w=WindupView{target,started_at,hit_at};
            if w.started_at>now||w.hit_at<w.started_at{return Err(SyncError::Malformed);}Ok(Some(w))},_=>Err(SyncError::Malformed)}
    }
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CombatApplyReport { pub stream:u64,pub sequence:u64,pub tick:Tick,pub duplicate:bool,pub events:usize }
impl CombatApplyReport{
    pub fn acknowledgement(self)->[u8;20]{let mut out=[0;20];out[..4].copy_from_slice(b"CA\x01\x00");out[4..12].copy_from_slice(&self.stream.to_le_bytes());out[12..].copy_from_slice(&self.sequence.to_le_bytes());out}
}
pub(crate) fn decode_ack(bytes:&[u8])->Result<(u64,u64),SyncError>{
    if bytes.len()!=20||&bytes[..4]!=b"CA\x01\x00"{return Err(SyncError::Malformed);}
    Ok((u64::from_le_bytes(bytes[4..12].try_into().unwrap()),u64::from_le_bytes(bytes[12..].try_into().unwrap())))
}
/// Atomic state+event application. Drain feedback before accepting a different
/// packet. Exact retransmissions can be acknowledged without replaying events.
pub struct CombatReplica {
    stream:u64,sequence:u64,tick:Tick,last_event:Option<Tick>,
    windowed:bool,recent:VecDeque<(u64,Vec<u8>)>,
    states:Vec<VitalState>,scratch:Vec<VitalState>,feedback:VecDeque<CombatFeedback>,event_scratch:VecDeque<CombatFeedback>,last_packet:Vec<u8>,
}
impl CombatReplica{
    pub fn new(stream:u64)->Self{Self{stream,sequence:0,tick:Tick(0),last_event:None,windowed:false,recent:VecDeque::new(),states:Vec::new(),scratch:Vec::new(),feedback:VecDeque::new(),event_scratch:VecDeque::new(),last_packet:Vec::new()}}
    /// Explicit IC v5 receiver with bounded exact-packet replay history and
    /// cumulative ACKs. A legacy replica continues to reject v5 packets.
    pub fn new_windowed(stream:u64)->Self{let mut replica=Self::new(stream);replica.windowed=true;replica}
    pub fn states(&self)->&[VitalState]{&self.states}
    pub fn sequence(&self)->u64{self.sequence}
    /// Stopping iteration early retains unconsumed feedback for the next call.
    pub fn drain_feedback(&mut self)->impl Iterator<Item=CombatFeedback> + '_ {std::iter::from_fn(move ||self.feedback.pop_front())}
    pub fn apply(&mut self,bytes:&[u8])->Result<CombatApplyReport,CombatSyncError>{
        if self.windowed&&bytes.get(2)==Some(&5)&&(bytes==self.last_packet||self.recent.iter().any(|(_,packet)|packet==bytes)){
            return Ok(CombatApplyReport{stream:self.stream,sequence:self.sequence,tick:self.tick,duplicate:true,events:0});
        }
        if !self.feedback.is_empty()&&bytes!=self.last_packet{return Err(CombatSyncError::EventBackpressure);}
        self.apply_inner(bytes).map_err(Into::into)
    }
    fn apply_inner(&mut self,bytes:&[u8])->Result<CombatApplyReport,SyncError>{
        if bytes.len()<COMBAT_SYNC_HEADER_BYTES||bytes.len()>MAX_COMBAT_SYNC_PACKET_BYTES{return Err(SyncError::Malformed);}
        if &bytes[..2]!=b"IC"{return Err(SyncError::Malformed);}
        if self.windowed!=(bytes[2]==5){return Err(SyncError::Malformed);}
        match bytes[2]{
            1 if bytes.len()<=LEGACY_PACKET_LIMIT=>self.apply_format::<false,false,false>(bytes),
            2=>self.apply_format::<true,false,false>(bytes),
            3=>self.apply_format::<true,true,false>(bytes),
            4=>self.apply_format::<true,true,true>(bytes),
            5=>self.apply_format::<true,true,true>(bytes),
            _=>Err(SyncError::Malformed),
        }
    }
    // Specialize once per packet; field readers do not branch on wire version.
    fn apply_format<const COMPACT:bool,const BATCHED:bool,const SHIELDED:bool>(&mut self,bytes:&[u8])->Result<CombatApplyReport,SyncError>{
        let mut r=Reader::<COMPACT>{bytes,at:3};
        let flags=r.byte()?;if flags>1{return Err(SyncError::Malformed);}let reset=flags==1;
        let stream=r.u64()?;let sequence=r.u64()?;let base=r.u64()?;let now=Tick(r.u64()?);
        let count=u16::from_le_bytes(r.take()?)as usize;let event_count=u16::from_le_bytes(r.take()?)as usize;
        if count>MAX_VISIBLE_ENTITIES*2||event_count>MAX_COMBAT_EVENTS_PER_PACKET{return Err(SyncError::LimitExceeded);}
        if stream!=self.stream{return Err(SyncError::WrongStream);}
        if sequence==self.sequence&&bytes==self.last_packet{return Ok(CombatApplyReport{stream,sequence,tick:now,duplicate:true,events:0});}
        if sequence<=self.sequence||(!reset&&(self.sequence==0||base!=self.sequence))||(reset&&base!=0){return Err(SyncError::OutOfOrder);}
        if now<self.tick{return Err(SyncError::StaleTick);}
        let common_max=if COMPACT{r.value()?}else{0};let mut index_base=0u32;
        // Event chunks retain the already validated snapshot. A reset, even
        // with zero rows, must still replace it with the requested empty state.
        let update_states=count!=0||reset;
        if update_states{
        let old: &[VitalState]=if reset{&[]}else{&self.states};let mut cursor=0;let mut previous=None;self.scratch.clear();
        for _ in 0..count{
            let (mut entity,mask)=r.state_head()?;if COMPACT{entity.index=index_base.checked_add(entity.index).ok_or(SyncError::Malformed)?;index_base=entity.index;}
            let k=key(entity);if previous.is_some_and(|p|p>=k){return Err(SyncError::DuplicateEntity);}previous=Some(k);
            if mask==0||(!COMPACT&&mask&192!=0){return Err(SyncError::Malformed);}
            if mask&(HP|FULL_HP)==(HP|FULL_HP)||mask&(MAX|COMMON_MAX)==(MAX|COMMON_MAX){return Err(SyncError::Malformed);}
            while cursor<old.len()&&key(old[cursor].entity)<k{self.scratch.push(old[cursor]);cursor+=1;}
            let existing=old.get(cursor).filter(|s|s.entity==entity).copied();
            if reset&&mask&ENTER==0{return Err(SyncError::Malformed);}
            if mask&LEAVE!=0{if mask!=LEAVE{return Err(SyncError::Malformed);}existing.ok_or(SyncError::MissingEntity)?;cursor+=1;continue;}
            // Exact masks only: all other forms retain the general validation
            // path. An entering HP value cannot reference a prior component.
            if COMPACT&&(mask==(ENTER|COMMON_MAX|FULL_HP)||mask==(ENTER|COMMON_MAX|HP)){
                if existing.is_some(){return Err(SyncError::DuplicateEntity);}
                let maximum=NonZeroU32::new(common_max).ok_or(SyncError::Malformed)?;
                let current=if mask&FULL_HP!=0{common_max}else{
                    let code=r.var()?;if code&1!=0{return Err(SyncError::Malformed);}
                    u32::try_from(code>>1).map_err(|_|SyncError::Malformed)?
                };
                if current>common_max{return Err(SyncError::Malformed);}
                self.scratch.push(VitalState{entity,health:Some(HealthState{current,maximum}),windup:None});
                if self.scratch.len()>MAX_VISIBLE_ENTITIES{return Err(SyncError::LimitExceeded);}continue;
            }
            let mut state=if mask&ENTER!=0{
                if existing.is_some(){return Err(SyncError::DuplicateEntity);}
                if !COMPACT&&mask&WINDUP==0{return Err(SyncError::Malformed);}
                if mask&CLEAR==0&&(mask&(HP|FULL_HP)==0||mask&(MAX|COMMON_MAX)==0){return Err(SyncError::Malformed);}
                VitalState{entity,health:None,windup:None}
            }else{cursor+=1;existing.ok_or(SyncError::MissingEntity)?};
            if mask&CLEAR!=0{if mask&(HP|MAX|COMMON_MAX|FULL_HP)!=0{return Err(SyncError::Malformed);}state.health=None;}
            else if mask&(HP|MAX|COMMON_MAX|FULL_HP)!=0{
                if state.health.is_none()&&(mask&(HP|FULL_HP)==0||mask&(MAX|COMMON_MAX)==0){return Err(SyncError::Malformed);}
                let current=if mask&HP!=0{
                    if COMPACT{let code=r.var()?;if code>u64::from(u32::MAX)*4+1{return Err(SyncError::Malformed);}
                        if code&1==0{u32::try_from(code>>1).map_err(|_|SyncError::Malformed)?}
                        else{let z=code>>1;let delta=((z>>1)as i64)^-((z&1)as i64);let before=state.health.ok_or(SyncError::Malformed)?.current;
                            u32::try_from(i64::from(before)+delta).map_err(|_|SyncError::Malformed)?}
                    }else{r.u32()?}
                }else{state.health.map_or(0,|h|h.current)};
                let maximum=if mask&(MAX|COMMON_MAX)!=0{NonZeroU32::new(if mask&COMMON_MAX!=0{common_max}else{r.value()?}).ok_or(SyncError::Malformed)?}else{state.health.unwrap().maximum};
                let current=if mask&FULL_HP!=0{maximum.get()}else{current};
                if current>maximum.get(){return Err(SyncError::Malformed);}state.health=Some(HealthState{current,maximum});
            }
            if mask&WINDUP!=0{state.windup=r.windup(now)?;}
            self.scratch.push(state);if self.scratch.len()>MAX_VISIBLE_ENTITIES{return Err(SyncError::LimitExceeded);}
        }
        self.scratch.extend_from_slice(&old[cursor..]);
        if self.scratch.len()>MAX_VISIBLE_ENTITIES{return Err(SyncError::LimitExceeded);}
        if self.scratch.windows(2).any(|s|s[0].entity.index==s[1].entity.index){return Err(SyncError::DuplicateEntity);}
        // Nearby paired combatants and consecutive attacks on one target are
        // common. Reuse only successful checks within this immutable snapshot;
        // arbitrary target order still falls back to the full handle lookup.
        let mut validated_target=None;
        for (i,state) in self.scratch.iter().enumerate(){if let Some(w)=state.windup{
            if w.target==state.entity{return Err(SyncError::MissingEntity);}
            if validated_target==Some(w.target){continue;}
            if !self.scratch.get(i+1).is_some_and(|s|s.entity==w.target)
                &&!self.scratch.get(i.wrapping_sub(1)).is_some_and(|s|s.entity==w.target)
                &&self.scratch.binary_search_by_key(&key(w.target),|s|key(s.entity)).is_err(){return Err(SyncError::MissingEntity);}
            validated_target=Some(w.target);
        }}
        }
        self.event_scratch.clear();let mut last=self.last_event;
        for _ in 0..event_count{
            let tag=r.byte()?;
            if BATCHED&&tag&0xe0!=0{return Err(SyncError::Malformed);}
            // References are packet-local, never to an ACK baseline or the
            // previous packet. A first-record reuse bit is malformed.
            let previous=if BATCHED{self.event_scratch.back().copied()}else{None};
            let code=if BATCHED{tag&3}else{tag};
            let at=if BATCHED&&tag&4!=0{previous.ok_or(SyncError::Malformed)?.tick}
                else if COMPACT{r.ago(now)?}else{Tick(r.u64()?)};
            let attacker=if BATCHED&&tag&8!=0{previous.ok_or(SyncError::Malformed)?.attacker}else{r.id()?};
            let target=if BATCHED&&tag&16!=0{previous.ok_or(SyncError::Malformed)?.target}else{r.id()?};
            if at>now||last.is_some_and(|t|at<t)||(attacker==target&&(!SHIELDED||code!=2)){return Err(SyncError::Malformed);}last=Some(at);
            let kind=match code{
                0=>{let started_at=if COMPACT{at}else{Tick(r.u64()?)};let hit_at=if COMPACT{r.later(started_at)?}else{Tick(r.u64()?)};if started_at!=at||hit_at<started_at{return Err(SyncError::Malformed);}CombatFeedbackKind::WindupStarted{started_at,hit_at}},
                1=>{let started_at=if COMPACT{r.ago(at)?}else{Tick(r.u64()?)};if started_at>at{return Err(SyncError::Malformed);}CombatFeedbackKind::WindupCancelled{started_at}},
                2=>{let damage=r.value()?;let remaining=r.value()?;if !SHIELDED&&damage==0{return Err(SyncError::Malformed);}CombatFeedbackKind::Hit{damage,remaining}},
                3=>CombatFeedbackKind::Ended(match r.byte()?{0=>CombatEndReason::AttackerDepleted,1=>CombatEndReason::TargetDepleted,2=>CombatEndReason::TargetGone,3=>CombatEndReason::HealthUnavailable,4=>CombatEndReason::CooldownOverflow,_=>return Err(SyncError::Malformed)}),
                _=>return Err(SyncError::Malformed),
            };self.event_scratch.push_back(CombatFeedback{tick:at,attacker,target,kind});
        }
        if r.at!=bytes.len(){return Err(SyncError::Malformed);}
        if update_states{std::mem::swap(&mut self.states,&mut self.scratch);}
        std::mem::swap(&mut self.feedback,&mut self.event_scratch);
        if self.windowed&&!self.last_packet.is_empty(){let spare=if self.recent.len()==crate::MAX_COMBAT_WINDOW_PACKETS-1{self.recent.pop_front().unwrap().1}else{Vec::new()};let previous=std::mem::replace(&mut self.last_packet,spare);self.recent.push_back((self.sequence,previous));}
        self.sequence=sequence;self.tick=now;self.last_event=last;self.last_packet.clear();self.last_packet.extend_from_slice(bytes);
        Ok(CombatApplyReport{stream,sequence,tick:now,duplicate:false,events:event_count})
    }
}
