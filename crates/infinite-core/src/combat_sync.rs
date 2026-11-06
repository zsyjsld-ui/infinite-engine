//! Vital snapshots and ordered combat feedback, independent of motion wire v1.
use std::{collections::VecDeque,num::NonZeroU32};
use crate::{CombatEndReason,CombatEvent,CombatEventKind,EntityId,GridPosition,HealthState,
    SpatialFrame,SyncError,SyncHeader,Tick,ViewRect,World,MAX_ACTIVE_COMBATANTS,MAX_SYNC_ENTITIES};
use crate::sync::{key,VisibleRows};

pub const MAX_COMBAT_EVENTS_PER_PACKET:usize=256;
pub const MAX_PENDING_COMBAT_EVENTS:usize=16_384;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct WindupView { pub target:EntityId,pub started_at:Tick,pub hit_at:Tick }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct VitalState { pub entity:EntityId,pub health:Option<HealthState>,pub windup:Option<WindupView> }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum CombatFeedbackKind {
    WindupStarted { started_at:Tick,hit_at:Tick },
    WindupCancelled { started_at:Tick },
    Hit { damage:u32,remaining:u32 },
    Ended(CombatEndReason),
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CombatFeedback { pub tick:Tick,pub attacker:EntityId,pub target:EntityId,pub kind:CombatFeedbackKind }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct CombatSyncConfig { pub pending_events:usize,pub events_per_packet:usize }
/// Counts from one successfully committed observation, before packet encoding.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct CombatObserveReport {
    pub captured_events:usize,
    pub queued_events:usize,
    pub filtered_events:usize,
    pub skipped_blocks:usize,
    pub checked_events:usize,
}
impl Default for CombatSyncConfig {
    fn default()->Self{Self{pending_events:4096,events_per_packet:MAX_COMBAT_EVENTS_PER_PACKET}}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum CombatSyncError { State(SyncError),InvalidConfig,FrameGap,EventBackpressure,UnsupportedFeedback }
impl From<SyncError> for CombatSyncError { fn from(error:SyncError)->Self{Self::State(error)} }

/// Capture once after this tick's commands and combat phase. Use the returned
/// motion_frame for motion replication too: one spatial capture per World tick.
/// Dense component rows share the spatial frame's world iteration order.
pub struct CombatFrame {
    spatial:SpatialFrame, serial:u64, tick:Option<Tick>,
    states:Vec<CapturedVital>,events:Vec<CapturedFeedback>,event_blocks:Vec<Option<ViewRect>>,
}
#[derive(Clone,Copy)]
struct CapturedVital { state:VitalState,windup_target:Option<GridPosition> }
#[derive(Clone)]
struct CapturedFeedback { feedback:CombatFeedback,bounds:Option<ViewRect> }
// Preserve capture order. Blocks only reject spatially unrelated work; they
// never sort, combine, or discard individual visible combat feedback.
const EVENT_BLOCK_SIZE:usize=32;
impl CombatFrame {
    pub(crate) fn serial(&self)->u64{self.serial}
    pub(crate) fn frozen_copy(&self)->Self{Self{spatial:self.spatial.frozen_copy(),serial:self.serial,tick:self.tick,states:self.states.clone(),events:self.events.clone(),event_blocks:self.event_blocks.clone()}}
    pub fn new(cell_size:NonZeroU32)->Self{Self{spatial:SpatialFrame::new(cell_size),serial:0,tick:None,states:Vec::new(),events:Vec::new(),event_blocks:Vec::new()}}
    pub fn motion_frame(&self)->&SpatialFrame{&self.spatial}
    pub fn capture(&mut self,world:&World,events:&[CombatEvent])->Result<(),SyncError>{
        if world.len()>MAX_SYNC_ENTITIES||events.len()>MAX_ACTIVE_COMBATANTS{return Err(SyncError::LimitExceeded);}
        if self.tick.is_some_and(|t|world.tick()<=t){return Err(SyncError::StaleTick);}
        let serial=self.serial.checked_add(1).ok_or(SyncError::LimitExceeded)?;
        self.spatial.capture(world)?;
        // Both captures iterate the same immutable world, including dense swaps
        // after despawn. No allocation indexed by lifetime entity slot numbers.
        self.states.clear();
        for motion in world.motion_states(){
            let entity=motion.entity;
            let windup=world.attack_state(entity).ok().and_then(|s|s.windup).map(|w|WindupView{target:w.target,started_at:w.started_at,hit_at:w.hit_at});
            let windup_target=windup.and_then(|w|world.position(w.target).ok());
            self.states.push(CapturedVital{state:VitalState{entity,health:world.health(entity).ok(),windup},windup_target});
        }
        self.events.clear();
        for event in events{
            let kind=match event.kind{
                CombatEventKind::WindupStarted{windup,..}=>CombatFeedbackKind::WindupStarted{started_at:windup.started_at,hit_at:windup.hit_at},
                CombatEventKind::WindupCancelled{windup}=>CombatFeedbackKind::WindupCancelled{started_at:windup.started_at},
                CombatEventKind::Hit{health,..}=>CombatFeedbackKind::Hit{damage:health.before.current.saturating_sub(health.after.current),remaining:health.after.current},
                CombatEventKind::Ended(reason)=>CombatFeedbackKind::Ended(reason),
            };
            // Validate both generations once per frame. An event is visible
            // exactly when this rectangle fits inside the recipient's view.
            let bounds=world.position(event.attacker).ok().zip(world.position(event.target).ok()).map(|(a,b)|ViewRect{
                min:GridPosition{x:a.x.min(b.x),y:a.y.min(b.y)},max:GridPosition{x:a.x.max(b.x),y:a.y.max(b.y)},
            });
            self.events.push(CapturedFeedback{feedback:CombatFeedback{tick:world.tick(),attacker:event.attacker,target:event.target,kind},bounds});
        }
        self.event_blocks.clear();
        for block in self.events.chunks(EVENT_BLOCK_SIZE){
            let mut bounds:Option<ViewRect>=None;
            for event in block{if let Some(b)=event.bounds{
                bounds=Some(bounds.map_or(b,|a|ViewRect{
                    min:GridPosition{x:a.min.x.min(b.min.x),y:a.min.y.min(b.min.y)},
                    max:GridPosition{x:a.max.x.max(b.max.x),y:a.max.y.max(b.max.y)},
                }));
            }}
            self.event_blocks.push(bounds);
        }
        self.serial=serial;self.tick=Some(world.tick());Ok(())
    }
}

#[derive(Clone,Copy)]
pub(crate) struct VitalChange { before:u16,after:u16 }
// Snapshot rows, not entity slots. These indices are consumed by the encoder
// before either snapshot can change; every prepare rebuilds the list.
const _:()=assert!(crate::MAX_VISIBLE_ENTITIES<u16::MAX as usize);
impl VitalChange {
    fn new(before:Option<usize>,after:Option<usize>)->Self{
        Self{before:before.map_or(u16::MAX,|i|i as u16),after:after.map_or(u16::MAX,|i|i as u16)}
    }
    pub(crate) fn resolve<'a>(&self,baseline:&'a[VitalState],latest:&'a[VitalState])->(EntityId,Option<&'a VitalState>,Option<&'a VitalState>){
        let before=(self.before!=u16::MAX).then(||&baseline[usize::from(self.before)]);
        let after=(self.after!=u16::MAX).then(||&latest[usize::from(self.after)]);
        (before.or(after).unwrap().entity,before,after)
    }
}
/// One recipient. observe MUST run on every captured frame, including while an
/// older packet awaits ACK. prepare may drain feedback over several packets.
pub struct CombatSyncSender {
    compact:bool,batched:bool,shielded:bool,
    stream:u64,config:CombatSyncConfig,observed:u64,tick:Tick,last_observed:Option<Tick>,
    issued:u64,confirmed:u64,initialized:bool,
    baseline:Vec<VitalState>,latest:Vec<VitalState>,staged:Vec<VitalState>,
    visible:VisibleRows,candidate:Vec<VitalState>,feedback:Vec<CombatFeedback>,
    queued:VecDeque<CombatFeedback>,changes:Vec<VitalChange>,packet:Vec<u8>,
    baseline_observed:u64,pending:Option<(SyncHeader,usize,u64,bool)>,
}
impl CombatSyncSender {
    pub fn new(stream:u64,config:CombatSyncConfig)->Result<Self,CombatSyncError>{
        if config.pending_events==0||config.pending_events>MAX_PENDING_COMBAT_EVENTS||config.events_per_packet==0
            ||config.events_per_packet>MAX_COMBAT_EVENTS_PER_PACKET||config.events_per_packet>config.pending_events{return Err(CombatSyncError::InvalidConfig);}
        Ok(Self{compact:false,batched:false,shielded:false,stream,config,observed:0,tick:Tick(0),last_observed:None,issued:0,confirmed:0,initialized:false,
            baseline:Vec::new(),latest:Vec::new(),staged:Vec::new(),visible:VisibleRows::default(),candidate:Vec::new(),feedback:Vec::new(),
            queued:VecDeque::new(),changes:Vec::new(),packet:Vec::new(),baseline_observed:0,pending:None})
    }
    /// IC v2 requires a v2-capable peer. `new` retains legacy IC v1 output.
    /// Reserve its bounded maximum packet at construction, outside tick work.
    /// This costs about 263 KiB per recipient and prevents later packet growth
    /// when a new view or a wider integer exceeds an earlier packet's size.
    pub fn new_compact(stream:u64,config:CombatSyncConfig)->Result<Self,CombatSyncError>{
        let mut sender=Self::new(stream,config)?;sender.compact=true;
        sender.packet=Vec::with_capacity(crate::MAX_COMBAT_SYNC_PACKET_BYTES);Ok(sender)
    }
    /// Explicit IC v3 opt-in: requires a peer that decodes packet-local event
    /// reuse. Existing v1/v2 constructors retain their wire formats.
    pub fn new_batched(stream:u64,config:CombatSyncConfig)->Result<Self,CombatSyncError>{
        let mut sender=Self::new_compact(stream,config)?;sender.batched=true;Ok(sender)
    }
    /// IC v4 uses v3 layout and permits zero-health-loss and self-inflicted hits.
    /// Requires an explicitly v4-capable peer; existing constructors stay v1–v3.
    pub fn new_shielded(stream:u64,config:CombatSyncConfig)->Result<Self,CombatSyncError>{
        let mut sender=Self::new_batched(stream,config)?;sender.shielded=true;Ok(sender)
    }
    pub fn observe(&mut self,frame:&CombatFrame,view:ViewRect)->Result<(),CombatSyncError>{
        self.observe_report(frame,view).map(|_|())
    }
    /// Same atomic enqueue contract as observe, with visibility work counters.
    /// A failed observation can be retried on this frame; no report is committed.
    pub fn observe_report(&mut self,frame:&CombatFrame,view:ViewRect)->Result<CombatObserveReport,CombatSyncError>{
        self.observe_with_inflight(frame,view,0)
    }
    pub(crate) fn observe_with_inflight(&mut self,frame:&CombatFrame,view:ViewRect,inflight:usize)->Result<CombatObserveReport,CombatSyncError>{
        let tick=frame.tick.ok_or(SyncError::NoFrame)?;
        if self.observed!=0&&self.observed.checked_add(1)!=Some(frame.serial){return Err(CombatSyncError::FrameGap);}
        if self.last_observed.is_some_and(|old|tick<=old){return Err(SyncError::StaleTick.into());}
        frame.spatial.collect_rows_cached(view,&mut self.visible)?;
        self.feedback.clear();
        let mut report=CombatObserveReport{captured_events:frame.events.len(),..CombatObserveReport::default()};
        for (block,bounds) in frame.events.chunks(EVENT_BLOCK_SIZE).zip(&frame.event_blocks){
            let Some(b)=bounds else{report.skipped_blocks+=1;continue;};
            if b.max.x<view.min.x||b.max.y<view.min.y||b.min.x>view.max.x||b.min.y>view.max.y{
                report.skipped_blocks+=1;continue;
            }
            for event in block{
                report.checked_events+=1;
                if event.bounds.is_some_and(|b|view.contains(b.min)&&view.contains(b.max)){
                    if !self.shielded&&matches!(event.feedback.kind,CombatFeedbackKind::Hit{damage,..} if damage==0||event.feedback.attacker==event.feedback.target){return Err(CombatSyncError::UnsupportedFeedback);}
                    self.feedback.push(event.feedback);
                }
            }
        }
        if self.queued.len()+self.feedback.len()+inflight>self.config.pending_events{return Err(CombatSyncError::EventBackpressure);}
        self.candidate.clear();
        for &(entity,row) in &self.visible.rows{
            let captured=frame.states[row];let mut state=captured.state;
            debug_assert_eq!(state.entity,entity);
            if !captured.windup_target.is_some_and(|p|view.contains(p)){state.windup=None;}
            self.candidate.push(state);
        }
        // Commit only after view validation and event-capacity preflight.
        self.queued.extend(self.feedback.iter().copied());
        std::mem::swap(&mut self.latest,&mut self.candidate);
        self.observed=frame.serial;self.last_observed=Some(tick);self.tick=tick;
        report.queued_events=self.feedback.len();report.filtered_events=report.captured_events-report.queued_events;Ok(report)
    }
    pub fn queued_events(&self)->usize{self.queued.len()}
    pub fn pending_packet(&self)->Option<&[u8]>{self.pending.map(|_|self.packet.as_slice())}
    pub fn pending_header(&self)->Option<SyncHeader>{self.pending.map(|p|p.0)}
    pub(crate) fn pending_event_count(&self)->usize{self.pending.map_or(0,|p|p.1)}
    // Only the window owner may call this after retaining the exact packet and
    // event count. Advances the encoding baseline, not the transport ACK frontier.
    pub(crate) fn stage_inflight(&mut self){
        let h=self.pending_header().expect("prepared window packet");let mut ack=[0u8;20];ack[..4].copy_from_slice(b"CA\x01\x00");ack[4..12].copy_from_slice(&h.stream.to_le_bytes());ack[12..].copy_from_slice(&h.sequence.to_le_bytes());self.acknowledge_packet(&ack).unwrap();
    }
    pub fn prepare(&mut self)->Result<Option<&[u8]>,CombatSyncError>{
        if self.pending.is_some(){return Err(SyncError::WaitingForAck.into());}
        if self.observed==0{return Err(SyncError::NoFrame.into());}
        self.changes.clear();let(mut old,mut new)=(0,0);
        // After ACK, remaining event chunks use the same snapshot. Compare it
        // again only after another successful observe (or an explicit reset).
        while self.baseline_observed!=self.observed&&(old<self.baseline.len()||new<self.latest.len()){
            let a=self.baseline.get(old);let b=self.latest.get(new);
            let (before,after)=match(a,b){
                (Some(a),Some(b))if a.entity==b.entity=>{old+=1;new+=1;if a==b{continue;}(Some(old-1),Some(new-1))},
                (Some(a),Some(b))if key(a.entity)<key(b.entity)=>{old+=1;(Some(old-1),None)},
                (Some(_),None)=>{old+=1;(Some(old-1),None)},
                (_,Some(_))=>{new+=1;(None,Some(new-1))},
                (None,None)=>break,
            };
            self.changes.push(VitalChange::new(before,after));
        }
        if self.initialized&&self.changes.is_empty()&&self.queued.is_empty(){self.baseline_observed=self.observed;return Ok(None);}
        let sequence=self.issued.checked_add(1).ok_or(SyncError::LimitExceeded)?;
        let header=SyncHeader{stream:self.stream,sequence,base:self.confirmed,tick:self.tick,reset:!self.initialized};
        self.feedback.clear();self.feedback.extend(self.queued.iter().take(self.config.events_per_packet).copied());
        if self.batched{crate::combat_sync_wire::encode_batched(header,&self.changes,&self.baseline,&self.latest,&self.feedback,&mut self.packet);if self.shielded{self.packet[2]=4;}}
        else if self.compact{crate::combat_sync_wire::encode_compact(header,&self.changes,&self.baseline,&self.latest,&self.feedback,&mut self.packet);}
        else{crate::combat_sync_wire::encode(header,&self.changes,&self.baseline,&self.latest,&self.feedback,&mut self.packet);}
        let stage=!self.initialized||!self.changes.is_empty();
        if stage{self.staged.clear();self.staged.extend_from_slice(&self.latest);}
        self.issued=sequence;self.pending=Some((header,self.feedback.len(),self.observed,stage));Ok(Some(&self.packet))
    }
    pub fn acknowledge_packet(&mut self,bytes:&[u8])->Result<(),CombatSyncError>{
        let(stream,sequence)=crate::combat_sync_wire::decode_ack(bytes)?;
        if stream!=self.stream{return Err(SyncError::WrongStream.into());}
        if self.initialized&&sequence==self.confirmed{return Ok(());}
        let(header,count,observed,stage)=self.pending.ok_or(SyncError::OutOfOrder)?;
        if sequence!=header.sequence{return Err(SyncError::OutOfOrder.into());}
        if stage{std::mem::swap(&mut self.baseline,&mut self.staged);}
        self.baseline_observed=observed;
        drop(self.queued.drain(..count));self.confirmed=sequence;self.initialized=true;self.pending=None;Ok(())
    }
    /// Never discard an unacknowledged event packet. Retry exact bytes first;
    /// disconnected sessions require a fresh stream and explicit event-loss policy.
    pub fn request_full(&mut self)->Result<(),CombatSyncError>{
        if self.pending.is_some(){return Err(SyncError::WaitingForAck.into());}
        self.initialized=false;self.confirmed=0;self.baseline.clear();self.baseline_observed=0;Ok(())
    }
}
