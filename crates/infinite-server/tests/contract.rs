use infinite_core::{GridPosition as P,RouteCheckpoint};
use infinite_runtime::{Config,ControlToken,MovementRuntime,Input};
use infinite_server::{asset::MapAsset,service,wire::{self,ClientFrame}};
fn map_bytes()->Vec<u8>{let mut b=vec![0;40];b[..8].copy_from_slice(b"IMAP\x01\x00\x03\x00");for(at,n)in[(8,8u32),(12,8),(16,768),(20,192)]{b[at..at+4].copy_from_slice(&n.to_le_bytes());}b}
fn input()->Vec<u8>{let mut b=vec![0;40];b[..4].copy_from_slice(b"NM02");b[4]=2;b[8]=9;b[16]=1;b}
#[test]
fn strict_map_shape_padding_and_geometry_identity(){
    let b=map_bytes();let a=MapAsset::decode(&b).unwrap();assert!(a.map.segment_is_clear(P{x:768,y:768},P{x:1536,y:768}));
    for len in 0..b.len(){assert!(MapAsset::decode(&b[..len]).is_err());}
    let mut blocked=b.clone();blocked[32+4]|=1<<1;let c=MapAsset::decode(&blocked).unwrap();assert_ne!(a.hash,c.hash);assert!(!c.map.segment_is_clear(P{x:768,y:768},P{x:1536,y:768}));
    let mut padded=b.clone();padded[8..12].copy_from_slice(&3u32.to_le_bytes());padded[12..16].copy_from_slice(&3u32.to_le_bytes());padded.truncate(34);padded[33]=0x80;assert!(MapAsset::decode(&padded).is_err());
    let mut extreme=b;extreme[16..20].copy_from_slice(&u32::MAX.to_le_bytes());assert!(MapAsset::decode(&extreme).is_err());
}
#[test]
fn input_shape_does_not_admit_speed_elapsed_or_unbounded_vectors(){
    let b=input();assert_eq!(wire::decode(&b).unwrap(),ClientFrame::Input{epoch:9,seq:1,input:Input::Stop});
    for len in 0..b.len(){assert!(wire::decode(&b[..len]).is_err());}
    for index in [5,6,7,25,26,27,36,37,38,39]{let mut v=b.clone();v[index]=1;assert!(wire::decode(&v).is_err());}
    let mut v=b.clone();v[24]=1;v[28..32].copy_from_slice(&32767i32.to_le_bytes());v[32..36].copy_from_slice(&32767i32.to_le_bytes());assert!(wire::decode(&v).is_err());
    v[32..36].fill(0);assert!(wire::decode(&v).is_ok());v.push(0);assert!(wire::decode(&v).is_err());
    // Arbitrary bounded input never panics, including malformed lengths/headers.
    let mut seed=7u32;for len in 0..300{let mut data=vec![0;len];for b in &mut data{seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);*b=(seed>>24)as u8;}let _=wire::decode(&data);}
}
#[test]
fn authority_packet_preserves_epoch_sequences_and_exact_route_progress(){
    let mut r=MovementRuntime::new(MapAsset::decode(&map_bytes()).unwrap().map,Config::default()).unwrap();
    let token=ControlToken{id:17,epoch:999};r.attach(token,P{x:768,y:768},3200).unwrap();r.submit(token,42,Input::Direction{x:32767,y:0}).unwrap();r.step().unwrap();
    let v=r.view(17).unwrap();let cursor=RouteCheckpoint{position:v.position,origin:P{x:768,y:768},progress:1234567890123,remainder:12345,radius:80,waypoints:vec![P{x:1536,y:768},P{x:1536,y:960}]};
    let b=wire::state(v,Some(&cursor),9000);let q=|at|u64::from_le_bytes(b[at..at+8].try_into().unwrap());let d=|at|u32::from_le_bytes(b[at..at+4].try_into().unwrap());
    assert_eq!(b.len(),wire::STATE_HEADER+56);assert_eq!(&b[..5],b"NM02\x81");assert_eq!((q(8),q(16),q(24),q(32),q(40)),(999,1,42,42,42));
    assert_eq!((d(48),d(52)),(928,768));assert_eq!((q(76),q(wire::STATE_HEADER+20)),(9000,cursor.progress));assert_eq!(d(wire::STATE_HEADER+28),12345);assert_eq!(d(wire::STATE_HEADER+36),2);
    assert_eq!(wire::state(v,None,9000).len(),wire::STATE_HEADER);
    let w=wire::welcome(999,17,123,&[5;32]);assert_eq!(w.len(),72);assert_eq!(&w[40..],[5;32]);
}
#[test]
fn accounts_are_bounded_unique_synthetic_identities(){
    let line=format!("1 {} 768 768 3200","ab".repeat(32));assert_eq!(service::accounts(&line).unwrap().len(),1);
    assert!(service::accounts(&(line.clone()+"\n"+&line)).is_err());assert!(service::accounts("").is_err());
    assert!(service::accounts(&format!("1 {} 768 768 3200","00".repeat(32))).is_err());
    assert!(service::accounts(&format!("1 {} 768 768 3200","é".repeat(32))).is_err());
}
