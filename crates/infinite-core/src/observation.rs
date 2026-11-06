//! Shared retained frames with fair, budgeted per-recipient observation.
use std::{collections::{HashMap,HashSet,VecDeque},sync::Arc};
use crate::{CombatConnections,CombatFrame,CombatSyncError,ConnectionId,NetworkBudget,NetworkError,ViewRect,MAX_NETWORK_CONNECTIONS,MAX_NETWORK_WORK};
pub const MAX_OBSERVATION_FRAMES:usize=16;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ObservationTarget {pub connection:ConnectionId,pub view:ViewRect}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ObservationError {InvalidCapacity,Full,NoFrame,FrameGap,DuplicateConnection,InvalidView,TooManyConnections,Network(NetworkError),InvalidBudget,RecoveryPending}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ObservationFailure {pub connection:ConnectionId,pub serial:u64,pub error:NetworkError}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct ObservationReport {pub visited:usize,pub observed:usize,pub backpressured:usize,pub failed:usize,pub pending:usize,pub frames:usize}
// This wrapper never exposes mutable access to the copy sharing geometry ID.
struct Frozen {frame:CombatFrame}
struct Work {frame:Arc<Frozen>,view:ViewRect}
struct Retained {serial:u64,frame:Arc<Frozen>,pending:usize}
/// Frames removed from the shared queue are transferred intact, not dropped.
/// The host owns their memory until replay/recovery finishes. Keep this owner
/// bounded too; extraction is not permission to accumulate unlimited journals.
pub struct ObservationRecovery {connection:ConnectionId,work:VecDeque<Work>,last_serial:u64}
/// The old subscription delivered through last_reserved_serial. Frames after
/// that boundary were NOT subscribed to. A new replica and explicit host
/// notification are required before observing a current frame on new_stream.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ObservationRestart {pub connection:ConnectionId,pub last_reserved_serial:u64,pub old_stream:u64,pub new_stream:u64}
impl ObservationRecovery {
    pub fn connection(&self)->ConnectionId{self.connection}
    pub fn len(&self)->usize{self.work.len()}
    pub fn is_empty(&self)->bool{self.work.is_empty()}
    pub fn first_serial(&self)->Option<u64>{self.work.front().map(|w|w.frame.frame.serial())}
    pub fn last_reserved_serial(&self)->u64{self.last_serial}
    /// End a drained subscription, never silently jump its event history.
    /// The host excludes this connection from new admissions while recovering,
    /// drains old ACKs, then detaches and clears the old transport decoder.
    /// Failure returns ownership of every remaining frame to the caller.
    pub fn try_restart(self,net:&mut CombatConnections)->Result<ObservationRestart,(Self,ObservationError)>{
        if !self.work.is_empty(){return Err((self,ObservationError::RecoveryPending));}
        let old_stream=match net.status(self.connection){Ok(s)=>s.stream,Err(e)=>return Err((self,ObservationError::Network(e)))};
        match net.reset_replica(self.connection){
            Ok(new_stream)=>Ok(ObservationRestart{connection:self.connection,last_reserved_serial:self.last_serial,old_stream,new_stream}),
            Err(e)=>Err((self,ObservationError::Network(e))),
        }
    }
    pub fn replay(&mut self,net:&mut CombatConnections,budget:&mut NetworkBudget)->Result<usize,ObservationError>{
        if budget.work>MAX_NETWORK_WORK{return Err(ObservationError::InvalidBudget);}let mut count=0;
        while budget.work>0{let Some(work)=self.work.front()else{break;};budget.work-=1;net.observe(self.connection,&work.frame.frame,work.view).map_err(ObservationError::Network)?;self.work.pop_front();count+=1;}Ok(count)
    }
}
/// One queue per captured world stream. Each recipient's frames are observed in
/// capture order; a blocked recipient rotates behind the others. The host must
/// poll before filling the bounded journal, and explicitly extract/recover a
/// persistently slow subscriber before admitting more than capacity frames.
pub struct ObservationQueue {
    capacity:usize,last_serial:u64,frames:VecDeque<Retained>,
    pending:HashMap<ConnectionId,VecDeque<Work>>,ready:VecDeque<ConnectionId>,work_count:usize,
}
impl ObservationQueue {
    pub fn new(capacity:usize)->Result<Self,ObservationError>{
        if capacity==0||capacity>MAX_OBSERVATION_FRAMES{return Err(ObservationError::InvalidCapacity);}
        Ok(Self{capacity,last_serial:0,frames:VecDeque::new(),pending:HashMap::new(),ready:VecDeque::new(),work_count:0})
    }
    pub fn pending(&self)->usize{self.work_count}
    pub fn retained_frames(&self)->usize{self.frames.len()}
    pub fn oldest_serial(&self)->Option<u64>{self.frames.front().map(|f|f.serial)}
    pub fn enqueue(&mut self,net:&CombatConnections,frame:&CombatFrame,targets:&[ObservationTarget])->Result<(),ObservationError>{
        let serial=frame.serial();if serial==0{return Err(ObservationError::NoFrame);}if self.last_serial!=0&&self.last_serial.checked_add(1)!=Some(serial){return Err(ObservationError::FrameGap);}
        if self.frames.len()==self.capacity{return Err(ObservationError::Full);}if targets.len()>MAX_NETWORK_CONNECTIONS{return Err(ObservationError::TooManyConnections);}
        let mut seen=HashSet::with_capacity(targets.len());let mut new=0;
        for target in targets{
            net.status(target.connection).map_err(ObservationError::Network)?;
            if !seen.insert(target.connection){return Err(ObservationError::DuplicateConnection);}if target.view.min.x>target.view.max.x||target.view.min.y>target.view.max.y{return Err(ObservationError::InvalidView);}
            new+=usize::from(!self.pending.contains_key(&target.connection));
        }
        if self.pending.len()+new>MAX_NETWORK_CONNECTIONS{return Err(ObservationError::TooManyConnections);}
        // Admission is atomic; the one shared copy is made only after validation.
        if !targets.is_empty(){let frozen=Arc::new(Frozen{frame:frame.frozen_copy()});self.frames.push_back(Retained{serial,frame:frozen.clone(),pending:targets.len()});
            for target in targets{let q=self.pending.entry(target.connection).or_insert_with(||{self.ready.push_back(target.connection);VecDeque::new()});q.push_back(Work{frame:frozen.clone(),view:target.view});self.work_count+=1;}
        }
        self.last_serial=serial;Ok(())
    }
    fn finished(&mut self,serial:u64){
        let frame=self.frames.iter_mut().find(|f|f.serial==serial).expect("retained work frame");frame.pending-=1;self.work_count-=1;
    }
    fn reclaim(&mut self){self.frames.retain(|f|f.pending!=0);}
    pub fn poll(&mut self,net:&mut CombatConnections,budget:&mut NetworkBudget,failures:&mut Vec<ObservationFailure>)->Result<ObservationReport,ObservationError>{
        if budget.work>MAX_NETWORK_WORK{return Err(ObservationError::InvalidBudget);}failures.clear();let mut report=ObservationReport::default();let visits=budget.work.min(self.ready.len());
        for _ in 0..visits{
            let id=self.ready.pop_front().unwrap();budget.work-=1;report.visited+=1;let q=self.pending.get_mut(&id).unwrap();let work=q.front().unwrap();let serial=work.frame.frame.serial();
            let result=net.observe(id,&work.frame.frame,work.view);let completed=result.is_ok();
            match result{Ok(())=>{q.pop_front();report.observed+=1;},Err(error)=>{if error==NetworkError::Sync(CombatSyncError::EventBackpressure){report.backpressured+=1;}else{report.failed+=1;}failures.push(ObservationFailure{connection:id,serial,error});}}
            if q.is_empty(){self.pending.remove(&id);}else{self.ready.push_back(id);}if completed{self.finished(serial);}
        }
        self.reclaim();report.pending=self.work_count;report.frames=self.frames.len();Ok(report)
    }
    /// Isolate a failed subscriber's backlog while preserving the full frames
    /// needed to retry. Its sender must also be retained. Other subscribers can
    /// continue; the host explicitly decides future subscription/recovery policy.
    pub fn extract(&mut self,id:ConnectionId)->Option<ObservationRecovery>{
        let work=self.pending.remove(&id)?;let last_serial=work.back().unwrap().frame.frame.serial();self.ready.retain(|&p|p!=id);for item in &work{self.finished(item.frame.frame.serial());}self.reclaim();Some(ObservationRecovery{connection:id,work,last_serial})
    }
    /// Scoped retained frame count for diagnostics; extraction moves that memory
    /// to the returned recovery owner rather than freeing it immediately.
    pub fn shared_frame_references(&self)->usize{self.frames.iter().map(|f|Arc::strong_count(&f.frame)).sum()}
}
