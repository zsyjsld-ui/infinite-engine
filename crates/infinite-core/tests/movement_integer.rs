use infinite_core::{World,GridPosition as P,MovementStatus,CollisionMap,DEFAULT_TICK_MS};
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}

#[test]
fn projection_matches_wide_integer_reference_including_extreme_endpoints() {
    let mut seed=0x91b641d9u32;
    let mut next=||{seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);seed as i32};
    let mut cases=vec![(P{x:i32::MIN,y:i32::MIN},P{x:i32::MAX,y:i32::MAX},50_000_000,65000),
        (P{x:i32::MAX,y:i32::MIN},P{x:i32::MIN,y:i32::MAX},50_000_000,65000)];
    for _ in 0..1000 {cases.push((P{x:next(),y:next()},P{x:next(),y:next()},50,2240));}
    for (a,b,ms,speed) in cases {
        let dx=i128::from(b.x)-i128::from(a.x);let dy=i128::from(b.y)-i128::from(a.y);
        let squared=(dx*dx+dy*dy) as u128;let floor=squared.isqrt();let length=floor+u128::from(floor*floor<squared);
        if length==0{continue;}
        let mut world=World::new(n(ms));let entity=world.spawn(a).unwrap();world.move_to(entity,b,NonZeroU16::new(speed).unwrap()).unwrap();
        for tick in 1..=8u128 {
            world.advance_tick().unwrap();let progress=(u128::from(speed)*u128::from(ms)*tick/1000).min(length);
            let expected=P{x:(i128::from(a.x)+dx*progress as i128/length as i128) as i32,y:(i128::from(a.y)+dy*progress as i128/length as i128) as i32};
            assert_eq!(world.position(entity).unwrap(),expected);
        }
    }
}

#[test]
fn slow_motion_keeps_fractional_credit_and_stops_at_obstacle() {
    let mut map=CollisionMap::new(P{x:0,y:0},n(4),n(4),n(16)).unwrap();map.set_blocked(2,1,true).unwrap();
    let mut world=World::with_map(DEFAULT_TICK_MS,map);let id=world.spawn(P{x:31,y:24}).unwrap();
    world.move_to(id,P{x:56,y:24},NonZeroU16::new(1).unwrap()).unwrap();
    for _ in 0..19 {world.advance_tick().unwrap();assert_eq!(world.position(id).unwrap(),P{x:31,y:24});assert_eq!(world.movement_status(id).unwrap(),MovementStatus::Moving);}
    world.advance_tick().unwrap();assert_eq!(world.position(id).unwrap(),P{x:31,y:24});assert_eq!(world.movement_status(id).unwrap(),MovementStatus::Blocked);
}
