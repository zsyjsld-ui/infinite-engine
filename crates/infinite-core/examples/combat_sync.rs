//! In-memory transport demonstration; not a running game service.
use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn main(){
    let mut world=World::default();
    let attacker=world.spawn(GridPosition{x:0,y:0}).unwrap();
    let target=world.spawn(GridPosition{x:64,y:0}).unwrap();
    let hidden=world.spawn(GridPosition{x:10000,y:0}).unwrap();
    for id in [attacker,target,hidden]{world.configure_health(id,50,n(50)).unwrap();}
    let profile=AttackProfile{range:96,damage:n(10),cooldown_ticks:NonZeroU16::new(2).unwrap(),windup_ticks:2};
    world.start_attack(attacker,target,profile).unwrap();
    let mut frame=CombatFrame::new(n(512));
    let config=CombatSyncConfig{events_per_packet:2,..CombatSyncConfig::default()};
    let mut sender=if std::env::args().any(|a|a=="--batched"){CombatSyncSender::new_batched(17,config)}
        else if std::env::args().any(|a|a=="--compact"){CombatSyncSender::new_compact(17,config)}
        else{CombatSyncSender::new(17,config)}.unwrap();
    let mut client=CombatReplica::new(17);
    let view=ViewRect{min:GridPosition{x:-512,y:-512},max:GridPosition{x:512,y:512}};
    let h=world.health(target).unwrap();
    let mut events=[CombatEvent{attacker,target,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};4];
    let mut changes=[HealthChange{entity:target,before:h,after:h};4];
    let mut delayed_ack:Option<[u8;20]>=None;
    for step in 1..=16{
        if step==6{world.set_position(target,GridPosition{x:256,y:0}).unwrap();}
        if step==8{world.set_position(target,GridPosition{x:64,y:0}).unwrap();}
        if step==10{world.stop_attack(attacker).unwrap();} // Interrupt the windup started on tick 9.
        if step==11{world.start_attack(attacker,target,profile).unwrap();}
        world.advance_tick().unwrap();let combat=world.resolve_combat(&mut events,&mut changes).unwrap();
        frame.capture(&world,&events[..combat.events_written]).unwrap();
        // Call even while an older packet is awaiting ACK. Motion SyncSender
        // can use frame.motion_frame() without capturing/indexing again.
        sender.observe(&frame,view).unwrap();
        if step==3{
            if let Some(ack)=delayed_ack.take(){
                let report=client.apply(sender.pending_packet().unwrap()).unwrap();
                println!("retry duplicate={} newly_delivered_events={}",report.duplicate,report.events);
                sender.acknowledge_packet(&ack).unwrap();
            }
        }
        while sender.pending_packet().is_none(){
            let Some(packet)=sender.prepare().unwrap()else{break;};
            let bytes=packet.to_vec(); // Stand-in for transport, no sockets here.
            let applied=client.apply(&bytes).unwrap();
            println!("tick={step} packet={} bytes={} events={}",applied.sequence,bytes.len(),applied.events);
            for event in client.drain_feedback(){println!("  feedback: {event:?}");}
            for state in client.states(){println!("  state: {state:?}");}
            let ack=applied.acknowledgement();
            if step==1{delayed_ack=Some(ack);break;} // Keep observing ticks 2/3.
            sender.acknowledge_packet(&ack).unwrap();
        }
        println!("  retained_feedback={}",sender.queued_events());
    }
}
