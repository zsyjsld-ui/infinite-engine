use infinite_core::*;
use std::num::NonZeroU32;
fn rect()->ViewRect{ViewRect{min:GridPosition{x:-100,y:-100},max:GridPosition{x:100,y:100}}}
fn frame(w:&World)->SpatialFrame{let mut f=SpatialFrame::new(NonZeroU32::new(64).unwrap());f.capture(w).unwrap();f}
fn deliver(s:&mut SyncSender,r:&mut Replica,f:&SpatialFrame,v:ViewRect)->usize{
    let bytes=s.prepare(f,v).unwrap().unwrap();let n=bytes.len();let ack=r.apply(bytes).unwrap();s.acknowledge(ack.stream,ack.sequence).unwrap();n
}
#[test]
fn full_delta_coalescing_leave_and_generation_reuse(){
    let mut w=World::default();let e=w.spawn(GridPosition{x:0,y:0}).unwrap();w.spawn(GridPosition{x:5000,y:5000}).unwrap();
    let(mut s,mut r)=(SyncSender::new(4),Replica::new(4));assert_eq!(deliver(&mut s,&mut r,&frame(&w),rect()),58);
    assert!(s.prepare(&frame(&w),rect()).unwrap().is_none());
    w.set_position(e,GridPosition{x:7,y:0}).unwrap();w.set_position(e,GridPosition{x:12,y:0}).unwrap();w.advance_tick().unwrap();
    assert_eq!(deliver(&mut s,&mut r,&frame(&w),rect()),51);assert_eq!(r.states()[0].position.x,12);
    w.despawn(e).unwrap();let replacement=w.spawn(GridPosition{x:-3,y:-4}).unwrap();assert_eq!(replacement.index,e.index);assert_ne!(replacement.generation,e.generation);
    deliver(&mut s,&mut r,&frame(&w),rect());assert_eq!(r.states(),frame(&w).visible(rect()).unwrap());
    w.set_position(replacement,GridPosition{x:999,y:999}).unwrap();assert_eq!(deliver(&mut s,&mut r,&frame(&w),rect()),49);assert!(r.states().is_empty());
}
#[test]
fn lost_packet_ack_retry_out_of_order_and_reset(){
    let mut w=World::default();let e=w.spawn(GridPosition{x:0,y:0}).unwrap();let(mut s,mut r)=(SyncSender::new(9),Replica::new(9));
    let first=s.prepare(&frame(&w),rect()).unwrap().unwrap().to_vec();
    w.set_position(e,GridPosition{x:5,y:0}).unwrap();assert_eq!(s.prepare(&frame(&w),rect()),Err(SyncError::WaitingForAck));assert_eq!(s.pending_packet().unwrap(),first);
    let ack=r.apply(&first).unwrap();assert!(r.apply(&first).unwrap().duplicate);s.acknowledge(9,ack.sequence).unwrap();
    let delta=s.prepare(&frame(&w),rect()).unwrap().unwrap().to_vec();
    assert_eq!(Replica::new(9).apply(&delta),Err(SyncError::OutOfOrder));
    s.request_full();let reset=s.prepare(&frame(&w),rect()).unwrap().unwrap().to_vec();let ack=r.apply(&reset).unwrap();s.acknowledge(9,ack.sequence).unwrap();
    assert_eq!(r.apply(&delta),Err(SyncError::OutOfOrder));assert_eq!(r.states(),frame(&w).visible(rect()).unwrap());
    assert_eq!(Replica::new(10).apply(&reset),Err(SyncError::WrongStream));
}
#[test]
fn malformed_packets_are_atomic(){
    let mut w=World::default();let e=w.spawn(GridPosition{x:0,y:0}).unwrap();let(mut s,mut r)=(SyncSender::new(1),Replica::new(1));deliver(&mut s,&mut r,&frame(&w),rect());
    w.set_position(e,GridPosition{x:2,y:3}).unwrap();let p=s.prepare(&frame(&w),rect()).unwrap().unwrap().to_vec();let before=r.states().to_vec();let seq=r.sequence();
    for len in 0..p.len(){assert!(r.apply(&p[..len]).is_err());assert_eq!(r.states(),before);assert_eq!(r.sequence(),seq);}
    for (at,value) in [(2,9),(3,128),(38,1),(48,128)]{let mut bad=p.clone();bad[at]=value;assert!(r.apply(&bad).is_err());assert_eq!(r.states(),before);}
    let mut trailing=p.clone();trailing.push(0);assert!(r.apply(&trailing).is_err());assert_eq!(r.states(),before);
    let ack=r.apply(&p).unwrap();s.acknowledge(1,ack.sequence).unwrap();assert_eq!(r.states()[0].position,GridPosition{x:2,y:3});
}
#[test]
fn signed_coordinates_absolute_fallback_and_independent_observers(){
    let mut w=World::default();let e=w.spawn(GridPosition{x:i32::MIN+20,y:-1}).unwrap();
    let mut f=SpatialFrame::new(NonZeroU32::new(u32::MAX).unwrap());f.capture(&w).unwrap();
    let v=ViewRect{min:GridPosition{x:i32::MIN,y:i32::MIN},max:GridPosition{x:i32::MAX,y:i32::MAX}};
    let(mut a,mut ar,mut b,mut br)=(SyncSender::new(1),Replica::new(1),SyncSender::new(2),Replica::new(2));
    deliver(&mut a,&mut ar,&f,v);deliver(&mut b,&mut br,&f,rect());assert!(br.states().is_empty());
    w.set_position(e,GridPosition{x:i32::MAX-20,y:25}).unwrap();f.capture(&w).unwrap();deliver(&mut a,&mut ar,&f,v);assert_eq!(ar.states(),f.visible(v).unwrap());
    w.set_position(e,GridPosition{x:-10,y:0}).unwrap();f.capture(&w).unwrap();deliver(&mut b,&mut br,&f,rect());assert_eq!(br.states(),f.visible(rect()).unwrap());
}
#[test]
fn view_boundaries_and_visibility_limit(){
    let mut w=World::default();for p in [(-100,-100),(100,100),(-101,0),(0,101)]{w.spawn(GridPosition{x:p.0,y:p.1}).unwrap();}
    let f=frame(&w);assert_eq!(f.visible(rect()).unwrap().len(),2);
    assert_eq!(f.visible(ViewRect{min:GridPosition{x:2,y:0},max:GridPosition{x:1,y:0}}),Err(SyncError::InvalidView));
    for _ in 0..MAX_VISIBLE_ENTITIES{w.spawn(GridPosition{x:0,y:0}).unwrap();}
    let mut s=SyncSender::new(1);assert_eq!(s.prepare(&frame(&w),rect()),Err(SyncError::LimitExceeded));assert!(s.pending_packet().is_none());
}
#[test]
fn status_only_and_stale_snapshot(){
    let mut w=World::default();let e=w.spawn(GridPosition{x:0,y:0}).unwrap();let(mut s,mut r)=(SyncSender::new(1),Replica::new(1));let old=frame(&w);deliver(&mut s,&mut r,&old,rect());
    w.move_to(e,GridPosition{x:5,y:0},std::num::NonZeroU16::new(1).unwrap()).unwrap();w.advance_tick().unwrap();let now=frame(&w);
    assert_eq!(deliver(&mut s,&mut r,&now,rect()),50);assert_eq!(r.states()[0].movement,MovementStatus::Moving);
    assert_eq!(s.prepare(&old,rect()),Err(SyncError::StaleTick));
}

