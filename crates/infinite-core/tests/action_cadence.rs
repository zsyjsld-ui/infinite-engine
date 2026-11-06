use infinite_core::*;
use std::num::NonZeroU32;
fn ledger()->ActionCadence{ActionCadence::new(CadenceGrace{maximum:80,divisor:NonZeroU32::new(10).unwrap()})}
#[test]
fn grace_anchors_old_deadline_but_authorized_cancel_resets_from_now(){
    let mut c=ledger();let t=CadenceTiming{cooldown:1000,recovery:Some((1,400))};
    c.accept_authorized(0,t,Tick(1000)).unwrap();
    assert!(!c.readiness(0,t,Tick(1919)).cooldown);assert!(c.readiness(0,t,Tick(1920)).cooldown);
    c.accept_authorized(0,t,Tick(1920)).unwrap();assert_eq!(c.cooldowns()[0].ready,Tick(3000));
    // Explicit game policy may authorize reduced-damage recovery cancellation.
    c.accept_authorized(0,t,Tick(2400)).unwrap();assert_eq!(c.cooldowns()[0].ready,Tick(3400));
}
#[test]
fn failed_accept_does_not_publish_partial_cooldown_or_last_action(){
    let mut c=ledger();c.accept_authorized(1,CadenceTiming{cooldown:1,recovery:None},Tick(3)).unwrap();
    assert_eq!(c.accept_authorized(2,CadenceTiming{cooldown:0,recovery:Some((1,5))},Tick(u64::MAX-1)),Err(CadenceError::TimeOverflow));
    assert_eq!(c.last_skill(),Some(1));assert_eq!(c.cooldowns().len(),1);assert!(c.recoveries().is_empty());
    for i in 2..=32{c.accept_authorized(i,CadenceTiming{cooldown:1,recovery:None},Tick(4)).unwrap();}
    assert_eq!(c.accept_authorized(33,CadenceTiming{cooldown:1,recovery:None},Tick(4)),Err(CadenceError::SkillCapacity));
    assert_eq!(c.last_skill(),Some(32));
}
