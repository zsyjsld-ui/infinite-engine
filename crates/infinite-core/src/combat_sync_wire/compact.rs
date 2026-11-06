use super::*;
// One bounded stack buffer per encode call. Every record has at most 52 bytes;
// keeping 64 bytes free also covers a ten-byte varint's checked scratch window.
struct Buffer { bytes:[u8;2048],len:usize }
impl Buffer {
    fn flush(&mut self,out:&mut Vec<u8>){out.extend_from_slice(&self.bytes[..self.len]);self.len=0;}
    #[inline]
    fn room(&mut self,out:&mut Vec<u8>){if self.len>self.bytes.len()-64{self.flush(out);}}
    #[inline]
    fn byte(&mut self,n:u8){self.bytes[self.len]=n;self.len+=1;}
    #[inline]
    fn var(&mut self,mut n:u64){
        let dst=&mut self.bytes[self.len..self.len+10];
        if n<128{dst[0]=n as u8;self.len+=1;}
        else if n<16384{dst[0]=(n as u8&127)|128;dst[1]=(n>>7)as u8;self.len+=2;}
        else if n<2097152{dst[0]=(n as u8&127)|128;dst[1]=((n>>7)as u8&127)|128;dst[2]=(n>>14)as u8;self.len+=3;}
        else{for (i,b)in dst.iter_mut().enumerate(){*b=(n as u8&127)|if n>=128{128}else{0};n>>=7;if n==0{self.len+=i+1;return;}}}
    }
    #[inline]
    fn entity(&mut self,id:EntityId){self.var(u64::from(id.index));self.var(u64::from(id.generation));}
    #[inline]
    fn state_head(&mut self,delta:u32,generation:u32,mask:u8){
        if delta<128&&generation<128{
            self.bytes[self.len..self.len+3].copy_from_slice(&[delta as u8,generation as u8,mask]);self.len+=3;
        }else{self.var(u64::from(delta));self.var(u64::from(generation));self.byte(mask);}
    }
}
/// Packet-local reuse only: no hidden/off-screen state is retained by the peer.
pub(crate) fn encode_compact(h:SyncHeader,changes:&[VitalChange],baseline:&[VitalState],latest:&[VitalState],events:&[CombatFeedback],out:&mut Vec<u8>){
    encode_format::<false>(h,changes,baseline,latest,events,out);
}
pub(crate) fn encode_batched(h:SyncHeader,changes:&[VitalChange],baseline:&[VitalState],latest:&[VitalState],events:&[CombatFeedback],out:&mut Vec<u8>){
    encode_format::<true>(h,changes,baseline,latest,events,out);
}
fn encode_format<const BATCHED:bool>(h:SyncHeader,changes:&[VitalChange],baseline:&[VitalState],latest:&[VitalState],events:&[CombatFeedback],out:&mut Vec<u8>){
    out.clear();out.extend_from_slice(if BATCHED{b"IC\x03"}else{b"IC\x02"});out.push(u8::from(h.reset));
    for n in [h.stream,h.sequence,h.base,h.tick.0]{out.extend_from_slice(&n.to_le_bytes());}
    out.extend_from_slice(&(changes.len()as u16).to_le_bytes());out.extend_from_slice(&(events.len()as u16).to_le_bytes());
    // Stop at the first available health component; searching for a changed
    // maximum would rescan every HP/windup delta in the common steady case.
    let common=changes.iter().find_map(|c|c.resolve(baseline,latest).2.and_then(|s|s.health).map(|h|h.maximum.get())).unwrap_or(0);
    let mut buffer=Buffer{bytes:[0;2048],len:0};buffer.var(u64::from(common));let mut previous=0;
    for c in changes{
        buffer.room(out);
        let(entity,before,after)=c.resolve(baseline,latest);
        let delta=entity.index-previous;previous=entity.index;
        let Some(after)=after else{buffer.state_head(delta,entity.generation,LEAVE);continue;};
        // Frequent AOI entries have no windup and share the packet maximum.
        // Their initial HP is absolute, so no baseline/delta selection is needed.
        if before.is_none()&&after.windup.is_none(){if let Some(health)=after.health{if health.maximum.get()==common{
            let full=health.current==common;
            buffer.state_head(delta,entity.generation,ENTER|COMMON_MAX|if full{FULL_HP}else{HP});
            if !full{buffer.var(u64::from(health.current)*2);}continue;
        }}}
        let old=before.and_then(|s|s.health);let mut mask=if before.is_none(){ENTER}else{0};
        if let Some(h)=after.health{
            if old.is_none_or(|o|o.current!=h.current){mask|=if h.current==h.maximum.get(){FULL_HP}else{HP};}
            if old.is_none_or(|o|o.maximum!=h.maximum){mask|=if h.maximum.get()==common{COMMON_MAX}else{MAX};}
        }else if before.is_none()||old.is_some(){mask|=CLEAR;}
        if before.map_or(after.windup.is_some(),|s|s.windup!=after.windup){mask|=WINDUP;}
        buffer.state_head(delta,entity.generation,mask);
        if mask&HP!=0{
            let current=after.health.unwrap().current;let mut code=u64::from(current)*2;
            if let Some(old)=old{let delta=i64::from(current)-i64::from(old.current);let zig=((delta<<1)^(delta>>63))as u64;code=code.min(zig*2+1);}
            buffer.var(code);
        }
        if mask&MAX!=0{buffer.var(u64::from(after.health.unwrap().maximum.get()));}
        if mask&WINDUP!=0{buffer.byte(u8::from(after.windup.is_some()));if let Some(w)=after.windup{buffer.entity(w.target);buffer.var(h.tick.0-w.started_at.0);buffer.var(w.hit_at.0-w.started_at.0);}}
    }
    let mut previous_event:Option<&CombatFeedback>=None;
    for e in events{
        buffer.room(out);
        let mut tag=match e.kind{CombatFeedbackKind::WindupStarted{..}=>0,CombatFeedbackKind::WindupCancelled{..}=>1,CombatFeedbackKind::Hit{..}=>2,CombatFeedbackKind::Ended(_)=>3};
        if BATCHED{if let Some(p)=previous_event{
            if p.tick==e.tick{tag|=4;}
            if p.attacker==e.attacker{tag|=8;}
            if p.target==e.target{tag|=16;}
        }}
        buffer.byte(tag);
        if tag&4==0{buffer.var(h.tick.0-e.tick.0);}
        if tag&8==0{buffer.entity(e.attacker);}
        if tag&16==0{buffer.entity(e.target);}
        match e.kind{
            CombatFeedbackKind::WindupStarted{started_at,hit_at}=>buffer.var(hit_at.0-started_at.0),
            CombatFeedbackKind::WindupCancelled{started_at}=>buffer.var(e.tick.0-started_at.0),
            CombatFeedbackKind::Hit{damage,remaining}=>{buffer.var(u64::from(damage));buffer.var(u64::from(remaining));},
            CombatFeedbackKind::Ended(reason)=>buffer.byte(reason_code(reason)),
        }
        previous_event=Some(e);
    }
    buffer.flush(out);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_state_heads_match_independent_fields_across_flushes(){
        fn reference(out:&mut Vec<u8>,mut n:u32){loop{let low=(n&127)as u8;n>>=7;out.push(low|if n==0{0}else{128});if n==0{break;}}}
        let values=[0,1,127,128,16383,16384,u32::MAX];
        let mut buffer=Buffer{bytes:[0;2048],len:0};let(mut actual,mut expected)=(Vec::new(),Vec::new());
        for _ in 0..20{for delta in values{for generation in values{for mask in [LEAVE,ENTER|COMMON_MAX|FULL_HP,ENTER|COMMON_MAX|HP,HP|WINDUP]{
            buffer.room(&mut actual);buffer.state_head(delta,generation,mask);
            reference(&mut expected,delta);reference(&mut expected,generation);expected.push(mask);
        }}}}buffer.flush(&mut actual);assert_eq!(actual,expected);
    }
    #[test]
    fn buffered_fields_match_reference_varints_across_flushes(){
        fn reference(out:&mut Vec<u8>,mut n:u64){loop{let byte=(n&127)as u8;n>>=7;out.push(byte|if n==0{0}else{128});if n==0{break;}}}
        let values=[0,1,127,128,16383,16384,2097151,2097152,u32::MAX as u64,1u64<<35,1u64<<63,u64::MAX];
        let mut buffer=Buffer{bytes:[0;2048],len:0};let(mut actual,mut expected)=(Vec::new(),Vec::new());
        for i in 0..1000{let value=values[i%values.len()];let id=EntityId{index:value as u32,generation:(value>>32)as u32};buffer.room(&mut actual);buffer.byte(17);buffer.var(value);buffer.entity(id);
            expected.push(17);reference(&mut expected,value);reference(&mut expected,u64::from(id.index));reference(&mut expected,u64::from(id.generation));}
        buffer.flush(&mut actual);assert_eq!(actual,expected);assert_eq!(buffer.len,0);
    }
}
