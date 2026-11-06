//! Single-owner simulation. Transport supplies authenticated identities, never
//! client elapsed time, speed, position corrections or authoritative checkpoints.
#![forbid(unsafe_code)]

use std::{collections::{BTreeMap, VecDeque}, num::NonZeroU16};
use infinite_core::{CollisionMap, DEFAULT_TICK_MS, DirectionRemainder, EntityId,
    GridPosition, MovementStatus, NavigationStatus, RouteCheckpoint, Tick, World,
    WorldError, slide_integer};

pub type ControllerId = u64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlToken { pub id: ControllerId, pub epoch: u64 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input { Stop, Direction { x: i16, y: i16 }, Goal(GridPosition), KeepAlive }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Idle, Waiting, Running, Arrived, Blocked, Failed, Cancelled, Paused }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission { Queued, Duplicate }
#[derive(Debug)]
pub enum RuntimeError { Configuration, Capacity, UnknownController, Epoch, Sequence, Direction, World(WorldError) }
impl From<WorldError> for RuntimeError { fn from(e:WorldError)->Self { Self::World(e) } }

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub max_controllers: usize,
    pub commands_per_tick: usize,
    pub navigation_work: u32,
    /// Expiration is checked at the beginning of a simulation interval.
    pub lease_ticks: u32,
    pub arrival_radius: u32,
}
impl Default for Config {
    fn default()->Self { Self { max_controllers:1024, commands_per_tick:1024,
        navigation_work:8192, lease_ticks:20, arrival_radius:80 } }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorView {
    pub controller:ControllerId, pub entity:EntityId, pub epoch:u64,
    pub received_seq:u64, pub processed_seq:u64, pub intent_seq:u64,
    pub tick:Tick, pub position:GridPosition, pub phase:Phase, pub speed:u16, pub input:Input,
    pub direction_remainder:DirectionRemainder, pub arrival_radius:u32, pub lease_left:u32,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct StepReport { pub commands:usize, pub pending:usize, pub moved:usize,
    pub waiting:usize, pub expired:usize, pub navigation_work:u32, pub slide_queries:u64 }

struct Controller {
    entity:EntityId, epoch:u64, speed:u16, enabled:bool,
    received:u64, processed:u64, intent_seq:u64, last_input:Option<Input>,
    last_tick:u64, queued:bool, nav_queued:bool, pending:Option<(u64,Input)>,
    intent:Input, phase:Phase, remainder:DirectionRemainder,
}

/// World is the only position store. The controller holds intention/lease data,
/// not a second actor state to copy into World each frame. No socket or disk IO.
pub struct MovementRuntime {
    config:Config, world:World, actors:BTreeMap<ControllerId,Controller>,
    commands:VecDeque<ControllerId>, navigation:VecDeque<ControllerId>,
}
impl MovementRuntime {
    pub fn new(map:CollisionMap,config:Config)->Result<Self,RuntimeError> {
        if config.max_controllers==0||config.max_controllers>65536||config.commands_per_tick==0
            ||config.commands_per_tick>config.max_controllers||config.navigation_work<8192
            ||config.navigation_work>65536||config.lease_ticks==0 { return Err(RuntimeError::Configuration); }
        Ok(Self {config,world:World::with_map(DEFAULT_TICK_MS,map),actors:BTreeMap::new(),commands:VecDeque::new(),navigation:VecDeque::new()})
    }
    pub fn world(&self)->&World { &self.world }
    pub fn tick(&self)->Tick { self.world.tick() }
    /// Only the trusted application assigns an identity, epoch and speed.
    pub fn attach(&mut self,token:ControlToken,position:GridPosition,speed:u16)->Result<EntityId,RuntimeError> {
        if token.epoch==0||self.actors.contains_key(&token.id) { return Err(RuntimeError::Epoch); }
        if self.actors.len()>=self.config.max_controllers { return Err(RuntimeError::Capacity); }
        let entity=self.world.spawn(position)?;
        self.actors.insert(token.id,Controller {entity,epoch:token.epoch,speed,enabled:true,
            received:0,processed:0,intent_seq:0,last_input:None,last_tick:self.world.tick().0,
            queued:false,nav_queued:false,pending:None,intent:Input::Stop,phase:Phase::Idle,remainder:DirectionRemainder::default()});
        Ok(entity)
    }
    pub fn detach(&mut self,id:ControllerId)->Result<(),RuntimeError> {
        let a=self.actors.get(&id).ok_or(RuntimeError::UnknownController)?;
        self.world.despawn(a.entity)?;self.actors.remove(&id);
        // Lifecycle operations are cold; remove the old queue slot so a reused
        // application ID cannot inherit a former controller's scheduling entry.
        self.commands.retain(|&v|v!=id);self.navigation.retain(|&v|v!=id);Ok(())
    }
    pub fn renew(&mut self,token:ControlToken)->Result<(),RuntimeError> {
        let a=self.actors.get_mut(&token.id).ok_or(RuntimeError::UnknownController)?;
        if token.epoch<=a.epoch {return Err(RuntimeError::Epoch);}
        self.world.stop_moving(a.entity)?;
        a.epoch=token.epoch;a.received=0;a.processed=0;a.intent_seq=0;a.last_input=None;
        a.pending=None;a.intent=Input::Stop;a.phase=Phase::Cancelled;
        a.remainder=DirectionRemainder::default();a.last_tick=self.world.tick().0;
        // Existing queued membership is retained, preventing duplicate slots.
        Ok(())
    }
    pub fn set_motion_policy(&mut self,id:ControllerId,speed:u16,enabled:bool)->Result<(),RuntimeError> {
        let a=self.actors.get_mut(&id).ok_or(RuntimeError::UnknownController)?;
        a.speed=speed;a.enabled=enabled;Ok(())
    }
    /// Disconnect is scoped to the exact control epoch. Late close callbacks
    /// cannot cancel a newly connected owner of the same persistent actor.
    pub fn revoke(&mut self,token:ControlToken)->Result<(),RuntimeError> {
        let a=self.actors.get_mut(&token.id).ok_or(RuntimeError::UnknownController)?;
        if a.epoch!=token.epoch {return Err(RuntimeError::Epoch);}
        self.world.stop_moving(a.entity)?;a.intent=Input::Stop;a.pending=None;a.phase=Phase::Cancelled;
        a.remainder=DirectionRemainder::default();Ok(())
    }
    pub fn submit(&mut self,token:ControlToken,seq:u64,input:Input)->Result<Admission,RuntimeError> {
        let a=self.actors.get_mut(&token.id).ok_or(RuntimeError::UnknownController)?;
        if token.epoch!=a.epoch { return Err(RuntimeError::Epoch); }
        if seq==0||seq<a.received {return Err(RuntimeError::Sequence);}
        if seq==a.received {return if a.last_input==Some(input){Ok(Admission::Duplicate)}else{Err(RuntimeError::Sequence)};}
        if let Input::Direction{x,y}=input {
            if i64::from(x)*i64::from(x)+i64::from(y)*i64::from(y)>32767_i64.pow(2) {return Err(RuntimeError::Direction);}
        }
        a.received=seq;a.last_input=Some(input);a.last_tick=self.world.tick().0;
        // Heartbeats must not overwrite a not-yet-consumed goal/stop. Only
        // replaceable movement intentions share this bounded latest slot.
        if input!=Input::KeepAlive {a.pending=Some((seq,input));}
        if !a.queued {self.commands.push_back(token.id);a.queued=true;}
        Ok(Admission::Queued)
    }
    pub fn view(&self,id:ControllerId)->Result<ActorView,RuntimeError> {
        let a=self.actors.get(&id).ok_or(RuntimeError::UnknownController)?;
        Ok(ActorView {controller:id,entity:a.entity,epoch:a.epoch,received_seq:a.received,
            processed_seq:a.processed,intent_seq:a.intent_seq,tick:self.world.tick(),
            position:self.world.position(a.entity)?,phase:if !a.enabled||a.speed==0 {Phase::Paused}else{a.phase},speed:if a.enabled{a.speed}else{0},input:a.intent,
            direction_remainder:a.remainder,arrival_radius:self.config.arrival_radius,
            lease_left:self.config.lease_ticks.saturating_sub(self.world.tick().0.saturating_sub(a.last_tick).min(u64::from(u32::MAX))as u32)})
    }
    pub fn checkpoint(&self,id:ControllerId)->Result<Option<RouteCheckpoint>,RuntimeError> {
        let a=self.actors.get(&id).ok_or(RuntimeError::UnknownController)?;
        Ok(self.world.route_checkpoint(a.entity)?)
    }
    /// Exactly one server-owned 50ms interval. Input floods cannot create extra
    /// intervals or replenish planning work; the owner controls wall-clock pace.
    pub fn step(&mut self)->Result<StepReport,RuntimeError> {
        let mut report=StepReport::default();let now=self.world.tick().0;
        self.world.restrict_navigation_work(self.config.navigation_work);
        for _ in 0..self.config.commands_per_tick {
            let Some(id)=self.commands.pop_front() else {break};
            let a=self.actors.get_mut(&id).ok_or(RuntimeError::UnknownController)?;a.queued=false;
            if let Some((seq,input))=a.pending.take() {
                let changed=a.intent!=input||matches!(a.phase,Phase::Failed|Phase::Blocked|Phase::Cancelled)||input==Input::Stop;
                a.intent_seq=seq;
                if changed {
                self.world.stop_moving(a.entity)?;a.remainder=DirectionRemainder::default();
                a.intent=input;a.phase=match input {
                    Input::Stop=>Phase::Cancelled,
                    Input::Direction{x:0,y:0}=>Phase::Idle,
                    Input::Direction{..}=>Phase::Running,
                    Input::Goal(_)=>Phase::Waiting,
                    Input::KeepAlive=>unreachable!(),
                };
                if matches!(input,Input::Goal(_))&&!a.nav_queued {
                    self.navigation.push_back(id);a.nav_queued=true;
                }
                }
            }
            a.processed=a.received;report.commands+=1;
        }
        // A paused zero-speed goal starts only after trusted speed is restored.
        for a in self.actors.values_mut() {
            if now.saturating_sub(a.last_tick)>=u64::from(self.config.lease_ticks)&&a.intent!=Input::Stop {
                self.world.stop_moving(a.entity)?;a.intent=Input::Stop;a.phase=Phase::Cancelled;
                a.remainder=DirectionRemainder::default();report.expired+=1;
            }
        }
        // Core search slots are bounded. Keep excess requests in a bounded FIFO
        // (one per attached actor), rather than letting new low IDs jump ahead.
        for _ in 0..self.navigation.len() {
            let id=self.navigation.pop_front().unwrap();let a=self.actors.get_mut(&id).ok_or(RuntimeError::UnknownController)?;
            let Input::Goal(goal)=a.intent else {a.nav_queued=false;continue;};
            let Some(speed)=NonZeroU16::new(a.speed).filter(|_|a.enabled) else {self.navigation.push_back(id);continue;};
            match self.world.queue_controlled_navigation(a.entity,goal,speed,self.config.arrival_radius) {
                Ok(())=>a.nav_queued=false,
                Err(WorldError::NavigationQueueFull)=>{self.navigation.push_front(id);break;},
                Err(WorldError::BlockedPosition|WorldError::Navigation(_))=>{a.phase=Phase::Failed;a.nav_queued=false;},
                Err(e)=>return Err(e.into()),
            }
        }
        // Controlled entities are not moved by World::advance_tick. Planning
        // happens here, then each actor consumes this interval exactly once.
        self.world.advance_tick()?;report.navigation_work=self.world.last_navigation_budget().used();
        for a in self.actors.values_mut() {
            if !a.enabled||a.speed==0 {continue;}
            let before=self.world.position(a.entity)?;
            match a.intent {
                Input::Direction{x,y} if x!=0||y!=0=>{
                    let delta=a.remainder.displacement(i32::from(x),i32::from(y),a.speed,50_000).map_err(|_|RuntimeError::Direction)?;
                    let map=self.world.collision_map().expect("runtime map");
                    let sample=slide_integer(before,delta,|from,to|map.segment_is_clear(from,to)).map_err(|_|RuntimeError::Direction)?;
                    self.world.set_position(a.entity,sample.position)?;report.slide_queries+=u64::from(sample.queries);
                    a.phase=if sample.position==before&&delta!=(GridPosition{x:0,y:0}){Phase::Blocked}else{Phase::Running};
                }
                Input::Goal(_) if a.phase!=Phase::Failed=>{
                    let state=self.world.entity_state(a.entity)?;
                    match state.navigation {
                        NavigationStatus::Pending{..}=>{a.phase=Phase::Waiting;report.waiting+=1;},
                        NavigationStatus::Failed{..}=>a.phase=Phase::Failed,
                        NavigationStatus::Ready{..}=>{
                            self.world.controlled_route_lite(a.entity,50_000,NonZeroU16::new(a.speed).unwrap())?;
                            a.phase=match self.world.entity_state(a.entity)?.movement {
                                MovementStatus::Arrived=>Phase::Arrived,MovementStatus::Blocked=>Phase::Blocked,_=>Phase::Running,
                            };
                        }
                        _=>{},
                    }
                }
                _=>{},
            }
            if self.world.position(a.entity)?!=before {report.moved+=1;}
        }
        report.pending=self.commands.len();report.waiting=self.actors.values().filter(|a|a.phase==Phase::Waiting).count();Ok(report)
    }
}