#[test]
fn acknowledgement_and_empty_reset_are_checked(){
    let mut w=World::default();w.spawn(GridPosition{x:0,y:0}).unwrap();let(mut s,mut r)=(SyncSender::new(8),Replica::new(8));
    let bytes=s.prepare(&frame(&w),rect()).unwrap().unwrap().to_vec();let ack=r.apply(&bytes).unwrap().acknowledgement();
    let mut bad=ack;bad[2]=2;assert_eq!(s.acknowledge_packet(&bad),Err(SyncError::Malformed));assert!(s.pending_packet().is_some());
    s.acknowledge_packet(&ack).unwrap();s.request_full();
    let empty=ViewRect{min:GridPosition{x:1000,y:1000},max:GridPosition{x:1001,y:1001}};
    assert_eq!(deliver(&mut s,&mut r,&frame(&w),empty),40);assert!(r.states().is_empty());assert_eq!(r.apply(&bytes),Err(SyncError::OutOfOrder));
}

#[test]
fn delta_overflow_is_rejected_without_partial_application(){
    let mut w=World::default();let e=w.spawn(GridPosition{x:i32::MAX-1,y:0}).unwrap();
    let v=ViewRect{min:GridPosition{x:i32::MAX-10,y:-1},max:GridPosition{x:i32::MAX,y:1}};
    let(mut s,mut r)=(SyncSender::new(1),Replica::new(1));deliver(&mut s,&mut r,&frame(&w),v);
    w.set_position(e,GridPosition{x:i32::MAX,y:0}).unwrap();let mut bytes=s.prepare(&frame(&w),v).unwrap().unwrap().to_vec();
    bytes[49..51].copy_from_slice(&32767i16.to_le_bytes());let original=r.states().to_vec();
    assert_eq!(r.apply(&bytes),Err(SyncError::Malformed));assert_eq!(r.states(),original);
}

#[test]
fn unchanged_frame_advances_stale_frame_guard(){
    let mut w=World::default();let e=w.spawn(GridPosition{x:0,y:0}).unwrap();
    let(mut s,mut r)=(SyncSender::new(1),Replica::new(1));deliver(&mut s,&mut r,&frame(&w),rect());
    w.advance_tick().unwrap();w.set_position(e,GridPosition{x:1,y:0}).unwrap();let old=frame(&w);
    w.advance_tick().unwrap();w.set_position(e,GridPosition{x:0,y:0}).unwrap();
    assert!(s.prepare(&frame(&w),rect()).unwrap().is_none());
    assert_eq!(s.prepare(&old,rect()),Err(SyncError::StaleTick));
}

#[test]
fn full_visibility_replacement_fits_packet_limit(){
    let mut w=World::default();
    for _ in 0..MAX_VISIBLE_ENTITIES {w.spawn(GridPosition{x:0,y:0}).unwrap();w.spawn(GridPosition{x:1000,y:0}).unwrap();}
    let(mut s,mut r)=(SyncSender::new(1),Replica::new(1));let f=frame(&w);
    deliver(&mut s,&mut r,&f,rect());
    let other=ViewRect{min:GridPosition{x:900,y:-100},max:GridPosition{x:1100,y:100}};
    assert_eq!(deliver(&mut s,&mut r,&f,other),MAX_SYNC_PACKET_BYTES);
    assert_eq!(r.states(),f.visible(other).unwrap());
}
