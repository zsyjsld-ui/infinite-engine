use std::sync::Arc;
use infinite_protocol::{state_delta::{StateEncoder,StateHistory,HISTORY},wire::STATE_HEADER};
fn state(tick:u64,len:usize)->Arc<[u8]>{let mut b=vec![0;len];b[..8].copy_from_slice(b"NM02\x81\0\0\0");b[8..16].copy_from_slice(&19u64.to_le_bytes());b[16..24].copy_from_slice(&tick.to_le_bytes());b[48..52].copy_from_slice(&(tick as u32*11).to_le_bytes());b.into()}
#[test]
fn delayed_ack_coalesced_ticks_and_route_size_changes_are_lossless(){
 let mut sender=StateEncoder::default();let mut receiver=StateHistory::default();let mut delayed=None;let mut delta=0;
 for tick in (0..300).step_by(3){if tick%9==0{if let Some(t)=delayed{sender.acknowledge(19,t).unwrap();}}
  let full=state(tick,STATE_HEADER+if tick%39<18{80}else{16});let packet=sender.encode(&full).unwrap();delta+=usize::from(packet[4]==131);assert!(packet.len()<=full.len());let decoded=receiver.expand(&packet).unwrap();assert_eq!(decoded,full);
  sender.commit_sent(full).unwrap();receiver.commit(decoded).unwrap();delayed=Some(tick);
 }
 assert!(delta>80);assert!(receiver.len()<=HISTORY);
}
#[test]
fn only_sent_acknowledged_baselines_are_referenced_and_eviction_falls_back(){
 let mut sender=StateEncoder::default();let mut receiver=StateHistory::default();let first=state(0,STATE_HEADER);sender.commit_sent(first.clone()).unwrap();receiver.commit(first).unwrap();sender.acknowledge(19,0).unwrap();
 let prepared=state(1,STATE_HEADER);let packet=sender.encode(&prepared).unwrap();assert_eq!(packet[4],131);assert!(sender.acknowledge(19,1).is_err()); // Prepare/backpressure is not a send.
 for tick in 1..=HISTORY as u64{let s=state(tick,STATE_HEADER);sender.commit_sent(s).unwrap();}
 assert_eq!(sender.encode(&state(200,STATE_HEADER)).unwrap()[4],129);assert!(sender.acknowledge(20,127).is_err());assert!(sender.retained_bytes()<=HISTORY*STATE_HEADER);
 sender.acknowledge(19,128).unwrap();assert!(sender.acknowledge(19,127).is_err());
}
#[test]
fn truncated_masks_unknown_baselines_and_identity_corruption_are_rejected(){
 let mut sender=StateEncoder::default();let mut receiver=StateHistory::default();let first=state(0,STATE_HEADER);sender.commit_sent(first.clone()).unwrap();receiver.commit(first).unwrap();sender.acknowledge(19,0).unwrap();let packet=sender.encode(&state(1,STATE_HEADER)).unwrap();
 assert!(StateHistory::default().expand(&packet).is_err());for len in 0..packet.len(){assert!(receiver.expand(&packet[..len]).is_err());}
 for at in [5,6,7,8,16,24,32,36,38]{let mut bad=packet.clone();bad[at]^=128;assert!(receiver.expand(&bad).is_err(),"offset {at}");}
 let mut bad=packet.clone();bad[43]|=0x80;assert!(receiver.expand(&bad).is_err());let mut trailing=packet.clone();trailing.push(0);assert!(receiver.expand(&trailing).is_err());
 let mut seed=13u32;for len in 0..400{let mut b=vec![0;len];for v in &mut b{seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);*v=(seed>>24)as u8;}let _=receiver.expand(&b);}
 assert_eq!(receiver.len(),1);assert_eq!(receiver.expand(&packet).unwrap(),state(1,STATE_HEADER));
}
