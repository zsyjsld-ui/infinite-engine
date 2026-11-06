//! The app chooses placement only after the core's respawn deadline.
use infinite_core::*;
use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn main(){
    let mut world=World::default();let monster=world.spawn(GridPosition::default()).unwrap();
    world.configure_health(monster,100,n(100)).unwrap();
    world.register_lifecycle(monster,LifecycleProfile{corpse_ticks:3,respawn_after:Some(6)}).unwrap();
    world.apply_damage(monster,100).unwrap();let mut output=Vec::new();let mut failed_placement=false;
    for _ in 0..9{
        loop {
            let report=world.resolve_lifecycle(1,&mut output).unwrap();assert!(report.blocked.is_none());
            for event in &output {
                println!("tick={} scheduled={} {:?}",world.tick().0,event.scheduled_at.0,event.kind);
                if let LifecycleEventKind::RespawnReady(ticket)=event.kind {
                    if !failed_placement {failed_placement=true;world.defer_respawn(ticket,n(1)).unwrap();println!("placement busy; retry next tick");}
                    else {let outcome=world.complete_respawn(ticket,GridPosition{x:64,y:0}).unwrap();assert_eq!(outcome.health.after.current,100);
                        println!("respawn tick={} position={:?} health={}",world.tick().0,world.position(monster).unwrap(),outcome.health.after.current);}
                }
            }
            if report.overdue.is_none()&&report.unscheduled==0{break;}
        }
        world.advance_tick().unwrap();
    }
    assert_eq!(world.lifecycle_state(monster).unwrap().phase,LifecyclePhase::Alive);assert_eq!(world.lifecycle_activity().queued,0);
}
