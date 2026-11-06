use infinite_core::*;
fn p(x:i32,y:i32)->GridPosition{GridPosition{x,y}}
#[test]
fn direction_carry_is_bounded_and_packet_subdivision_preserves_free_motion(){
    let mut small=DirectionRemainder::default();let mut whole=small;let mut sum=p(0,0);
    for _ in 0..1000{let d=small.displacement(23169,-23169,3200,200).unwrap();sum.x+=d.x;sum.y+=d.y;}
    assert_eq!(sum,whole.displacement(23169,-23169,3200,200_000).unwrap());
    let saved=small;assert_eq!(small.displacement(i32::MIN,i32::MIN,1,1),Err(DirectionError::InvalidDirection));assert_eq!(small,saved);
    assert_eq!(small.displacement(32767,32767,1,1),Err(DirectionError::InvalidDirection));
}
#[test]
fn straight_wall_keeps_tangent_component_and_head_on_does_not_choose_a_side(){
    let allowed=|_:GridPosition,b:GridPosition|b.x<160;
    let h=slide_integer(p(80,80),p(160,0),allowed).unwrap();assert!(h.position.x<160);assert_eq!(h.position.y,80);
    let diagonal=slide_integer(p(80,80),p(160,40),allowed).unwrap();
    assert!(diagonal.position.x<160);assert!(diagonal.position.y<=120);assert!(diagonal.position.y>80);assert!(diagonal.queries<=256);
}
#[test]
fn published_segments_share_staggered_collision_oracle(){
    use std::num::NonZeroU32 as N;
    let mut map=CollisionMap::new_staggered(p(0,0),N::new(10).unwrap(),N::new(10).unwrap(),N::new(768).unwrap(),N::new(192).unwrap()).unwrap();
    map.set_blocked(4,4,true).unwrap();
    for start in [p(2304,768),p(2688,576),p(2688,960)]{
        for delta in [p(800,0),p(800,300),p(800,-300),p(-800,0)]{
            let result=slide_integer(start,delta,|a,b|map.segment_is_clear(a,b)).unwrap();let mut last=start;
            for point in result.segments.into_iter().flatten(){assert!(map.segment_is_clear(last,point));last=point;}
            assert_eq!(last,result.position);assert!(result.queries<=256);
        }
    }
}
