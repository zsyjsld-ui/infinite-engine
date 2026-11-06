use std::{collections::BTreeMap, fs::{self,File,OpenOptions}, io::{self,Read,Write}, path::{Path,PathBuf}, sync::{Arc,atomic::{AtomicBool,Ordering}}};
use crate::{Change,FrameKind,JournalMark,Limits,RecordKey,StoreError,decode_frame,encode_checkpoint,encode_transaction};

pub type Records=BTreeMap<RecordKey,Arc<[u8]>>;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum IoStage{BeforeCreate,AfterWrite,AfterFileSync,BeforePublish,AfterPublish,BeforeDirectorySync,BeforePrune,AfterPrune,BeforeCleanup}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
#[cfg(feature="fault-injection")]
pub struct IoPoint{pub stage:IoStage,pub kind:FrameKind,pub sequence:u64}
#[cfg(feature="fault-injection")]
pub type FaultHook=Arc<dyn Fn(IoPoint)->io::Result<()>+Send+Sync>;
fn point(lock:&WriterLock,kind:FileKind,sequence:u64,stage:IoStage)->io::Result<()>{
 #[cfg(feature="fault-injection")]{let hook=lock.hook.lock().unwrap().clone();if let Some(hook)=hook{return hook(IoPoint{stage,sequence,kind:if kind==FileKind::Transaction{FrameKind::Transaction}else{FrameKind::Checkpoint}});}}
 let _=(lock,kind,sequence,stage);Ok(())
}
#[derive(Clone,Copy,Debug)]
pub struct JournalOptions {pub limits:Limits,pub max_log_files:usize,pub max_log_bytes:u64,pub max_live_bytes:usize}
impl Default for JournalOptions {fn default()->Self{Self{limits:Limits::default(),max_log_files:4096,max_log_bytes:256*1024*1024,max_live_bytes:128*1024*1024}}}
impl JournalOptions {fn validate(self)->Result<Self,JournalError>{self.limits.validate()?;let max=Self::default();if self.max_log_files==0||self.max_log_files>max.max_log_files||self.max_log_bytes==0||self.max_log_bytes>max.max_log_bytes||self.max_live_bytes==0||self.max_live_bytes>max.max_live_bytes{return Err(JournalError::InvalidOptions);}Ok(self)}}
#[derive(Debug)]
pub enum JournalError {Codec(StoreError),Io(io::Error),InvalidOptions,ForeignEntry,Locked,MissingCheckpoint,HighWater,LiveLimit,Stopped,CheckpointBusy,ForeignCheckpoint,
    /// The visible commit marker exists. Never retry/roll back as if nothing
    /// committed; stop the application and recover this history.
    Published{mark:JournalMark,error:io::Error},
    /// Failure to remove an owned incomplete file latches the writer too.
    Cleanup{primary:io::Error,cleanup:io::Error},
}
impl From<StoreError> for JournalError {fn from(e:StoreError)->Self{Self::Codec(e)}}
impl From<io::Error> for JournalError {fn from(e:io::Error)->Self{Self::Io(e)}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Durability {DirectorySynced,ProcessCrashOnly}
#[derive(Clone,Copy,Debug)]
pub struct CommitReceipt {pub mark:JournalMark,pub bytes:usize,pub durability:Durability}
pub struct Recovered {pub mark:JournalMark,pub records:Records,pub live_bytes:usize,pub replayed:usize}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum FileKind{Transaction,Checkpoint,Temporary}
struct Entry{path:PathBuf,sequence:u64,kind:FileKind,bytes:u64}
fn file_name(sequence:u64,kind:FileKind)->String{format!("{sequence:020}.{}",if kind==FileKind::Transaction{"txn"}else{"checkpoint"})}
fn entries(directory:&Path,options:JournalOptions)->Result<Vec<Entry>,JournalError>{
 let metadata=fs::symlink_metadata(directory)?;if !metadata.is_dir()||metadata.file_type().is_symlink(){return Err(JournalError::ForeignEntry);}
 let mut result=Vec::new();let(mut log_count,mut log_bytes,mut snapshots,mut temps)=(0usize,0u64,0usize,0usize);
 for item in fs::read_dir(directory)? {let item=item?;let metadata=fs::symlink_metadata(item.path())?;if !metadata.is_file()||metadata.file_type().is_symlink(){return Err(JournalError::ForeignEntry);}let name=item.file_name();let name=name.to_str().ok_or(JournalError::ForeignEntry)?;if name=="writer.lock" {if metadata.len()>128{return Err(JournalError::ForeignEntry);}continue;}
  let(temp,base)=name.strip_suffix(".tmp").map_or((false,name),|n|(true,n));let(seq,extension)=base.split_once('.').ok_or(JournalError::ForeignEntry)?;if seq.len()!=20||!seq.bytes().all(|c|c.is_ascii_digit()){return Err(JournalError::ForeignEntry);}let sequence=seq.parse::<u64>().map_err(|_|JournalError::ForeignEntry)?;
  let kind=match extension{"txn"=>FileKind::Transaction,"checkpoint"=>FileKind::Checkpoint,_=>return Err(JournalError::ForeignEntry)};if kind==FileKind::Transaction&&sequence==0{return Err(JournalError::ForeignEntry);}let limit=if kind==FileKind::Transaction{options.limits.max_transaction_bytes}else{options.limits.max_checkpoint_bytes};if metadata.len()>limit as u64{return Err(JournalError::HighWater);}
  if temp{temps+=1;}else if kind==FileKind::Transaction{log_count+=1;log_bytes=log_bytes.checked_add(metadata.len()).ok_or(JournalError::HighWater)?;}else{snapshots+=1;}
  if temps>2||snapshots>2||log_count>options.max_log_files||log_bytes>options.max_log_bytes{return Err(JournalError::HighWater);}
  result.push(Entry{path:item.path(),sequence,kind:if temp{FileKind::Temporary}else{kind},bytes:metadata.len()});
 }
 result.sort_unstable_by_key(|e|e.sequence);Ok(result)
}
fn read_frame(path:&Path,limit:usize)->Result<Vec<u8>,JournalError>{let mut f=File::open(path)?;let mut bytes=Vec::new();Read::by_ref(&mut f).take(limit as u64+1).read_to_end(&mut bytes)?;if bytes.len()>limit{return Err(JournalError::HighWater);}Ok(bytes)}
fn next_size(records:&Records,live:usize,changes:&[Change],options:JournalOptions)->Result<usize,JournalError>{let mut count=records.len();let mut size=live;for change in changes{if let Some(old)=records.get(&change.key){size-=old.len();count-=1;}if let Some(value)=&change.value{size=size.checked_add(value.len()).ok_or(JournalError::LiveLimit)?;count+=1;}}if count>options.limits.max_records||size>options.max_live_bytes{return Err(JournalError::LiveLimit);}Ok(size)}
fn apply(records:&mut Records,changes:&[Change]){for change in changes{if let Some(value)=&change.value{records.insert(change.key,Arc::clone(value));}else{records.remove(&change.key);}}}
/// Read-only, all-or-error recovery. A truncated published transaction is
/// corruption, not an ignorable tail. Only precisely named .tmp files are ignored.
pub fn recover(directory:impl AsRef<Path>,options:JournalOptions)->Result<Recovered,JournalError>{
 let options=options.validate()?;let list=entries(directory.as_ref(),options)?;let checkpoint=list.iter().filter(|e|e.kind==FileKind::Checkpoint).max_by_key(|e|e.sequence).ok_or(JournalError::MissingCheckpoint)?;
 let bytes=read_frame(&checkpoint.path,options.limits.max_checkpoint_bytes)?;let frame=decode_frame(&bytes,options.limits)?;if frame.kind!=FrameKind::Checkpoint||frame.sequence!=checkpoint.sequence{return Err(JournalError::ForeignEntry);}let mut mark=frame.checkpoint_mark()?;let mut records=Records::new();let mut live=next_size(&records,0,&frame.changes,options)?;apply(&mut records,&frame.changes);let mut replayed=0;
 for e in list.iter().filter(|e|e.kind==FileKind::Transaction&&e.sequence>checkpoint.sequence){let bytes=read_frame(&e.path,options.limits.max_transaction_bytes)?;let frame=decode_frame(&bytes,options.limits)?;if frame.kind!=FrameKind::Transaction||frame.sequence!=e.sequence{return Err(JournalError::ForeignEntry);}let next=frame.successor(mark)?;live=next_size(&records,live,&frame.changes,options)?;apply(&mut records,&frame.changes);mark=next;replayed+=1;}
 Ok(Recovered{mark,records,live_bytes:live,replayed})
}
fn sync_directory(path:&Path)->io::Result<Durability>{
 #[cfg(unix)] {File::open(path)?.sync_all()?;Ok(Durability::DirectorySynced)}
 #[cfg(not(unix))] {let _=path;Ok(Durability::ProcessCrashOnly)}
}
struct WriterLock{path:PathBuf,owned:bool,cleanup_failed:AtomicBool,#[cfg(feature="fault-injection")]hook:std::sync::Mutex<Option<FaultHook>>}
impl WriterLock{fn acquire(directory:&Path)->Result<Self,JournalError>{let path=directory.join("writer.lock");let mut file=OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e|if e.kind()==io::ErrorKind::AlreadyExists{JournalError::Locked}else{JournalError::Io(e)})?;let lock=Self{path,owned:true,cleanup_failed:AtomicBool::new(false),#[cfg(feature="fault-injection")]hook:std::sync::Mutex::new(None)};write!(file,"{}",std::process::id())?;file.sync_all()?;Ok(lock)}fn release(&mut self)->io::Result<()>{if self.owned{fs::remove_file(&self.path)?;self.owned=false;}Ok(())}}
impl Drop for WriterLock{fn drop(&mut self){let _=self.release();}}
fn publish(lock:&WriterLock,directory:&Path,sequence:u64,kind:FileKind,bytes:&[u8],mark:JournalMark)->Result<Durability,JournalError>{
 let target=directory.join(file_name(sequence,kind));if target.try_exists()?{return Err(JournalError::ForeignEntry);}let temporary=directory.join(format!("{}.tmp",file_name(sequence,kind)));
 point(lock,kind,sequence,IoStage::BeforeCreate)?;let mut file=OpenOptions::new().write(true).create_new(true).open(&temporary)?;
 let result=(||{file.write_all(bytes)?;point(lock,kind,sequence,IoStage::AfterWrite)?;file.sync_all()?;point(lock,kind,sequence,IoStage::AfterFileSync)?;drop(file);point(lock,kind,sequence,IoStage::BeforePublish)?;fs::rename(&temporary,&target)})();// No overwrite under the exclusive writer lock.
 if let Err(primary)=result{if let Err(cleanup)=point(lock,kind,sequence,IoStage::BeforeCleanup).and_then(|_|fs::remove_file(&temporary)){return Err(JournalError::Cleanup{primary,cleanup});}return Err(JournalError::Io(primary));}
 point(lock,kind,sequence,IoStage::AfterPublish).and_then(|_|point(lock,kind,sequence,IoStage::BeforeDirectorySync)).and_then(|_|sync_directory(directory)).map_err(|error|JournalError::Published{mark,error})
}
/// An immutable committed snapshot. Its lease keeps the exclusive writer lock
/// alive even if the caller drops the Journal while this job is running.
pub struct CheckpointJob {directory:PathBuf,lock:Arc<WriterLock>,mark:JournalMark,records:Vec<Change>,limits:Limits}
pub struct PreparedCheckpoint {temporary:Option<PathBuf>,target:PathBuf,lock:Arc<WriterLock>,mark:JournalMark}
impl CheckpointJob {
 pub fn mark(&self)->JournalMark{self.mark}
 /// Encode and fsync on a checkpoint worker. Does not publish or prune logs.
 pub fn prepare(self)->Result<PreparedCheckpoint,JournalError>{
  let bytes=encode_checkpoint(self.mark,&self.records,self.limits)?;let target=self.directory.join(file_name(self.mark.sequence,FileKind::Checkpoint));if target.try_exists()?{return Err(JournalError::ForeignEntry);}
  let temporary=self.directory.join(format!("{}.tmp",file_name(self.mark.sequence,FileKind::Checkpoint)));
  point(&self.lock,FileKind::Checkpoint,self.mark.sequence,IoStage::BeforeCreate)?;let mut file=OpenOptions::new().write(true).create_new(true).open(&temporary)?;
  let result=file.write_all(&bytes).and_then(|_|point(&self.lock,FileKind::Checkpoint,self.mark.sequence,IoStage::AfterWrite)).and_then(|_|file.sync_all()).and_then(|_|point(&self.lock,FileKind::Checkpoint,self.mark.sequence,IoStage::AfterFileSync));drop(file);
  if let Err(primary)=result{if let Err(cleanup)=point(&self.lock,FileKind::Checkpoint,self.mark.sequence,IoStage::BeforeCleanup).and_then(|_|fs::remove_file(&temporary)){self.lock.cleanup_failed.store(true,Ordering::Release);return Err(JournalError::Cleanup{primary,cleanup});}return Err(JournalError::Io(primary));}
  Ok(PreparedCheckpoint{temporary:Some(temporary),target,lock:self.lock,mark:self.mark})
 }
}
impl PreparedCheckpoint{pub fn mark(&self)->JournalMark{self.mark}}
impl Drop for PreparedCheckpoint{fn drop(&mut self){if let Some(path)=&self.temporary{if point(&self.lock,FileKind::Checkpoint,self.mark.sequence,IoStage::BeforeCleanup).and_then(|_|fs::remove_file(path)).is_err(){self.lock.cleanup_failed.store(true,Ordering::Release);}}}}
pub struct Journal {directory:PathBuf,options:JournalOptions,lock:Arc<WriterLock>,mark:JournalMark,records:Records,live_bytes:usize,logs:BTreeMap<u64,u64>,log_bytes:u64,checkpoint_sequence:u64,stopped:bool}
impl Journal {
 #[cfg(feature="fault-injection")]
 pub fn set_fault_hook(&mut self,hook:Option<FaultHook>){*self.lock.hook.lock().unwrap()=hook;}
 /// Creates a new directory exclusively; existing saves are never initialized
 /// over. The parent must already exist and be application-owned.
 pub fn create(directory:impl AsRef<Path>,history:[u8;16],options:JournalOptions)->Result<Self,JournalError>{Self::create_with_records(directory,history,Records::new(),options)}
 /// Seed a new synthetic/application-owned store with one initial checkpoint.
 /// This is not a migration/import of an existing game's save format.
 pub fn create_with_records(directory:impl AsRef<Path>,history:[u8;16],records:Records,options:JournalOptions)->Result<Self,JournalError>{
  let options=options.validate()?;let mark=JournalMark::initial(history)?;
  let changes:Vec<_>=records.iter().map(|(&key,value)|Change{key,value:Some(Arc::clone(value))}).collect();
  let live_bytes=next_size(&Records::new(),0,&changes,options)?;let bytes=encode_checkpoint(mark,&changes,options.limits)?;
  let directory=directory.as_ref();fs::create_dir(directory)?;let absolute=fs::canonicalize(directory)?;let directory=absolute.as_path();let lock=Arc::new(WriterLock::acquire(directory)?);
  publish(&lock,directory,0,FileKind::Checkpoint,&bytes,mark)?;if let Some(parent)=directory.parent(){sync_directory(parent)?;}
  Ok(Self{directory:directory.to_owned(),options,lock,mark,records,live_bytes,logs:BTreeMap::new(),log_bytes:0,checkpoint_sequence:0,stopped:false})
 } /// A stale writer.lock is not automatically stolen. After process failure the
 /// operator must establish that its owner has exited before removing that lock.
 pub fn open(directory:impl AsRef<Path>,options:JournalOptions)->Result<Self,JournalError>{let options=options.validate()?;let directory=directory.as_ref();let metadata=fs::symlink_metadata(directory)?;if !metadata.is_dir()||metadata.file_type().is_symlink(){return Err(JournalError::ForeignEntry);}let absolute=fs::canonicalize(directory)?;let directory=absolute.as_path();let lock=Arc::new(WriterLock::acquire(directory)?);let recovered=recover(directory,options)?;let list=entries(directory,options)?;let checkpoint_sequence=list.iter().filter(|e|e.kind==FileKind::Checkpoint).map(|e|e.sequence).max().unwrap();let mut logs=BTreeMap::new();let mut log_bytes=0;for e in list {if e.kind==FileKind::Temporary||(e.kind==FileKind::Transaction&&e.sequence<=checkpoint_sequence)||(e.kind==FileKind::Checkpoint&&e.sequence<checkpoint_sequence){fs::remove_file(e.path)?;}else if e.kind==FileKind::Transaction{log_bytes+=e.bytes;logs.insert(e.sequence,e.bytes);}}sync_directory(directory)?;Ok(Self{directory:directory.to_owned(),options,lock,mark:recovered.mark,records:recovered.records,live_bytes:recovered.live_bytes,logs,log_bytes,checkpoint_sequence,stopped:false})}
 pub fn mark(&self)->JournalMark{self.mark}
 pub fn records(&self)->&Records{&self.records}
 pub fn retained_log_bytes(&self)->u64{self.log_bytes}
 pub fn retained_log_files(&self)->usize{self.logs.len()}
 pub fn stopped(&self)->bool{self.stopped||self.lock.cleanup_failed.load(Ordering::Acquire)}
 pub(crate) fn limits(&self)->Limits{self.options.limits}
 pub(crate) fn fail_stop(&mut self){self.stopped=true;}
 pub fn commit(&mut self,expected:JournalMark,changes:&[Change])->Result<CommitReceipt,JournalError>{
  if self.stopped(){return Err(JournalError::Stopped);}if expected!=self.mark{return Err(StoreError::ChainMismatch.into());}
  let(bytes,next)=encode_transaction(self.mark,changes,self.options.limits)?;let live=next_size(&self.records,self.live_bytes,changes,self.options)?;if self.logs.len()>=self.options.max_log_files||self.log_bytes+bytes.len()as u64>self.options.max_log_bytes{return Err(JournalError::HighWater);}
  let result=publish(&self.lock,&self.directory,next.sequence,FileKind::Transaction,&bytes,next);
  let published=result.is_ok()||matches!(result,Err(JournalError::Published{..}));if published{apply(&mut self.records,changes);self.live_bytes=live;self.mark=next;self.logs.insert(next.sequence,bytes.len()as u64);self.log_bytes+=bytes.len()as u64;}
  match result{Ok(durability)=>Ok(CommitReceipt{mark:next,bytes:bytes.len(),durability}),Err(e)=>{self.stopped=true;Err(e)}}
 }
 /// Capture only committed records. There may be one outstanding preparation;
 /// later commits can proceed while another thread encodes/writes this snapshot.
 pub fn begin_checkpoint(&mut self)->Result<Option<CheckpointJob>,JournalError>{
  if self.stopped(){return Err(JournalError::Stopped);}if Arc::strong_count(&self.lock)!=1{return Err(JournalError::CheckpointBusy);}
  if self.checkpoint_sequence==self.mark.sequence&&self.logs.is_empty(){return Ok(None);}
  let records=self.records.iter().map(|(&key,value)|Change{key,value:Some(Arc::clone(value))}).collect();
  Ok(Some(CheckpointJob{directory:self.directory.clone(),lock:Arc::clone(&self.lock),mark:self.mark,records,limits:self.options.limits}))
 }
 /// Publish on the serialized journal owner. An older captured sequence is
 /// valid: prune only its prefix, never transactions committed during prepare.
 pub fn finish_checkpoint(&mut self,mut prepared:PreparedCheckpoint)->Result<(),JournalError>{
  if !Arc::ptr_eq(&self.lock,&prepared.lock){return Err(JournalError::ForeignCheckpoint);}
  if self.stopped(){return Err(JournalError::Stopped);}
  if prepared.mark.history!=self.mark.history||prepared.mark.sequence>self.mark.sequence||prepared.mark.sequence<=self.checkpoint_sequence{return Err(JournalError::ForeignCheckpoint);}
  if prepared.target.try_exists()?{self.stopped=true;return Err(JournalError::ForeignEntry);}
  if let Err(e)=point(&self.lock,FileKind::Checkpoint,prepared.mark.sequence,IoStage::BeforePublish).and_then(|_|fs::rename(prepared.temporary.as_ref().unwrap(),&prepared.target)){self.stopped=true;return Err(e.into());}
  prepared.temporary=None;
  if let Err(error)=point(&self.lock,FileKind::Checkpoint,prepared.mark.sequence,IoStage::AfterPublish).and_then(|_|point(&self.lock,FileKind::Checkpoint,prepared.mark.sequence,IoStage::BeforeDirectorySync)).and_then(|_|sync_directory(&self.directory)){self.stopped=true;return Err(JournalError::Published{mark:prepared.mark,error});}
  self.checkpoint_sequence=prepared.mark.sequence;
  let cleanup=(||->Result<(),JournalError>{point(&self.lock,FileKind::Checkpoint,self.checkpoint_sequence,IoStage::BeforePrune)?;for e in entries(&self.directory,self.options)?{if(e.kind==FileKind::Transaction&&e.sequence<=self.checkpoint_sequence)||(e.kind==FileKind::Checkpoint&&e.sequence<self.checkpoint_sequence){fs::remove_file(&e.path)?;point(&self.lock,FileKind::Checkpoint,self.checkpoint_sequence,IoStage::AfterPrune)?;}}sync_directory(&self.directory)?;Ok(())})();
  if let Err(e)=cleanup{self.stopped=true;return Err(e);}
  self.logs.retain(|&sequence,_|sequence>self.checkpoint_sequence);self.log_bytes=self.logs.values().sum();Ok(())
 }
 pub fn checkpoint(&mut self)->Result<(),JournalError>{
  if let Some(job)=self.begin_checkpoint()?{let prepared=job.prepare()?;self.finish_checkpoint(prepared)?;}Ok(())
 }
 pub fn close(self)->Result<(),JournalError>{let mut lock=Arc::try_unwrap(self.lock).map_err(|_|JournalError::CheckpointBusy)?;lock.release()?;Ok(())}
}
