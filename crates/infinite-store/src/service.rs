//! Bounded ownership transfer to a serialized journal worker. A second worker
//! prepares at most one immutable checkpoint while commits continue.
use std::{collections::BTreeMap,io,sync::{Arc,atomic::{AtomicU8,Ordering},mpsc::{self,Receiver,SyncSender,TrySendError,TryRecvError,RecvTimeoutError}},thread::{self,JoinHandle},time::Duration};
use crate::{Change,CommitReceipt,Journal,JournalError,JournalMark,Limits,PreparedCheckpoint};
const QUEUED:u8=0;const CLAIMED:u8=1;const CANCELLED:u8=2;const FINISHED:u8=3;
#[derive(Clone,Copy,Debug)]
pub struct ServiceOptions{pub outstanding:usize,pub retained_bytes:usize}
impl Default for ServiceOptions{fn default()->Self{Self{outstanding:256,retained_bytes:32*1024*1024}}}
#[derive(Debug)]
pub enum ServiceError{InvalidOptions,Spawn(io::Error),WorkerLost,Journal(JournalError)}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum SubmitError{Full,Oversize,Closed,TicketExhausted}
#[derive(Debug)]
pub struct RejectedCommit{pub reason:SubmitError,pub changes:Vec<Change>}
#[derive(Clone,Debug)]
pub struct Ticket{id:u64,state:Arc<AtomicU8>}
impl Ticket{
 pub fn id(&self)->u64{self.id}
 /// Linearizes against worker claim. True means no commit will begin for this
 /// ticket; false means it may already be published and must not be rolled back.
 pub fn cancel(&self)->bool{match self.state.compare_exchange(QUEUED,CANCELLED,Ordering::AcqRel,Ordering::Acquire){Ok(_)=>true,Err(state)=>state==CANCELLED}}
}
#[derive(Debug)]
pub enum CompletionKind{Commit(Result<CommitReceipt,JournalError>),Checkpoint(Result<JournalMark,JournalError>),Cancelled}
#[derive(Debug)]
pub struct Completion{pub ticket:u64,pub kind:CompletionKind}
pub struct Shutdown{pub completions:Vec<Completion>,pub worker:Result<(),ServiceError>}
enum CommandKind{Commit{expected:Option<JournalMark>,changes:Vec<Change>},Checkpoint}
struct Command{ticket:Ticket,kind:CommandKind}
struct Reservation{bytes:usize,ticket:Ticket}
pub struct JournalService{sender:Option<SyncSender<Command>>,receiver:Receiver<Completion>,worker:Option<JoinHandle<Result<(),JournalError>>>,pending:BTreeMap<u64,Reservation>,bytes:usize,next:u64,options:ServiceOptions,limits:Limits,mark:JournalMark,lost:bool}
fn complete(sender:&mpsc::Sender<Completion>,ticket:&Ticket,kind:CompletionKind){ticket.state.store(FINISHED,Ordering::Release);let _=sender.send(Completion{ticket:ticket.id,kind});}
fn work(mut journal:Journal,receiver:Receiver<Command>,sender:mpsc::Sender<Completion>)->Result<(),JournalError>{
 let mut checkpoint:Option<(Ticket,JoinHandle<Result<PreparedCheckpoint,JournalError>>)>=None;let mut disconnected=false;
 loop{
  if checkpoint.as_ref().is_some_and(|(_,job)|job.is_finished()){
   let(ticket,job)=checkpoint.take().unwrap();let result=match job.join(){Ok(Ok(prepared))=>{let mark=prepared.mark();journal.finish_checkpoint(prepared).map(|_|mark)},Ok(Err(e))=>Err(e),Err(_)=>{journal.fail_stop();Err(JournalError::Stopped)}};complete(&sender,&ticket,CompletionKind::Checkpoint(result));
  }
  if disconnected{if checkpoint.is_none(){break;}thread::sleep(Duration::from_millis(5));continue;}
  let command=match receiver.recv_timeout(Duration::from_millis(5)){Ok(c)=>c,Err(RecvTimeoutError::Timeout)=>continue,Err(RecvTimeoutError::Disconnected)=>{disconnected=true;continue;}};
  if command.ticket.state.compare_exchange(QUEUED,CLAIMED,Ordering::AcqRel,Ordering::Acquire).is_err(){complete(&sender,&command.ticket,CompletionKind::Cancelled);continue;}
  match command.kind{
   CommandKind::Commit{expected,changes}=>{let result=journal.commit(expected.unwrap_or_else(||journal.mark()),&changes);complete(&sender,&command.ticket,CompletionKind::Commit(result));}
   CommandKind::Checkpoint=>{
    if checkpoint.is_some(){complete(&sender,&command.ticket,CompletionKind::Checkpoint(Err(JournalError::CheckpointBusy)));continue;}
    match journal.begin_checkpoint(){Ok(Some(job))=>match thread::Builder::new().name("infinite-checkpoint".into()).spawn(move||job.prepare()){Ok(worker)=>checkpoint=Some((command.ticket,worker)),Err(e)=>complete(&sender,&command.ticket,CompletionKind::Checkpoint(Err(e.into())))},Ok(None)=>complete(&sender,&command.ticket,CompletionKind::Checkpoint(Ok(journal.mark()))),Err(e)=>complete(&sender,&command.ticket,CompletionKind::Checkpoint(Err(e)))}
   }
  }
 }
 journal.close()
}
impl JournalService{
 pub fn start(journal:Journal,options:ServiceOptions)->Result<Self,ServiceError>{
  Self::start_inner(journal,options,||{})
 }
 fn start_inner(journal:Journal,options:ServiceOptions,before:impl FnOnce()+Send+'static)->Result<Self,ServiceError>{
  if options.outstanding==0||options.outstanding>1024||options.retained_bytes==0||options.retained_bytes>256*1024*1024{return Err(ServiceError::InvalidOptions);}
  let mark=journal.mark();let limits=journal.limits();let(sender,commands)=mpsc::sync_channel(options.outstanding);let(completions,receiver)=mpsc::channel();let worker=thread::Builder::new().name("infinite-journal".into()).spawn(move||{before();work(journal,commands,completions)}).map_err(ServiceError::Spawn)?;
  Ok(Self{sender:Some(sender),receiver,worker:Some(worker),pending:BTreeMap::new(),bytes:0,next:1,options,limits,mark,lost:false})
 }
 pub fn outstanding(&self)->usize{self.pending.len()}
 pub fn retained_bytes(&self)->usize{self.bytes}
 /// Latest commit confirmed by a polled receipt, not a speculative queue head.
 pub fn confirmed_mark(&self)->JournalMark{self.mark}
 fn admit(&self,bytes:usize)->Result<(),SubmitError>{if self.sender.is_none()||self.lost{return Err(SubmitError::Closed);}if self.next==u64::MAX{return Err(SubmitError::TicketExhausted);}if bytes>self.options.retained_bytes{return Err(SubmitError::Oversize);}if self.pending.len()==self.options.outstanding||bytes>self.options.retained_bytes-self.bytes{return Err(SubmitError::Full);}Ok(())}
 fn enqueue(&mut self,kind:CommandKind,bytes:usize)->Result<Ticket,(SubmitError,CommandKind)>{
  if let Err(e)=self.admit(bytes){return Err((e,kind));}let ticket=Ticket{id:self.next,state:Arc::new(AtomicU8::new(QUEUED))};let command=Command{ticket:ticket.clone(),kind};
  if let Err(e)=self.sender.as_ref().unwrap().try_send(command){return Err(match e{TrySendError::Full(c)=>(SubmitError::Full,c.kind),TrySendError::Disconnected(c)=>{self.lost=true;(SubmitError::Closed,c.kind)}});}
  self.next+=1;self.bytes+=bytes;self.pending.insert(ticket.id,Reservation{bytes,ticket:ticket.clone()});Ok(ticket)
 }
 /// None uses FIFO journal order. Some additionally checks an exact base mark.
 /// The host owns business reservations until receipt and must not expose asset
 /// grants on acceptance alone. Rejection returns the original immutable set.
 pub fn try_commit(&mut self,expected:Option<JournalMark>,changes:Vec<Change>)->Result<Ticket,RejectedCommit>{
  let mut bytes=changes.capacity().checked_mul(std::mem::size_of::<Change>()).and_then(|n|n.checked_add(128));let mut oversize=changes.len()>self.limits.max_transaction_records;
  for change in &changes{if let Some(value)=&change.value{oversize|=value.len()>self.limits.max_record_bytes;bytes=bytes.and_then(|n|n.checked_add(value.len()));}}
  if oversize||bytes.is_none(){return Err(RejectedCommit{reason:SubmitError::Oversize,changes});}
  self.enqueue(CommandKind::Commit{expected,changes},bytes.unwrap()).map_err(|(reason,kind)|{let CommandKind::Commit{changes,..}=kind else{unreachable!()};RejectedCommit{reason,changes}})
 }
 pub fn try_checkpoint(&mut self)->Result<Ticket,SubmitError>{self.enqueue(CommandKind::Checkpoint,128).map_err(|(e,_)|e)}
 pub fn try_poll(&mut self)->Result<Option<Completion>,ServiceError>{
  match self.receiver.try_recv(){Ok(completion)=>{let Some(reservation)=self.pending.remove(&completion.ticket)else{self.lost=true;return Err(ServiceError::WorkerLost);};self.bytes-=reservation.bytes;
   let committed=match &completion.kind{CompletionKind::Commit(Ok(r))=>Some(r.mark),CompletionKind::Commit(Err(JournalError::Published{mark,..}))=>Some(*mark),_=>None};if let Some(mark)=committed{if mark.sequence>self.mark.sequence{self.mark=mark;}}Ok(Some(completion))},Err(TryRecvError::Empty)=>Ok(None),Err(TryRecvError::Disconnected)=>{if self.pending.is_empty(){Ok(None)}else{self.lost=true;Err(ServiceError::WorkerLost)}}}
 }
 /// Blocking lifecycle operation, never a tick-path flush. Drain all accepted
 /// work and return every unpolled receipt even if the worker/close failed.
 pub fn shutdown(mut self)->Shutdown{
  self.sender.take();let mut worker=match self.worker.take().unwrap().join(){Ok(Ok(()))=>Ok(()),Ok(Err(e))=>Err(ServiceError::Journal(e)),Err(_)=>Err(ServiceError::WorkerLost)};let mut completions=Vec::new();loop{match self.try_poll(){Ok(Some(c))=>completions.push(c),Ok(None)=>break,Err(e)=>{worker=Err(e);break;}}}Shutdown{completions,worker}
 }
}
impl Drop for JournalService{fn drop(&mut self){if let Some(worker)=self.worker.take(){for p in self.pending.values(){p.ticket.cancel();}self.sender.take();let _=worker.join();}}}

