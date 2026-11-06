//! Indexed tick deadlines: bounded storage, cancellation without tombstones.
use std::num::NonZeroU32;
use crate::Tick;
pub const MAX_TICK_TASKS: usize = 65_536;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
pub struct TickTaskId { pub index:u32, pub generation:u32 }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum TickQueueError { InvalidCapacity, Full, SequenceExhausted, InvalidTask }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct TickTask<T> { pub id:TickTaskId, pub due:Tick, pub value:T, order:u64 }
struct Slot<T> { generation:u32, heap:Option<NonZeroU32>, task:Option<TickTask<T>> }
#[derive(Clone,Copy)]
struct Deadline { index:u32, due:Tick, order:u64 }
pub(crate) enum TickEdit<T> { Cancel(TickTaskId), Replace(TickTaskId,Tick,T) }
// Heap repairs move only compact deadlines. Larger cast/effect payloads remain
// in stable slots instead of being copied at every sift step.
pub struct TickQueue<T> { heap:Vec<Deadline>, slots:Vec<Slot<T>>, free:Vec<u32>, capacity:usize, sequence:u64 }
impl<T> TickQueue<T> {
    pub fn new(capacity:usize)->Result<Self,TickQueueError>{
        if capacity==0||capacity>MAX_TICK_TASKS{return Err(TickQueueError::InvalidCapacity);}
        Ok(Self{heap:Vec::new(),slots:Vec::new(),free:Vec::new(),capacity,sequence:0})
    }
    pub fn len(&self)->usize{self.heap.len()}
    pub fn is_empty(&self)->bool{self.heap.is_empty()}
    pub fn peek(&self)->Option<&TickTask<T>>{self.slots[self.heap.first()?.index as usize].task.as_ref()}
    pub fn get(&self,id:TickTaskId)->Option<&TickTask<T>>{self.row(id)?;self.slots[id.index as usize].task.as_ref()}
    pub fn check_schedule(&self)->Result<(),TickQueueError>{
        if self.heap.len()==self.capacity||(self.free.is_empty()&&self.slots.len()==self.capacity){return Err(TickQueueError::Full);}
        self.sequence.checked_add(1).ok_or(TickQueueError::SequenceExhausted)?;Ok(())
    }
    pub fn replace_value(&mut self,id:TickTaskId,value:T)->Result<T,TickQueueError>{
        self.row(id).ok_or(TickQueueError::InvalidTask)?;
        Ok(std::mem::replace(&mut self.slots[id.index as usize].task.as_mut().unwrap().value,value))
    }
    pub fn schedule(&mut self,due:Tick,value:T)->Result<TickTaskId,TickQueueError>{
        self.check_schedule()?;
        let order=self.sequence.checked_add(1).ok_or(TickQueueError::SequenceExhausted)?;
        let index=if let Some(index)=self.free.pop(){index}else{let index=self.slots.len()as u32;self.slots.push(Slot{generation:0,heap:None,task:None});index};
        let id=TickTaskId{index,generation:self.slots[index as usize].generation};
        let row=self.heap.len();self.heap.push(Deadline{index,due,order});self.slots[index as usize].task=Some(TickTask{id,due,value,order});self.slots[index as usize].heap=NonZeroU32::new((row+1)as u32);
        self.sequence=order;self.up(row);Ok(id)
    }
    /// Rescheduling retains acceptance order for equal deadlines.
    pub fn reschedule(&mut self,id:TickTaskId,due:Tick)->Result<(),TickQueueError>{
        let row=self.row(id).ok_or(TickQueueError::InvalidTask)?;self.heap[row].due=due;self.slots[id.index as usize].task.as_mut().unwrap().due=due;self.repair(row);Ok(())
    }
    pub fn cancel(&mut self,id:TickTaskId)->Option<TickTask<T>>{let row=self.row(id)?;Some(self.remove(row))}
    pub fn pop_due(&mut self,now:Tick)->Option<TickTask<T>>{
        if self.heap.first()?.due>now{return None;}Some(self.remove(0))
    }
    /// Dense, current-tick cohort only. Catch-up must continue using the heap
    /// because a repeated pulse can interleave with another existing deadline.
    pub(crate) fn current_batch(&self,now:Tick,limit:usize,output:&mut Vec<TickTaskId>)->bool{
        if limit<128||self.heap.len()<128||self.heap.iter().take(64).any(|d|d.due!=now){return false;}
        // A full cohort retains acceptance order across uniform periodic ticks.
        // Generation/deadline checks reject equal-count cancel-and-reuse changes.
        if output.len()==self.heap.len()&&output.len()<=limit&&output.iter().all(|id|self.get(*id).is_some_and(|t|t.due==now)){return true;}
        output.clear();
        self.collect_current(0,now,output);
        if output.len()<128{return false;}
        output.sort_unstable_by_key(|id|self.slots[id.index as usize].task.as_ref().unwrap().order);
        output.truncate(limit);true
    }
    fn collect_current(&self,row:usize,now:Tick,output:&mut Vec<TickTaskId>){
        if let Some(d)=self.heap.get(row).filter(|d|d.due==now){
            output.push(self.slots[d.index as usize].task.as_ref().unwrap().id);
            self.collect_current(row*2+1,now,output);self.collect_current(row*2+2,now,output);
        }
    }
    /// Internal cohort edits refer to distinct live tasks. Validate all handles
    /// before changing storage, then rebuild once, including an interrupted prefix.
    pub(crate) fn apply_batch(&mut self,edits:&[TickEdit<T>]) where T:Copy {
        for edit in edits{let id=match edit{TickEdit::Cancel(id)|TickEdit::Replace(id,..)=>*id};assert!(self.row(id).is_some(),"live batch task");}
        for edit in edits{match *edit{
            TickEdit::Cancel(id)=>{let row=self.row(id).unwrap();self.remove_unrepaired(row);},
            TickEdit::Replace(id,due,value)=>{let row=self.row(id).unwrap();self.heap[row].due=due;let task=self.slots[id.index as usize].task.as_mut().unwrap();task.due=due;task.value=value;}
        }}
        if !edits.is_empty(){for row in (0..self.heap.len()/2).rev(){self.down(row);}}
    }
    /// Actual Vec capacity bytes, excluding self, allocator metadata and payload
    /// allocations owned by T. Read outside hot loops for diagnostics.
    pub fn capacity_bytes(&self)->usize{
        self.heap.capacity()*std::mem::size_of::<Deadline>()+self.slots.capacity()*std::mem::size_of::<Slot<T>>()+self.free.capacity()*std::mem::size_of::<u32>()
    }
    fn row(&self,id:TickTaskId)->Option<usize>{let slot=self.slots.get(id.index as usize)?;if slot.generation!=id.generation{return None;}slot.heap.map(|v|(v.get()-1)as usize)}
    fn less(&self,a:usize,b:usize)->bool{(self.heap[a].due,self.heap[a].order)<(self.heap[b].due,self.heap[b].order)}
    fn swap(&mut self,a:usize,b:usize){
        self.heap.swap(a,b);self.slots[self.heap[a].index as usize].heap=NonZeroU32::new((a+1)as u32);self.slots[self.heap[b].index as usize].heap=NonZeroU32::new((b+1)as u32);
    }
    fn up(&mut self,mut row:usize){while row>0{let parent=(row-1)/2;if !self.less(row,parent){break;}self.swap(row,parent);row=parent;}}
    fn down(&mut self,mut row:usize){loop{let left=row*2+1;if left>=self.heap.len(){break;}let right=left+1;let child=if right<self.heap.len()&&self.less(right,left){right}else{left};if !self.less(child,row){break;}self.swap(child,row);row=child;}}
    fn repair(&mut self,row:usize){if row>0&&self.less(row,(row-1)/2){self.up(row);}else{self.down(row);}}
    fn remove(&mut self,row:usize)->TickTask<T>{
        let task=self.remove_unrepaired(row);if row<self.heap.len(){self.repair(row);}task
    }
    fn remove_unrepaired(&mut self,row:usize)->TickTask<T>{
        let deadline=self.heap.swap_remove(row);
        let slot=&mut self.slots[deadline.index as usize];let task=slot.task.take().unwrap();slot.heap=None;
        if let Some(generation)=slot.generation.checked_add(1){slot.generation=generation;self.free.push(task.id.index);}
        if row<self.heap.len(){self.slots[self.heap[row].index as usize].heap=NonZeroU32::new((row+1)as u32);}
        task
    }
}
