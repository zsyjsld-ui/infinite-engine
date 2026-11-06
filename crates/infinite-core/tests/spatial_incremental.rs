use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};

fn verify(world:&World, frame:&mut SpatialFrame) {
    frame.capture(world).unwrap();
    for (x,y) in [(-300,-300),(-64,-64),(0,0),(127,55),(300,300)] {
        let view=ViewRect{min:GridPosition{x:x-150,y:y-150},max:GridPosition{x:x+150,y:y+150}};
        let mut expected:Vec<_>=world.motion_states().filter(|s|view.contains(s.position)).collect();
        expected.sort_unstable_by_key(|s|(s.entity.index,s.entity.generation));
        assert_eq!(frame.visible(view).unwrap(),expected);
    }
}

#[test]
fn persistent_buckets_follow_churn_and_dense_swaps() {
    let mut world=World::default();let mut frame=SpatialFrame::new(NonZeroU32::new(64).unwrap());
    let mut ids=Vec::new();let mut random=1234567u32;
    for step in 0..600 {
        random=random.wrapping_mul(1664525).wrapping_add(1013904223);
        let p=GridPosition{x:(random%900) as i32-450,y:((random>>12)%900) as i32-450};
        if ids.is_empty()||step%5==0 {ids.push(world.spawn(p).unwrap());}
        else {
            let at=random as usize%ids.len();let id=ids[at];
            match step%7 {
                0=>{world.despawn(id).unwrap();ids.swap_remove(at);},
                1=>{world.despawn(id).unwrap();ids[at]=world.spawn(p).unwrap();},
                2=>{world.move_to(id,p,NonZeroU16::new(2240).unwrap()).unwrap();},
                _=>world.set_position(id,p).unwrap(),
            }
        }
        if step%3==0 {world.advance_tick().unwrap();}
        verify(&world,&mut frame);
        // Repeated captures on the same tick must not duplicate bucket members.
        if step%11==0 {verify(&world,&mut frame);}
    }
    for id in ids {world.despawn(id).unwrap();verify(&world,&mut frame);}
    world.spawn(GridPosition{x:-64,y:64}).unwrap();verify(&world,&mut frame);
}

#[test]
fn bucket_head_middle_tail_and_status_only_updates() {
    let mut world=World::default();let mut frame=SpatialFrame::new(NonZeroU32::new(64).unwrap());
    let ids:Vec<_>=(0..12).map(|_|world.spawn(GridPosition{x:0,y:0}).unwrap()).collect();
    verify(&world,&mut frame);
    for (i,&id) in ids.iter().enumerate() {
        world.set_position(id,GridPosition{x:if i%2==0{-65}else{64},y:i as i32}).unwrap();verify(&world,&mut frame);
        world.move_to(id,GridPosition{x:200,y:200},NonZeroU16::new(1).unwrap()).unwrap();verify(&world,&mut frame);
    }
    for &id in ids.iter().rev() {world.despawn(id).unwrap();verify(&world,&mut frame);}
    // A caller can capture another World; stale bucket contents must disappear.
    let mut other=World::default();other.spawn(GridPosition{x:10,y:-10}).unwrap();verify(&other,&mut frame);
}

#[test]
fn negative_boundaries_and_extreme_jumps_keep_exact_membership() {
    let mut world=World::default();let id=world.spawn(GridPosition{x:0,y:0}).unwrap();
    let mut frame=SpatialFrame::new(NonZeroU32::new(64).unwrap());
    for x in [0,-1,-64,-65,63,64,65,i32::MIN,i32::MAX,0] {
        let p=GridPosition{x,y:x};world.set_position(id,p).unwrap();frame.capture(&world).unwrap();
        assert_eq!(frame.visible(ViewRect{min:p,max:p}).unwrap(),world.motion_states().collect::<Vec<_>>());
        let absent=GridPosition{x:x.wrapping_add(1),y:x};
        assert!(frame.visible(ViewRect{min:absent,max:absent}).unwrap().is_empty());
    }
}