#[cfg(test)]
mod tests{
 use super::*;use crate::{recover,JournalOptions,RecordKey};use std::{fs,path::PathBuf,sync::Barrier};
 #[test]
 fn cancellation_before_claim_is_definitive_and_keeps_unpolled_reservation(){
  let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m6/test-data");fs::create_dir_all(&root).unwrap();let stamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();let path=root.join(format!("cancel-{}-{stamp}",std::process::id()));let options=JournalOptions::default();let journal=Journal::create(&path,[9;16],options).unwrap();let gate=Arc::new(Barrier::new(2));let child=gate.clone();let mut service=JournalService::start_inner(journal,ServiceOptions{outstanding:2,retained_bytes:4096},move||{child.wait();}).unwrap();
  let ticket=service.try_commit(None,vec![Change{key:RecordKey{table:1,id:1},value:Some(b"never".as_slice().into())}]).unwrap();let checkpoint=service.try_checkpoint().unwrap();assert!(ticket.cancel());assert!(ticket.cancel());assert!(checkpoint.cancel());assert_eq!(service.outstanding(),2);let denied=service.try_commit(None,Vec::new()).unwrap_err();assert_eq!(denied.reason,SubmitError::Full);gate.wait();let result=service.shutdown();result.worker.unwrap();assert_eq!(result.completions.len(),2);assert!(result.completions.iter().all(|r|matches!(r.kind,CompletionKind::Cancelled)));assert_eq!(recover(path,options).unwrap().mark.sequence,0);
 }
 #[test]
 fn completed_unpolled_receipt_still_blocks_admission(){
  let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m6/test-data");fs::create_dir_all(&root).unwrap();let stamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();let path=root.join(format!("unpolled-{}-{stamp}",std::process::id()));let journal=Journal::create(&path,[10;16],JournalOptions::default()).unwrap();let mut s=JournalService::start(journal,ServiceOptions{outstanding:1,retained_bytes:1024}).unwrap();let ticket=s.try_commit(None,Vec::new()).unwrap();let until=std::time::Instant::now()+Duration::from_secs(10);while ticket.state.load(Ordering::Acquire)!=FINISHED{assert!(std::time::Instant::now()<until);thread::sleep(Duration::from_millis(1));}assert_eq!(s.outstanding(),1);assert_eq!(s.try_commit(None,Vec::new()).unwrap_err().reason,SubmitError::Full);assert!(matches!(s.try_poll().unwrap().unwrap().kind,CompletionKind::Commit(Ok(_))));s.shutdown().worker.unwrap();
 }
}
