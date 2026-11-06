//! Offline common-workload driver; fixture bytes are provided by the application.
use std::{fs, num::NonZeroU32 as N, time::Instant};
use infinite_core::{CollisionMap,GridPosition as P};
use infinite_runtime::{Config,ControlToken,Input,MovementRuntime,Phase};
fn n(v:u32)->N {N::new(v).unwrap()}
fn trip(r:&mut MovementRuntime,q:&[i32],epoch:u64)->(u64,u64,u64,u64,u64){
    let token=ControlToken{id:1,epoch};r.attach(token,P{x:q[0],y:q[1]},3200).unwrap();
    r.submit(token,1,Input::Goal(P{x:q[2],y:q[3]})).unwrap();
    let(mut steps,mut waits,mut work,mut first)=(0,0,0,0);let mut previous=P{x:q[0],y:q[1]};let mut trace=14695981039346656037_u64;
    loop {let report=r.step().unwrap();steps+=1;waits+=report.waiting as u64;work+=u64::from(report.navigation_work);
        assert!(report.navigation_work<=65536);let state=r.view(1).unwrap();
        if first==0&&state.position!=(P{x:q[0],y:q[1]}){first=steps;}
        if state.position!=previous {for v in [state.position.x,state.position.y] {trace=(trace^u64::from(v as u32)).wrapping_mul(1099511628211);}previous=state.position;}
        if state.phase==Phase::Arrived {let dx=i64::from(state.position.x-q[2]);let dy=i64::from(state.position.y-q[3]);assert!(dx*dx+dy*dy<=80*80);break;}
        assert!(!matches!(state.phase,Phase::Failed|Phase::Blocked),"route failed {q:?}");assert!(steps<20000,"route timeout {q:?}");
    }
    r.detach(1).unwrap();(steps,waits,work,first,trace)
}
fn main(){
    let path=std::env::args().nth(1).expect("fixture path");let data=fs::read_to_string(path).unwrap();let mut lines=data.lines();
    let size:Vec<u32>=lines.next().unwrap().split_whitespace().map(|n|n.parse().unwrap()).collect();
    let bitmap=lines.next().unwrap().as_bytes();assert_eq!(bitmap.len(),(size[0]*size[1])as usize);
    let mut map=CollisionMap::new_staggered_connected(P{x:0,y:0},n(size[0]),n(size[1]),n(768),n(192)).unwrap();
    for (i,b) in bitmap.iter().enumerate(){assert!(matches!(b,b'0'|b'1'));if *b==b'1'{map.set_blocked(i as u32%size[0],i as u32/size[0],true).unwrap();}}
    map.prepare_navigation();let queries:Vec<Vec<i32>>=lines.map(|line|{let row:Vec<_>=line.split_whitespace().map(|v|v.parse().unwrap()).collect();assert_eq!(row.len(),4);row}).collect();
    let navigation_work=std::env::args().nth(2).map(|v|v.parse().unwrap()).unwrap_or(8192);
    let mut r=MovementRuntime::new(map,Config{lease_ticks:20000,navigation_work,..Config::default()}).unwrap();let mut epoch=1;
    for q in &queries {trip(&mut r,q,epoch);epoch+=1;}
    let(mut steps,mut waits,mut work,mut first_sum,mut first_max)=(0,0,0,0,0);let mut trace=0_u64;let started=Instant::now();
    for _ in 0..3 {for q in &queries {let(s,w,b,f,h)=trip(&mut r,q,epoch);epoch+=1;steps+=s;waits+=w;work+=b;first_sum+=f;first_max=first_max.max(f);trace=trace.rotate_left(1)^h;}}
    println!("{{\"requests\":{},\"completed\":{},\"steps\":{},\"waitingTicks\":{},\"navigationWork\":{},\"firstMotionTicksSum\":{},\"firstMotionTicksMax\":{},\"navigationLimit\":{},\"trace\":\"{}\",\"elapsedMs\":{}}}",queries.len()*3,queries.len()*3,steps,waits,work,first_sum,first_max,navigation_work,trace,started.elapsed().as_secs_f64()*1000.);
}
