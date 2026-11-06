use std::num::NonZeroU32;
use std::collections::HashMap;
use std::sync::Arc;
use crate::{EntityId, GridPosition, MovementStatus, Tick, World};

pub const MAX_SYNC_ENTITIES: usize = 65_536;
pub const MAX_VISIBLE_ENTITIES: usize = 4096;
const MAX_VIEW_CELLS: i64 = 4096;
pub(crate) fn key(id: EntityId) -> (u32,u32) { (id.index,id.generation) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionState {
    pub entity: EntityId,
    pub position: GridPosition,
    pub movement: MovementStatus,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewRect { pub min: GridPosition, pub max: GridPosition }
impl ViewRect {
    pub fn contains(self, p: GridPosition) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncError {
    InvalidView, LimitExceeded, NoFrame, WaitingForAck, WrongStream,
    OutOfOrder, StaleTick, Malformed, MissingEntity, DuplicateEntity,
}
#[derive(Clone, Copy)]
struct Entry {
    cell: (i64,i64), state: MotionState,
    previous: Option<usize>, next: Option<usize>,
}
#[derive(Clone,Copy,Default,PartialEq,Eq)]
struct CellRevision { geometry:u64,membership:u64 }
#[derive(Clone)]
struct Bucket { head:usize,revision:CellRevision }
#[derive(Clone,Copy,PartialEq,Eq)]
struct CellRect { x0:i64,x1:i64,y0:i64,y1:i64 }
impl CellRect {
    fn cells(self)->impl Iterator<Item=(i64,i64)>{(self.x0..=self.x1).flat_map(move|x|(self.y0..=self.y1).map(move|y|(x,y)))}
    fn contains(self,(x,y):(i64,i64))->bool{x>=self.x0&&x<=self.x1&&y>=self.y0&&y<=self.y1}
    fn contains_rect(self,other:Self)->bool{self.contains((other.x0,other.y0))&&self.contains((other.x1,other.y1))}
    fn disjoint(self,other:Self)->bool{self.x1<other.x0||other.x1<self.x0||self.y1<other.y0||other.y1<self.y0}
}
#[derive(Clone)]
struct CellCrossing { row:usize,old:(i64,i64),new:(i64,i64),old_position:GridPosition }
#[derive(Clone)]
struct CrossingBlock { start:usize,end:usize,bounds:CellRect,positions:ViewRect }
const CROSSING_BLOCK_SIZE:usize=32;

/// One bounded membership cache per recipient. Strong identity prevents address
/// reuse after a source frame is dropped; it does not retain the frame itself.
#[derive(Default)]
pub(crate) struct VisibleRows {
    source:Option<Arc<()>>,revision:u64,view:Option<ViewRect>,
    pub(crate) rows:Vec<(EntityId,usize)>,
    cells:Option<CellRect>,cell_revisions:Vec<CellRevision>,
    candidates:Vec<(EntityId,usize)>,complete:bool,
    entering:Vec<(EntityId,usize)>,
}

/// Capture once after all commands/movement for the tick; share this immutable
/// frame across recipients. Dense rows mirror the world's iteration order.
/// Capture scans rows, but only changes bucket membership for added/removed
/// rows or cell crossings. A geometry revision allows recipients to reuse sorted
/// membership while reading fresh component/movement state from the current rows.
pub struct SpatialFrame {
    cell_size: NonZeroU32,
    pub(crate) tick: Option<Tick>,
    entries: Vec<Entry>,
    buckets: HashMap<(i64,i64),Bucket>,
    geometry_source:Arc<()>,geometry_revision:u64,
    crossing_base:u64,layout_changed:bool,in_cell_moved:bool,
    crossings:Vec<CellCrossing>,crossing_blocks:Vec<CrossingBlock>,
}
impl SpatialFrame {
    pub fn tick(&self)->Option<Tick>{self.tick}
    /// Retained element capacity, including indexed bucket payload. Excludes
    /// allocator metadata and hash-table control/unused allocation rounding.
    pub fn retained_payload_capacity_bytes(&self)->usize{
        self.entries.capacity()*std::mem::size_of::<Entry>()
        +self.buckets.capacity()*std::mem::size_of::<((i64,i64),Bucket)>()
        +self.crossings.capacity()*std::mem::size_of::<CellCrossing>()
        +self.crossing_blocks.capacity()*std::mem::size_of::<CrossingBlock>()
    }
    // Only for immutable retained frames. Two mutable forks must never share
    // geometry identity/revision, so SpatialFrame deliberately does not Clone.
    pub(crate) fn frozen_copy(&self)->Self{Self{cell_size:self.cell_size,tick:self.tick,entries:self.entries.clone(),buckets:self.buckets.clone(),geometry_source:self.geometry_source.clone(),geometry_revision:self.geometry_revision,crossing_base:self.crossing_base,layout_changed:self.layout_changed,in_cell_moved:self.in_cell_moved,crossings:self.crossings.clone(),crossing_blocks:self.crossing_blocks.clone()}}
    pub fn new(cell_size: NonZeroU32) -> Self { Self { cell_size, tick: None, entries: Vec::new(), buckets: HashMap::new(),geometry_source:Arc::new(()),geometry_revision:0,crossing_base:0,layout_changed:false,in_cell_moved:false,crossings:Vec::new(),crossing_blocks:Vec::new() } }
    pub fn capture(&mut self, world: &World) -> Result<(),SyncError> {
        if world.len() > MAX_SYNC_ENTITIES { return Err(SyncError::LimitExceeded); }
        self.crossing_base=self.geometry_revision;self.layout_changed=false;self.in_cell_moved=false;
        self.crossings.clear();self.crossing_blocks.clear();
        let size = i64::from(self.cell_size.get());
        let next_revision=self.geometry_revision.checked_add(1).unwrap_or(1);
        let mut geometry_changed=false;
        for (row,state) in world.motion_states().enumerate() {
            if let Some(old) = self.entries.get(row) {
                if old.state == state { continue; }
                // Even movement inside one bucket can cross a view boundary.
                geometry_changed|=old.state.entity!=state.entity||old.state.position!=state.position;
                let identity_changed=old.state.entity!=state.entity;
                self.layout_changed|=identity_changed;
                // Status/id-only changes and movement within a bucket do not
                // require unlinking. Slot reuse and dense swap removal are
                // handled by replacing this row's complete captured state.
                let mut relinked=false;
                if old.state.position != state.position {
                    let cell = (i64::from(state.position.x).div_euclid(size),i64::from(state.position.y).div_euclid(size));
                    if old.cell != cell {
                        self.crossings.push(CellCrossing{row,old:old.cell,new:cell,old_position:old.state.position});
                        self.unlink(row,next_revision);
                        self.entries[row].cell = cell;
                        self.link(row,next_revision);
                        relinked=true;
                    }else{self.in_cell_moved=true;}
                }
                if !relinked&&(identity_changed||self.entries[row].state.position!=state.position){
                    let bucket=self.buckets.get_mut(&self.entries[row].cell).expect("indexed bucket");
                    bucket.revision.geometry=next_revision;
                    if identity_changed{bucket.revision.membership=next_revision;}
                }
                self.entries[row].state = state;
            } else {
                self.layout_changed=true;
                geometry_changed=true;
                let cell = (i64::from(state.position.x).div_euclid(size),i64::from(state.position.y).div_euclid(size));
                self.entries.push(Entry { cell,state,previous:None,next:None });
                self.link(row,next_revision);
            }
        }
        while self.entries.len() > world.len() {
            self.layout_changed=true;
            geometry_changed=true;
            self.unlink(self.entries.len()-1,next_revision);
            self.entries.pop();
        }
        if geometry_changed{
            if self.geometry_revision==u64::MAX{
                // Zero denotes an absent cell. Rebase every live stamp when
                // rotating identity so old stamps cannot alias future values.
                self.geometry_source=Arc::new(());
                for bucket in self.buckets.values_mut(){bucket.revision=CellRevision{geometry:1,membership:1};}
            }
            self.geometry_revision=next_revision;
        }
        if !self.layout_changed{self.prepare_crossings();}
        else{self.crossings.clear();}
        self.tick = Some(world.tick());
        Ok(())
    }
    /// Sort at most once per capture, shared by all recipients. Ordered dense
    /// layouts need no sort. Blocks let recipients skip wholly internal/remote moves.
    fn prepare_crossings(&mut self){
        if !self.crossings.windows(2).all(|w|key(self.entries[w[0].row].state.entity)<key(self.entries[w[1].row].state.entity)){
            self.crossings.sort_unstable_by_key(|c|key(self.entries[c.row].state.entity));
        }
        for start in (0..self.crossings.len()).step_by(CROSSING_BLOCK_SIZE){
            let end=(start+CROSSING_BLOCK_SIZE).min(self.crossings.len());let first=&self.crossings[start];
            let mut bounds=CellRect{x0:first.old.0,x1:first.old.0,y0:first.old.1,y1:first.old.1};
            let mut positions=ViewRect{min:first.old_position,max:first.old_position};
            for crossing in &self.crossings[start..end]{for (x,y) in [crossing.old,crossing.new]{bounds.x0=bounds.x0.min(x);bounds.x1=bounds.x1.max(x);bounds.y0=bounds.y0.min(y);bounds.y1=bounds.y1.max(y);}}
            for crossing in &self.crossings[start..end]{for p in [crossing.old_position,self.entries[crossing.row].state.position]{
                positions.min.x=positions.min.x.min(p.x);positions.min.y=positions.min.y.min(p.y);
                positions.max.x=positions.max.x.max(p.x);positions.max.y=positions.max.y.max(p.y);
            }}
            self.crossing_blocks.push(CrossingBlock{start,end,bounds,positions});
        }
    }
    fn unlink(&mut self, row:usize, revision:u64) {
        let entry = self.entries[row];
        if let Some(previous) = entry.previous { self.entries[previous].next = entry.next; }
        if let Some(next) = entry.next { self.entries[next].previous = entry.previous; }
        if entry.previous.is_none()&&entry.next.is_none(){self.buckets.remove(&entry.cell);}
        else{
            let bucket=self.buckets.get_mut(&entry.cell).expect("indexed bucket");
            if entry.previous.is_none(){bucket.head=entry.next.expect("remaining head");}
            bucket.revision=CellRevision{geometry:revision,membership:revision};
        }
    }
    fn link(&mut self, row:usize, revision:u64) {
        let cell = self.entries[row].cell;
        let next = self.buckets.insert(cell,Bucket{head:row,revision:CellRevision{geometry:revision,membership:revision}}).map(|b|b.head);
        self.entries[row].previous = None;
        self.entries[row].next = next;
        if let Some(next) = next { self.entries[next].previous = Some(row); }
    }
    pub fn visible(&self, view: ViewRect) -> Result<Vec<MotionState>,SyncError> {
        let mut result = Vec::new(); self.collect(view,&mut result)?; Ok(result)
    }
    pub(crate) fn collect(&self, view: ViewRect, out: &mut Vec<MotionState>) -> Result<(),SyncError> {
        out.clear();
        self.visit_visible(view,|_,state|out.push(state))?;
        out.sort_unstable_by_key(|state| key(state.entity));
        Ok(())
    }
    /// Row indices refer to this capture's world iteration order, never entity
    /// slot numbers. Paired component frames can avoid per-recipient hash lookups.
    pub(crate) fn collect_rows(&self, view: ViewRect, out: &mut Vec<(EntityId,usize)>) -> Result<(),SyncError> {
        out.clear();
        self.visit_visible(view,|row,state|out.push((state.entity,row)))?;
        out.sort_unstable_by_key(|(entity,_)|key(*entity));
        Ok(())
    }
    /// Returns true only when the previous successful membership query is reused.
    pub(crate) fn collect_rows_cached(&self,view:ViewRect,cache:&mut VisibleRows)->Result<bool,SyncError>{
        if self.tick.is_none(){return Err(SyncError::NoFrame);}
        let same_source=cache.source.as_ref().is_some_and(|source|Arc::ptr_eq(source,&self.geometry_source));
        if same_source&&cache.revision==self.geometry_revision&&cache.view==Some(view){
            return Ok(true);
        }
        let cells=match self.view_cells(view){Ok(cells)=>cells,Err(error)=>{cache.source=None;return Err(error)}};
        if same_source&&cache.cells==Some(cells){
            // A complete, contiguous crossing-only journal can prove both the
            // cell union and exact visible membership unchanged. Keep the rows,
            // but always read their new positions/components from this frame.
            if cache.complete&&!self.layout_changed&&!self.in_cell_moved
                &&cache.revision==self.crossing_base&&cache.view==Some(view)
                &&self.crossings_preserve_rows(cells,view){
                let mut geometry_same=true;
                for (cell,stamp) in cells.cells().zip(&mut cache.cell_revisions){
                    let current=self.cell_revision(cell);geometry_same&=stamp.geometry==current.geometry;*stamp=current;
                }
                cache.revision=self.geometry_revision;
                // Remote-only changes still report reuse as before; local
                // journal validation keeps the existing false result.
                return Ok(geometry_same);
            }
            let(mut geometry_same,mut membership_same)=(true,true);
            if cache.revision!=self.geometry_revision{
                for (cell,old) in cells.cells().zip(&mut cache.cell_revisions){
                    let current=self.cell_revision(cell);
                    geometry_same&=old.geometry==current.geometry;membership_same&=old.membership==current.membership;
                    *old=current;
                    // Both incremental repair and full rebuild refresh all saved
                    // stamps. Do not query the remaining cells twice here.
                    if !membership_same{break;}
                }
            }
            if geometry_same&&cache.view==Some(view){cache.revision=self.geometry_revision;return Ok(true);}
            if membership_same&&cache.complete{
                // Positions or exact view bounds changed, but sorted cell
                // membership and dense row indices are still valid.
                self.filter_candidates(view,cache);
                cache.view=Some(view);cache.revision=self.geometry_revision;return Ok(false);
            }
            if cache.complete&&!self.layout_changed&&cache.revision==self.crossing_base&&self.patch_crossings(cells,cache){
                self.filter_candidates(view,cache);
                for (cell,stamp) in cells.cells().zip(&mut cache.cell_revisions){*stamp=self.cell_revision(cell);}
                cache.view=Some(view);cache.revision=self.geometry_revision;return Ok(false);
            }
        }
        if same_source&&cache.complete&&cache.cells.is_some_and(|old|old!=cells&&!old.disjoint(cells))
            &&(cache.revision==self.geometry_revision||(!self.layout_changed&&cache.revision==self.crossing_base))
            &&self.patch_view(cache.cells.unwrap(),cells,cache){
            self.filter_candidates(view,cache);
            cache.cell_revisions.clear();cache.cell_revisions.extend(cells.cells().map(|cell|self.cell_revision(cell)));
            cache.cells=Some(cells);cache.view=Some(view);cache.revision=self.geometry_revision;return Ok(false);
        }
        // A failed query may leave partial scratch rows. Never mark them valid,
        // including when the caller next retries its previously valid view.
        cache.source=None;
        cache.candidates.clear();cache.cell_revisions.clear();let mut complete=true;
        for cell in cells.cells(){
            let bucket=self.buckets.get(&cell);
            cache.cell_revisions.push(bucket.map_or(CellRevision::default(),|b|b.revision));
            if !complete{continue;}
            let mut cursor=bucket.map(|b|b.head);
            while let Some(row)=cursor{
                if cache.candidates.len()==MAX_VISIBLE_ENTITIES{complete=false;cache.candidates.clear();break;}
                let entry=&self.entries[row];cache.candidates.push((entry.state.entity,row));cursor=entry.next;
            }
        }
        if complete{
            cache.candidates.sort_unstable_by_key(|&(entity,_)|key(entity));self.filter_candidates(view,cache);
        }else{
            // Dense edge cells may hold more candidates than the visible limit.
            // Bound cache memory without rejecting an otherwise valid view.
            self.collect_rows(view,&mut cache.rows)?;
        }
        cache.cells=Some(cells);cache.complete=complete;
        cache.revision=self.geometry_revision;cache.view=Some(view);cache.source=Some(Arc::clone(&self.geometry_source));
        Ok(false)
    }
    /// A contiguous capture with unchanged dense layout can update the sorted
    /// union directly. Moves between two covered cells do not change that union.
    fn crossings_preserve_rows(&self,cells:CellRect,view:ViewRect)->bool{
        for block in &self.crossing_blocks{
            if cells.disjoint(block.bounds){continue;}
            if cells.contains_rect(block.bounds)&&view.contains(block.positions.min)&&view.contains(block.positions.max){continue;}
            for crossing in &self.crossings[block.start..block.end]{
                if cells.contains(crossing.old)!=cells.contains(crossing.new)
                    ||view.contains(crossing.old_position)!=view.contains(self.entries[crossing.row].state.position){return false;}
            }
        }
        true
    }
    fn patch_crossings(&self,cells:CellRect,cache:&mut VisibleRows)->bool{
        cache.entering.clear();let mut leaving=false;
        for block in &self.crossing_blocks{
            if cells.contains_rect(block.bounds)||cells.disjoint(block.bounds){continue;}
            for crossing in &self.crossings[block.start..block.end]{
                let old=cells.contains(crossing.old);let new=cells.contains(crossing.new);
                if new&&!old{
                    if cache.entering.len()==MAX_VISIBLE_ENTITIES{return false;}
                    cache.entering.push((self.entries[crossing.row].state.entity,crossing.row));
                }else if old&&!new{leaving=true;}
            }
        }
        if leaving{cache.candidates.retain(|&(_,row)|cells.contains(self.entries[row].cell));}
        Self::merge_entering(cache)
    }
    /// Preserve the old/new cell intersection. Query only newly covered strips;
    /// add journal arrivals into the intersection when movement happened as well.
    fn patch_view(&self,old:CellRect,cells:CellRect,cache:&mut VisibleRows)->bool{
        cache.entering.clear();
        for cell in cells.cells(){
            if old.contains(cell){continue;}
            let mut cursor=self.buckets.get(&cell).map(|b|b.head);
            while let Some(row)=cursor{
                if cache.entering.len()==MAX_VISIBLE_ENTITIES{return false;}
                let entry=&self.entries[row];cache.entering.push((entry.state.entity,row));cursor=entry.next;
            }
        }
        if cache.revision!=self.geometry_revision{
            for block in &self.crossing_blocks{
                if old.contains_rect(block.bounds)||cells.disjoint(block.bounds){continue;}
                for crossing in &self.crossings[block.start..block.end]{
                    if !old.contains(crossing.old)&&old.contains(crossing.new)&&cells.contains(crossing.new){
                        if cache.entering.len()==MAX_VISIBLE_ENTITIES{return false;}
                        cache.entering.push((self.entries[crossing.row].state.entity,crossing.row));
                    }
                }
            }
        }
        // Old members moving into a new strip were collected above. Retain only
        // the intersection, so each entity enters the merge exactly once.
        cache.candidates.retain(|&(_,row)|{let cell=self.entries[row].cell;old.contains(cell)&&cells.contains(cell)});
        cache.entering.sort_unstable_by_key(|&(id,_)|key(id));
        Self::merge_entering(cache)
    }
    fn merge_entering(cache:&mut VisibleRows)->bool{
        if cache.candidates.len()+cache.entering.len()>MAX_VISIBLE_ENTITIES{return false;}
        if !cache.entering.is_empty(){
            cache.rows.clear();let(mut a,mut b)=(0,0);
            while a<cache.candidates.len()&&b<cache.entering.len(){
                if key(cache.candidates[a].0)<key(cache.entering[b].0){cache.rows.push(cache.candidates[a]);a+=1;}
                else{cache.rows.push(cache.entering[b]);b+=1;}
            }
            cache.rows.extend_from_slice(&cache.candidates[a..]);cache.rows.extend_from_slice(&cache.entering[b..]);
            std::mem::swap(&mut cache.rows,&mut cache.candidates);
        }
        true
    }
    fn cell_revision(&self,cell:(i64,i64))->CellRevision{self.buckets.get(&cell).map_or(CellRevision::default(),|b|b.revision)}
    fn filter_candidates(&self,view:ViewRect,cache:&mut VisibleRows){
        cache.rows.clear();cache.rows.extend(cache.candidates.iter().copied().filter(|&(_,row)|view.contains(self.entries[row].state.position)));
    }
    fn collect_cached(&self,view:ViewRect,cache:&mut VisibleRows,out:&mut Vec<MotionState>)->Result<(),SyncError>{
        self.collect_rows_cached(view,cache)?;
        out.clear();out.extend(cache.rows.iter().map(|&(_,row)|self.entries[row].state));Ok(())
    }
    fn view_cells(&self,view:ViewRect)->Result<CellRect,SyncError>{
        if self.tick.is_none() { return Err(SyncError::NoFrame); }
        if view.min.x > view.max.x || view.min.y > view.max.y { return Err(SyncError::InvalidView); }
        let size = i64::from(self.cell_size.get());
        let (x0,x1,y0,y1) = (i64::from(view.min.x).div_euclid(size),i64::from(view.max.x).div_euclid(size),
            i64::from(view.min.y).div_euclid(size),i64::from(view.max.y).div_euclid(size));
        if (x1-x0+1).saturating_mul(y1-y0+1) > MAX_VIEW_CELLS { return Err(SyncError::LimitExceeded); }
        Ok(CellRect{x0,x1,y0,y1})
    }
    fn visit_visible(&self, view: ViewRect, mut push:impl FnMut(usize,MotionState)) -> Result<(),SyncError> {
        let cells=self.view_cells(view)?;
        let mut count=0;
        for cell in cells.cells(){
            let mut cursor = self.buckets.get(&cell).map(|b|b.head);
            while let Some(row) = cursor {
                let e = &self.entries[row];
                cursor = e.next;
                if view.contains(e.state.position) {
                    if count == MAX_VISIBLE_ENTITIES { return Err(SyncError::LimitExceeded); }
                    count+=1;push(row,e.state);
                }
            }
        }
        Ok(())
    }

    /// Combat queries share the buckets but not the replication hit cap. Charge
    /// every visited bucket/entry, even entries outside the precise rectangle,
    /// so a dense boundary cell cannot hide unbounded candidate work.
    pub(crate) fn visit_combat_candidates(
        &self, view: ViewRect, budget: &mut crate::CombatQueryBudget,
        mut visit: impl FnMut(MotionState, &mut crate::CombatQueryBudget) -> Result<(), crate::CombatQueryError>,
    ) -> Result<(), crate::CombatQueryError> {
        let cells = self.view_cells(view).map_err(crate::CombatQueryError::Spatial)?;
        for cell in cells.cells() {
            budget.cell()?;
            let mut cursor = self.buckets.get(&cell).map(|b| b.head);
            while let Some(row) = cursor {
                budget.candidate()?;
                let entry = &self.entries[row];
                cursor = entry.next;
                if view.contains(entry.state.position) { visit(entry.state, budget)?; }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisChange { pub value: i32, pub delta: Option<i16> }
impl AxisChange {
    fn between(old: i32, value: i32) -> Option<Self> {
        (old != value).then(|| Self { value, delta: i16::try_from(i64::from(value)-i64::from(old)).ok() })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncChange {
    Enter(MotionState),
    Leave(EntityId),
    Update { entity: EntityId, x: Option<AxisChange>, y: Option<AxisChange>, movement: Option<MovementStatus> },
}
impl SyncChange {
    pub fn entity(self) -> EntityId { match self {Self::Enter(s)=>s.entity,Self::Leave(e)=>e,Self::Update{entity,..}=>entity} }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncHeader { pub stream: u64, pub sequence: u64, pub base: u64, pub tick: Tick, pub reset: bool }

/// One sender per recipient/world subscription. stream must identify that
/// subscription uniquely. Acknowledgements come from the bound recipient only.
/// One outstanding packet: retry its exact bytes, or request a full resync.
pub struct SyncSender {
    stream: u64, issued: u64, confirmed: u64, last_tick: Tick, initialized: bool,
    baseline: Vec<MotionState>, staged: Vec<MotionState>,
    changes: Vec<SyncChange>, packet: Vec<u8>, pending: Option<SyncHeader>,
    visible:VisibleRows,
}
impl SyncSender {
    /// Sender baselines, change buffers and spatial membership cache payload.
    /// This is capacity accounting, not total process/allocator memory.
    pub fn retained_payload_capacity_bytes(&self)->usize{
        let v=&self.visible;
        (self.baseline.capacity()+self.staged.capacity())*std::mem::size_of::<MotionState>()
        +self.changes.capacity()*std::mem::size_of::<SyncChange>()+self.packet.capacity()
        +(v.rows.capacity()+v.candidates.capacity()+v.entering.capacity())*std::mem::size_of::<(EntityId,usize)>()
        +v.cell_revisions.capacity()*std::mem::size_of::<CellRevision>()
    }
    pub fn new(stream: u64) -> Self {
        Self { stream,issued:0,confirmed:0,last_tick:Tick(0),initialized:false,
            baseline:Vec::new(),staged:Vec::new(),changes:Vec::new(),packet:Vec::new(),pending:None,visible:VisibleRows::default() }
    }
    pub fn prepare(&mut self, frame: &SpatialFrame, view: ViewRect) -> Result<Option<&[u8]>,SyncError> {
        if self.pending.is_some() { return Err(SyncError::WaitingForAck); }
        let tick = frame.tick.ok_or(SyncError::NoFrame)?;
        if tick.0 < self.last_tick.0 { return Err(SyncError::StaleTick); }
        let sequence = self.issued.checked_add(1).ok_or(SyncError::LimitExceeded)?;
        frame.collect_cached(view,&mut self.visible,&mut self.staged)?;
        self.changes.clear();
        let mut old = 0; let mut new = 0;
        while old < self.baseline.len() || new < self.staged.len() {
            let a = self.baseline.get(old); let b = self.staged.get(new);
            match (a,b) {
                (Some(a),Some(b)) if key(a.entity) == key(b.entity) => {
                    let x = AxisChange::between(a.position.x,b.position.x);
                    let y = AxisChange::between(a.position.y,b.position.y);
                    let movement = (a.movement != b.movement).then_some(b.movement);
                    if x.is_some() || y.is_some() || movement.is_some() {
                        self.changes.push(SyncChange::Update {entity:b.entity,x,y,movement});
                    }
                    old+=1;new+=1;
                },
                (Some(a),Some(b)) if key(a.entity)<key(b.entity) => { self.changes.push(SyncChange::Leave(a.entity));old+=1; },
                (Some(a),None) => { self.changes.push(SyncChange::Leave(a.entity));old+=1; },
                (_,Some(b)) => { self.changes.push(SyncChange::Enter(*b));new+=1; },
                _ => unreachable!(),
            }
        }
        if self.initialized && self.changes.is_empty() { self.last_tick=tick; return Ok(None); }
        let header = SyncHeader {stream:self.stream,sequence,base:if self.initialized {self.confirmed}else{0},tick,reset:!self.initialized};
        crate::sync_wire::encode(header,&self.changes,&mut self.packet);
        self.issued=sequence;self.last_tick=tick;self.pending=Some(header);
        Ok(Some(&self.packet))
    }
    pub fn pending_packet(&self) -> Option<&[u8]> { self.pending.map(|_| self.packet.as_slice()) }
    pub fn pending_header(&self) -> Option<SyncHeader> { self.pending }
    pub fn pending_changes(&self) -> &[SyncChange] { if self.pending.is_some() { &self.changes } else { &[] } }
    /// Re-encode the pending logical message for transports with their own buffer.
    pub fn encode_pending_into(&self, out: &mut Vec<u8>) -> Result<(),SyncError> {
        let header=self.pending.ok_or(SyncError::NoFrame)?;crate::sync_wire::encode(header,&self.changes,out);Ok(())
    }
    pub fn acknowledge(&mut self, stream:u64, sequence:u64) -> Result<(),SyncError> {
        if stream!=self.stream {return Err(SyncError::WrongStream);}
        if sequence==self.confirmed && self.initialized {return Ok(());}
        let pending=self.pending.ok_or(SyncError::OutOfOrder)?;
        if sequence!=pending.sequence {return Err(SyncError::OutOfOrder);}
        std::mem::swap(&mut self.baseline,&mut self.staged);
        self.confirmed=sequence;self.initialized=true;self.pending=None;Ok(())
    }
    pub fn acknowledge_packet(&mut self, bytes:&[u8]) -> Result<(),SyncError> {
        let (stream,sequence)=crate::sync_wire::decode_ack(bytes)?;self.acknowledge(stream,sequence)
    }
    /// Keep sequence monotonic so an old delayed packet cannot undo a resync.
    /// If an ACK is missing, this can replace the outstanding delta with a reset.
    pub fn request_full(&mut self) {
        self.pending=None;self.initialized=false;self.confirmed=0;self.baseline.clear();
    }
}

#[cfg(test)]
mod visibility_cache_tests {
    use super::*;
    use std::num::NonZeroU16;
    fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
    fn rect(a:i32,b:i32)->ViewRect{ViewRect{min:p(a),max:p(b)}}
    fn check(w:&World,f:&SpatialFrame,c:&mut VisibleRows,v:ViewRect,reused:bool){
        assert_eq!(f.collect_rows_cached(v,c).unwrap(),reused);
        let mut expected:Vec<_>=w.motion_states().filter(|s|v.contains(s.position)).collect();expected.sort_unstable_by_key(|s|key(s.entity));
        let mut actual=Vec::new();f.collect_cached(v,c,&mut actual).unwrap();assert_eq!(actual,expected);
    }
    #[test]
    fn crossing_proof_retains_rows_and_reads_current_positions(){
        let mut w=World::default();let ids:Vec<_>=(0..96).map(|i|w.spawn(p(i*2)).unwrap()).collect();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();
        let v=ViewRect{min:p(0),max:GridPosition{x:128,y:64}};f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);let original=c.rows.clone();
        for y in [64,0,64]{for &id in &ids{let mut position=w.position(id).unwrap();position.y=y;w.set_position(id,position).unwrap();}
            f.capture(&w).unwrap();assert!(!f.in_cell_moved&&!f.layout_changed);assert!(f.crossings_preserve_rows(f.view_cells(v).unwrap(),v));
            check(&w,&f,&mut c,v,false);assert_eq!(c.rows,original);
        }
    }
    #[test]
    fn crossing_proof_checks_exact_edges_and_same_cell_motion(){
        let mut w=World::default();let a=w.spawn(p(1)).unwrap();let b=w.spawn(p(10)).unwrap();let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();
        let v=ViewRect{min:p(0),max:GridPosition{x:20,y:70}};f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);
        w.set_position(a,GridPosition{x:1,y:71}).unwrap();f.capture(&w).unwrap();
        // Both grid cells remain covered, but y71 is outside the exact view.
        assert!(!f.crossings_preserve_rows(f.view_cells(v).unwrap(),v));check(&w,&f,&mut c,v,false);assert_eq!(c.rows.len(),1);
        w.set_position(a,p(1)).unwrap();w.set_position(b,p(21)).unwrap();f.capture(&w).unwrap();
        assert!(f.in_cell_moved);check(&w,&f,&mut c,v,false);assert_eq!(c.rows[0].0,a);
        // Positions may swap membership at an edge; journal row identity matters.
        w.set_position(a,GridPosition{x:1,y:71}).unwrap();w.set_position(b,GridPosition{x:10,y:64}).unwrap();f.capture(&w).unwrap();
        assert!(!f.crossings_preserve_rows(f.view_cells(v).unwrap(),v));check(&w,&f,&mut c,v,false);assert_eq!(c.rows[0].0,b);
    }
    #[test]
    fn crossing_proof_rejects_cell_union_changes_and_refreshes_stamps(){
        let mut w=World::default();let a=w.spawn(p(-1)).unwrap();let b=w.spawn(p(128)).unwrap();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();let v=ViewRect{min:p(-64),max:GridPosition{x:63,y:64}};
        f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);w.set_position(a,GridPosition{x:-1,y:64}).unwrap();w.set_position(b,p(192)).unwrap();f.capture(&w).unwrap();
        assert!(f.crossings_preserve_rows(f.view_cells(v).unwrap(),v));check(&w,&f,&mut c,v,false);
        assert!(c.cells.unwrap().cells().zip(&c.cell_revisions).all(|(cell,stamp)|*stamp==f.cell_revision(cell)));
        w.set_position(b,p(63)).unwrap();f.capture(&w).unwrap();assert!(!f.crossings_preserve_rows(f.view_cells(v).unwrap(),v));check(&w,&f,&mut c,v,false);assert_eq!(c.rows.len(),2);
        // Losing one captured journal must use the existing full-query fallback.
        w.set_position(a,p(128)).unwrap();f.capture(&w).unwrap();w.set_position(b,p(0)).unwrap();f.capture(&w).unwrap();
        assert_ne!(c.revision,f.crossing_base);check(&w,&f,&mut c,v,false);assert_eq!(c.rows[0].0,b);
    }
    #[test]
    fn reuses_membership_but_reads_fresh_status_and_invalidates_same_cell_moves(){
        let mut w=World::default();let id=w.spawn(p(9)).unwrap();let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();let v=rect(0,10);
        assert_eq!(f.collect_rows_cached(v,&mut c),Err(SyncError::NoFrame));
        f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);w.advance_tick().unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,true);
        w.move_to(id,p(30),NonZeroU16::new(1).unwrap()).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,true);
        assert_eq!(f.entries[c.rows[0].1].state.movement,MovementStatus::Moving);
        w.stop_moving(id).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,true);
        for (x,reused) in [(11,false),(10,false),(-1,false),(0,false),(65,false),(i32::MIN,true),(i32::MAX,true),(9,false)]{w.set_position(id,p(x)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,reused);}
        check(&w,&f,&mut c,rect(0,9),false);check(&w,&f,&mut c,rect(0,9),true);
    }
    #[test]
    fn source_identity_dense_swaps_reuse_and_revision_wrap_invalidate(){
        let mut a=World::default();let first=a.spawn(p(-1)).unwrap();a.spawn(p(1)).unwrap();
        let mut b=World::default();b.spawn(p(1)).unwrap();b.spawn(p(-1)).unwrap();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut g=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();let v=rect(-10,0);
        f.capture(&a).unwrap();g.capture(&b).unwrap();assert_eq!(f.geometry_revision,g.geometry_revision);
        check(&a,&f,&mut c,v,false);check(&b,&g,&mut c,v,false);check(&a,&f,&mut c,v,false);
        a.despawn(first).unwrap();let replacement=a.spawn(p(-1)).unwrap();assert_eq!(first.index,replacement.index);assert_ne!(first.generation,replacement.generation);
        f.capture(&a).unwrap();check(&a,&f,&mut c,v,false);
        let source=Arc::clone(&f.geometry_source);f.geometry_revision=u64::MAX;a.set_position(replacement,p(-2)).unwrap();f.capture(&a).unwrap();
        assert_eq!(f.geometry_revision,1);assert!(!Arc::ptr_eq(&source,&f.geometry_source));check(&a,&f,&mut c,v,false);
        drop(f);let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());f.capture(&a).unwrap();check(&a,&f,&mut c,v,false);
    }
    #[test]
    fn failed_queries_cannot_reuse_partial_rows_or_bypass_limits(){
        let mut w=World::default();for _ in 0..MAX_VISIBLE_ENTITIES{w.spawn(p(0)).unwrap();}w.spawn(p(64)).unwrap();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());f.capture(&w).unwrap();let mut c=VisibleRows::default();let good=rect(0,0);
        check(&w,&f,&mut c,good,false);
        for (bad,error) in [(rect(0,64),SyncError::LimitExceeded),(rect(1,0),SyncError::InvalidView),(rect(i32::MIN,i32::MAX),SyncError::LimitExceeded)]{
            assert_eq!(f.collect_rows_cached(bad,&mut c),Err(error));check(&w,&f,&mut c,good,false);
        }
        check(&w,&f,&mut c,good,true);
    }
    #[test]
    fn unrelated_cells_reuse_rows_and_empty_cell_recreation_has_a_new_stamp(){
        let mut w=World::default();let near=w.spawn(p(1)).unwrap();let far=w.spawn(p(640)).unwrap();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();let v=rect(0,2);
        f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);
        for x in [641,704,4096,-4096]{w.set_position(far,p(x)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,true);}
        // Both changes occur without observing the temporarily empty cell.
        w.set_position(near,p(64)).unwrap();f.capture(&w).unwrap();w.set_position(near,p(10)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);assert!(c.rows.is_empty());
        w.set_position(near,p(2)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);assert_eq!(c.rows[0].0,near);
        // Empty cells are removed, not retained as an unbounded revision history.
        for i in 1..=200{w.set_position(far,p(i*128)).unwrap();f.capture(&w).unwrap();assert_eq!(f.buckets.len(),2);check(&w,&f,&mut c,v,true);}
    }
    #[test]
    fn sorted_cell_candidates_follow_exact_view_bounds_and_dense_row_changes(){
        let mut w=World::default();let a=w.spawn(p(1)).unwrap();let b=w.spawn(p(20)).unwrap();let outside=w.spawn(p(1000)).unwrap();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();f.capture(&w).unwrap();check(&w,&f,&mut c,rect(0,10),false);
        let candidates=c.candidates.clone();assert_eq!(candidates.len(),2);
        check(&w,&f,&mut c,rect(10,30),false);assert_eq!(c.candidates,candidates);assert_eq!(c.rows[0].0,b);
        w.set_position(a,p(15)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,rect(10,30),false);assert_eq!(c.candidates,candidates);assert_eq!(c.rows.len(),2);
        w.despawn(outside).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,rect(10,30),true);
        let tail=w.spawn(p(25)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,rect(10,30),false);
        w.despawn(a).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,rect(10,30),false);assert!(c.rows.iter().any(|&(id,row)|id==tail&&row==0));
    }
    #[test]
    fn crossings_merge_sorted_members_after_dense_reordering_and_missed_captures(){
        let mut w=World::default();let first=w.spawn(p(0)).unwrap();let ids:Vec<_>=(0..95).map(|i|w.spawn(p((i%5)*64)).unwrap()).collect();
        w.despawn(first).unwrap(); // A high ID now precedes lower IDs in dense order.
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();let v=rect(0,127);
        f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);
        for (i,&id)in ids.iter().enumerate(){w.set_position(id,p(if i%3==0{-64}else{64})).unwrap();}
        f.capture(&w).unwrap();assert!(!f.layout_changed);assert!(f.crossings.windows(2).all(|a|key(f.entries[a[0].row].state.entity)<key(f.entries[a[1].row].state.entity)));
        check(&w,&f,&mut c,v,false);assert!(c.complete);assert!(!c.entering.is_empty());
        let retained=c.candidates.clone();for &id in &ids{if w.position(id).unwrap().x==64{w.set_position(id,p(0)).unwrap();}}
        f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);assert_eq!(c.candidates,retained);
        // A missed crossing cannot be repaired using only the latest capture.
        w.set_position(ids[0],p(0)).unwrap();f.capture(&w).unwrap();w.set_position(ids[1],p(1)).unwrap();f.capture(&w).unwrap();
        assert_ne!(c.revision,f.crossing_base);check(&w,&f,&mut c,v,false);assert!(c.rows.iter().any(|&(id,_)|id==ids[0]));
    }
    #[test]
    fn crossing_patch_overflow_falls_back_and_failed_query_can_recover(){
        let mut w=World::default();w.spawn(p(0)).unwrap();for _ in 1..MAX_VISIBLE_ENTITIES{w.spawn(p(32)).unwrap();}let entrant=w.spawn(p(64)).unwrap();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();let v=rect(0,0);
        f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);assert!(c.complete);
        w.set_position(entrant,p(32)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);assert!(!c.complete);assert_eq!(c.rows.len(),1);
        assert_eq!(f.collect_rows_cached(rect(0,63),&mut c),Err(SyncError::LimitExceeded));check(&w,&f,&mut c,v,false);
        w.set_position(entrant,p(64)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);assert!(c.complete);
    }
    #[test]
    fn crossing_caches_match_direct_world_with_delayed_observers_and_churn(){
        let mut w=World::default();let mut ids:Vec<_>=(0..160).map(|i|w.spawn(p(i*13-1000)).unwrap()).collect();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut caches:Vec<_>=(0..3).map(|_|VisibleRows::default()).collect();
        let mut seed=12345u32;
        for tick in 0..120{
            if tick%17==0{let old=ids.remove(0);w.despawn(old).unwrap();ids.push(w.spawn(p(0)).unwrap());}
            for _ in 0..48{seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);let id=ids[seed as usize%ids.len()];seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);w.set_position(id,p((seed%4096)as i32-2048)).unwrap();}
            f.capture(&w).unwrap();
            for (r,c)in caches.iter_mut().enumerate(){if tick%(r+1)!=0{continue;}let v=rect(-1000+(tick/8)as i32*13+r as i32*100,300+r as i32*500);let mut actual=Vec::new();f.collect_cached(v,c,&mut actual).unwrap();
                let mut expected:Vec<_>=w.motion_states().filter(|s|v.contains(s.position)).collect();expected.sort_unstable_by_key(|s|key(s.entity));assert_eq!(actual,expected);
                let cells=f.view_cells(v).unwrap();let mut union:Vec<_>=f.entries.iter().enumerate().filter(|(_,e)|cells.contains(e.cell)).map(|(row,e)|(e.state.entity,row)).collect();union.sort_unstable_by_key(|&(id,_)|key(id));assert!(c.complete);assert_eq!(c.candidates,union);
            }
        }
    }
    #[test]
    fn view_strips_and_simultaneous_crossings_merge_without_duplicate_members(){
        let mut w=World::default();let ids:Vec<_>=[-64,0,64,128,192,256].into_iter().map(|x|w.spawn(p(x)).unwrap()).collect();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();f.capture(&w).unwrap();check(&w,&f,&mut c,rect(0,127),false);
        for (i,x)in [(0,64),(1,128),(2,256),(3,64),(4,128)]{w.set_position(ids[i],p(x)).unwrap();}
        f.capture(&w).unwrap();check(&w,&f,&mut c,rect(64,191),false);assert_eq!(c.candidates.iter().map(|&(id,_)|id).collect::<Vec<_>>(),vec![ids[0],ids[1],ids[3],ids[4]]);
        for v in [rect(-64,191),rect(0,127),rect(-1000,-900),rect(0,191),rect(64,256)]{check(&w,&f,&mut c,v,false);}
        w.set_position(ids[0],p(-64)).unwrap();f.capture(&w).unwrap();w.set_position(ids[1],p(1)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,rect(-64,128),false);
    }
    #[test]
    fn view_strip_overflow_falls_back_without_rejecting_valid_exact_bounds(){
        let mut w=World::default();w.spawn(p(0)).unwrap();for _ in 0..MAX_VISIBLE_ENTITIES{w.spawn(p(96)).unwrap();}
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();f.capture(&w).unwrap();check(&w,&f,&mut c,rect(0,0),false);
        check(&w,&f,&mut c,rect(0,64),false);assert!(!c.complete);assert_eq!(c.rows.len(),1);
        check(&w,&f,&mut c,rect(0,0),false);assert_eq!(f.collect_rows_cached(rect(0,127),&mut c),Err(SyncError::LimitExceeded));check(&w,&f,&mut c,rect(0,0),false);
    }
    #[test]
    fn dense_edge_cells_fall_back_without_rejecting_a_small_valid_view(){
        let mut w=World::default();w.spawn(p(0)).unwrap();let mut extra=Vec::new();for _ in 0..MAX_VISIBLE_ENTITIES{extra.push(w.spawn(p(32)).unwrap());}let far=w.spawn(p(128)).unwrap();
        let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());let mut c=VisibleRows::default();let v=rect(0,0);f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);assert!(!c.complete);assert!(c.candidates.is_empty());assert_eq!(c.rows.len(),1);
        w.set_position(far,p(256)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,true);
        w.set_position(extra[0],p(33)).unwrap();f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);
        assert_eq!(f.collect_rows_cached(rect(0,33),&mut c),Err(SyncError::LimitExceeded));check(&w,&f,&mut c,v,false);
        for id in extra{w.despawn(id).unwrap();}f.capture(&w).unwrap();check(&w,&f,&mut c,v,false);assert!(c.complete);assert_eq!(c.candidates.len(),1);
    }
}
