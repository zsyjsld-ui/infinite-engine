use std::sync::Arc;
use sha2::{Digest, Sha256};

const HEADER: usize = 64;
const DIGEST: usize = 32;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordKey { pub table: u16, pub id: u64 }
/// None deletes a record. An empty value is an existing zero-length record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change { pub key: RecordKey, pub value: Option<Arc<[u8]>> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalMark { pub history: [u8;16], pub sequence: u64, pub digest: [u8;32] }
impl JournalMark {
    fn validate(self) -> Result<(),StoreError> {
        if self.history == [0;16] { return Err(StoreError::InvalidHistory); }
        if self.sequence == 0 && self.digest != [0;32] { return Err(StoreError::Malformed); }
        Ok(())
    }
    /// The application supplies a fresh persistent history identifier. Reusing
    /// an identifier across independent stores is outside this contract.
    pub fn initial(history: [u8;16]) -> Result<Self,StoreError> {
        if history == [0;16] { return Err(StoreError::InvalidHistory); }
        Ok(Self { history, sequence: 0, digest: [0;32] })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind { Transaction, Checkpoint }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_records: usize, pub max_record_bytes: usize,
    pub max_transaction_records: usize, pub max_transaction_bytes: usize,
    pub max_checkpoint_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self { Self { max_records: 1_000_000, max_record_bytes: 65_536,
        max_transaction_records: 4096, max_transaction_bytes: 4*1024*1024,
        max_checkpoint_bytes: 256*1024*1024 } }
}
impl Limits {
    pub fn validate(self) -> Result<Self,StoreError> {
        let ceiling=Self::default();
        if self.max_records==0 || self.max_records>ceiling.max_records
            || self.max_record_bytes==0 || self.max_record_bytes>ceiling.max_record_bytes
            || self.max_transaction_records==0 || self.max_transaction_records>ceiling.max_transaction_records
            || self.max_transaction_records>self.max_records
            || !(HEADER+DIGEST..=ceiling.max_transaction_bytes).contains(&self.max_transaction_bytes)
            || !(HEADER+DIGEST..=ceiling.max_checkpoint_bytes).contains(&self.max_checkpoint_bytes) {
            return Err(StoreError::InvalidLimits);
        }
        Ok(self)
    }
    fn count(self,kind:FrameKind)->usize { match kind { FrameKind::Transaction=>self.max_transaction_records,FrameKind::Checkpoint=>self.max_records } }
    fn bytes(self,kind:FrameKind)->usize { match kind { FrameKind::Transaction=>self.max_transaction_bytes,FrameKind::Checkpoint=>self.max_checkpoint_bytes } }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError { InvalidLimits, InvalidHistory, SequenceExhausted, WrongHistory,
    ChainMismatch, SequenceGap, UnsortedKeys, RecordLimit, RecordTooLarge, FrameTooLarge,
    Truncated, Malformed, ChecksumMismatch }
#[derive(Debug, PartialEq, Eq)]
pub struct DecodedFrame {
    pub kind: FrameKind, pub history: [u8;16], pub sequence: u64,
    /// Transactions point to the prior commit digest. Checkpoints retain the
    /// digest of their captured commit, independently of their own checksum.
    pub previous: [u8;32], pub checksum: [u8;32], pub changes: Vec<Change>,
}
impl DecodedFrame {
    pub fn checkpoint_mark(&self) -> Result<JournalMark,StoreError> {
        if self.kind!=FrameKind::Checkpoint { return Err(StoreError::Malformed); }
        Ok(JournalMark{history:self.history,sequence:self.sequence,digest:self.previous})
    }
    pub fn successor(&self,base:JournalMark)->Result<JournalMark,StoreError> {
        base.validate()?;
        if self.kind!=FrameKind::Transaction { return Err(StoreError::Malformed); }
        if self.history!=base.history { return Err(StoreError::WrongHistory); }
        if base.sequence.checked_add(1)!=Some(self.sequence) { return Err(StoreError::SequenceGap); }
        if self.previous!=base.digest { return Err(StoreError::ChainMismatch); }
        Ok(JournalMark{history:self.history,sequence:self.sequence,digest:self.checksum})
    }
}
fn var(out:&mut Vec<u8>,mut value:u64) { while value>=128 { out.push(value as u8|128);value>>=7; }out.push(value as u8); }
fn var_len(mut value:u64)->usize { let mut len=1;while value>=128 { len+=1;value>>=7; }len }
fn encode(kind:FrameKind,mark:JournalMark,changes:&[Change],limits:Limits)->Result<(Vec<u8>,[u8;32]),StoreError> {
    let limits=limits.validate()?;
    if mark.history==[0;16] { return Err(StoreError::InvalidHistory); }
    if (kind==FrameKind::Transaction&&mark.sequence==0)||(kind==FrameKind::Checkpoint&&mark.sequence==0&&mark.digest!=[0;32]) { return Err(StoreError::Malformed); }
    if changes.len()>limits.count(kind) { return Err(StoreError::RecordLimit); }
    let mut size=HEADER+DIGEST;let mut prior=None;
    for change in changes {
        if prior.is_some_and(|key|key>=change.key) { return Err(StoreError::UnsortedKeys); }prior=Some(change.key);
        size=size.checked_add(1+var_len(u64::from(change.key.table))+var_len(change.key.id)).ok_or(StoreError::FrameTooLarge)?;
        if let Some(value)=&change.value {
            if value.len()>limits.max_record_bytes { return Err(StoreError::RecordTooLarge); }
            size=size.checked_add(var_len(value.len()as u64)+value.len()).ok_or(StoreError::FrameTooLarge)?;
        } else if kind==FrameKind::Checkpoint { return Err(StoreError::Malformed); }
    }
    if size>limits.bytes(kind) { return Err(StoreError::FrameTooLarge); }
    let mut out=Vec::with_capacity(size);
    out.extend_from_slice(match kind { FrameKind::Transaction=>b"IJ\x01\x00",FrameKind::Checkpoint=>b"IK\x01\x00" });
    out.extend_from_slice(&mark.history);out.extend_from_slice(&mark.sequence.to_le_bytes());out.extend_from_slice(&mark.digest);out.extend_from_slice(&(changes.len()as u32).to_le_bytes());
    for change in changes {out.push(u8::from(change.value.is_some()));var(&mut out,u64::from(change.key.table));var(&mut out,change.key.id);if let Some(value)=&change.value {var(&mut out,value.len()as u64);out.extend_from_slice(value);}}
    let digest:[u8;32]=Sha256::digest(&out).into();out.extend_from_slice(&digest);debug_assert_eq!(out.len(),size);Ok((out,digest))
}
pub fn encode_transaction(base:JournalMark,changes:&[Change],limits:Limits)->Result<(Vec<u8>,JournalMark),StoreError> {
    base.validate()?;
    let sequence=base.sequence.checked_add(1).ok_or(StoreError::SequenceExhausted)?;
    let(bytes,digest)=encode(FrameKind::Transaction,JournalMark{sequence,..base},changes,limits)?;
    Ok((bytes,JournalMark{sequence,digest,..base}))
}
pub fn encode_checkpoint(mark:JournalMark,records:&[Change],limits:Limits)->Result<Vec<u8>,StoreError> {
    encode(FrameKind::Checkpoint,mark,records,limits).map(|(bytes,_)|bytes)
}
struct Reader<'a> { bytes:&'a [u8],at:usize }
impl Reader<'_> {
    fn var(&mut self)->Result<u64,StoreError> {let mut value=0;for i in 0..10{let byte=*self.bytes.get(self.at).ok_or(StoreError::Truncated)?;self.at+=1;if i==9&&byte>1{return Err(StoreError::Malformed);}value|=u64::from(byte&127)<<(i*7);if byte&128==0{if i>0&&byte==0{return Err(StoreError::Malformed);}return Ok(value);}}Err(StoreError::Malformed)}
    fn byte(&mut self)->Result<u8,StoreError>{let b=*self.bytes.get(self.at).ok_or(StoreError::Truncated)?;self.at+=1;Ok(b)}
}
pub fn decode_frame(bytes:&[u8],limits:Limits)->Result<DecodedFrame,StoreError> {
    let limits=limits.validate()?;if bytes.len()<HEADER+DIGEST { return Err(StoreError::Truncated); }
    let kind=match &bytes[..4] {b"IJ\x01\x00"=>FrameKind::Transaction,b"IK\x01\x00"=>FrameKind::Checkpoint,_=>return Err(StoreError::Malformed)};
    if bytes.len()>limits.bytes(kind) { return Err(StoreError::FrameTooLarge); }
    let end=bytes.len()-DIGEST;let checksum:[u8;32]=bytes[end..].try_into().unwrap();let calculated:[u8;32]=Sha256::digest(&bytes[..end]).into();if calculated!=checksum{return Err(StoreError::ChecksumMismatch);}
    let history=bytes[4..20].try_into().unwrap();if history==[0;16]{return Err(StoreError::InvalidHistory);}let sequence=u64::from_le_bytes(bytes[20..28].try_into().unwrap());let previous=bytes[28..60].try_into().unwrap();
    if (kind==FrameKind::Transaction&&sequence==0)||(kind==FrameKind::Checkpoint&&sequence==0&&previous!=[0;32]){return Err(StoreError::Malformed);}
    let count=u32::from_le_bytes(bytes[60..64].try_into().unwrap())as usize;if count>limits.count(kind){return Err(StoreError::RecordLimit);}
    // Even a deletion needs an operation byte and two one-byte key varints.
    // Reject an impossible count before reserving its bounded but large vector.
    if count > (end-HEADER)/3 { return Err(StoreError::Truncated); }
    let mut r=Reader{bytes:&bytes[..end],at:HEADER};let mut changes=Vec::with_capacity(count);let mut prior=None;
    for _ in 0..count {let op=r.byte()?;if op>1||(kind==FrameKind::Checkpoint&&op==0){return Err(StoreError::Malformed);}let table=u16::try_from(r.var()?).map_err(|_|StoreError::Malformed)?;let id=r.var()?;let key=RecordKey{table,id};if prior.is_some_and(|old|old>=key){return Err(StoreError::UnsortedKeys);}prior=Some(key);
        let value=if op==1{let length=usize::try_from(r.var()?).map_err(|_|StoreError::RecordTooLarge)?;if length>limits.max_record_bytes{return Err(StoreError::RecordTooLarge);}let stop=r.at.checked_add(length).ok_or(StoreError::Truncated)?;let data=r.bytes.get(r.at..stop).ok_or(StoreError::Truncated)?;r.at=stop;Some(Arc::from(data))}else{None};changes.push(Change{key,value});
    }
    if r.at!=end{return Err(StoreError::Malformed);}Ok(DecodedFrame{kind,history,sequence,previous,checksum,changes})
}
