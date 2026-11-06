//! A fifth, unsubscribed sparse map shares the server clock and save queue.
//! It wakes only for authoritative deadlines while the four busy maps run.
use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
pub struct DormantMap { world:World,target:EntityId,pub wakes:usize,pub sleeping_ticks:usize }
impl DormantMap {
    pub fn new()->Self{
        let mut world=World::default();let target=world.spawn(GridPosition::default()).unwrap();
        world.configure_health(target,100,NonZeroU32::new(100).unwrap()).unwrap();
        for i in 1..64{world.spawn(GridPosition{x:i*16,y:0}).unwrap();}
        world.apply_periodic(PeriodicEffect{source:target,target,key:1,kind:PeriodicKind::Damage,
            amount:NonZeroU32::new(2).unwrap(),first_delay:20,interval:NonZeroU16::new(20).unwrap(),
            pulses:NonZeroU16::new(3).unwrap(),refresh:PeriodicRefresh::Replace}).unwrap();
        Self{world,target,wakes:0,sleeping_ticks:0}
    }
    pub fn update(&mut self,tick:Tick)->bool{
        let state=self.world.map_activity(MapWork::default());assert!(!state.pending&&!state.continuous);
        let due=state.next_due.is_some_and(|due|due<=tick);
        if due{
            assert_eq!(state.next_due,Some(tick),"never skip an authority deadline");
            self.world.skip_idle_to(tick,MapWork::default()).unwrap();
            let mut effects=Vec::new();let mut combat=Vec::new();
            let report=self.world.resolve_effects(4,&mut effects,&mut combat).unwrap();
            assert_eq!(report.pulses,1);assert!(report.overdue.is_none()&&report.blocked.is_none());
            assert_eq!(combat.len(),1);self.wakes+=1;
        }else{self.sleeping_ticks+=1;}
        assert_eq!(self.world.health(self.target).unwrap().current,100-2*(tick.0/20).min(3)as u32);
        due
    }
    pub fn record(&self)->Vec<u8>{
        let mut bytes=self.world.tick().0.to_le_bytes().to_vec();bytes.extend_from_slice(&self.world.health(self.target).unwrap().current.to_le_bytes());bytes
    }
    pub fn verify(&self,ticks:u64){assert_eq!(self.wakes,((ticks-1)/20).min(3)as usize);assert_eq!(self.wakes+self.sleeping_ticks,ticks as usize);}
}
