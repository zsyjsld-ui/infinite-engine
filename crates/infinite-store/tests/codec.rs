use infinite_store::*;
use sha2::{Digest,Sha256};
fn base()->JournalMark { JournalMark::initial([7;16]).unwrap() }
fn put(table:u16,id:u64,value:&[u8])->Change {Change{key:RecordKey{table,id},value:Some(value.into())}}
fn seal(bytes:&mut Vec<u8>) {let end=bytes.len()-32;let hash=Sha256::digest(&bytes[..end]);bytes[end..].copy_from_slice(&hash);}
#[test]
fn node_crypto_golden_transaction_and_extreme_keys(){
 let changes=[put(3,300,&[0,1,255])];let(bytes,mark)=encode_transaction(base(),&changes,Limits::default()).unwrap();
 assert_eq!(bytes.len(),104);assert_eq!(&bytes[64..72],&[1,3,172,2,3,0,1,255]);
 let digest=mark.digest.iter().map(|b|format!("{b:02x}")).collect::<String>();assert_eq!(digest,"7968bc194e772faa98c6fec1794c15313ea55ea3797dc9a644cf3d16f942c129");
 let decoded=decode_frame(&bytes,Limits::default()).unwrap();assert_eq!(decoded.changes,changes);assert_eq!(decoded.successor(base()).unwrap(),mark);
 let changes=[put(0,0,&[]),Change{key:RecordKey{table:0,id:u64::MAX},value:None},put(u16::MAX,u64::MAX,&[255])];let(bytes,next)=encode_transaction(mark,&changes,Limits::default()).unwrap();let frame=decode_frame(&bytes,Limits::default()).unwrap();assert_eq!(frame.changes,changes);assert_eq!(frame.successor(mark).unwrap(),next);
}
#[test]
fn checkpoint_carries_commit_anchor_and_replays_next_transaction(){
 let(records,first)=encode_transaction(base(),&[put(1,1,b"a")],Limits::default()).unwrap();let tx=decode_frame(&records,Limits::default()).unwrap();let snapshot=encode_checkpoint(first,&tx.changes,Limits::default()).unwrap();let snapshot=decode_frame(&snapshot,Limits::default()).unwrap();assert_ne!(snapshot.checksum,first.digest);assert_eq!(snapshot.checkpoint_mark().unwrap(),first);
 let(bytes,second)=encode_transaction(first,&[Change{key:RecordKey{table:1,id:1},value:None}],Limits::default()).unwrap();assert_eq!(decode_frame(&bytes,Limits::default()).unwrap().successor(snapshot.checkpoint_mark().unwrap()).unwrap(),second);
 assert_eq!(snapshot.successor(base()),Err(StoreError::Malformed));assert_eq!(tx.checkpoint_mark(),Err(StoreError::Malformed));assert!(encode_checkpoint(base(),&[],Limits::default()).is_ok());assert_eq!(encode_checkpoint(first,&[Change{key:RecordKey{table:0,id:0},value:None}],Limits::default()),Err(StoreError::Malformed));
}
#[test]
fn truncation_bit_corruption_and_trailing_bytes_never_decode(){
 let(bytes,_)=encode_transaction(base(),&[put(1,9,&[42;127]),put(2,130,&[])],Limits::default()).unwrap();for cut in 0..bytes.len(){assert!(decode_frame(&bytes[..cut],Limits::default()).is_err(),"cut {cut}");}for index in 0..bytes.len(){let mut bad=bytes.clone();bad[index]^=1;assert!(decode_frame(&bad,Limits::default()).is_err(),"bit at {index}");}
 let mut trailing=bytes.clone();trailing.insert(trailing.len()-32,0);seal(&mut trailing);assert_eq!(decode_frame(&trailing,Limits::default()),Err(StoreError::Malformed));
}
#[test]
fn valid_hash_cannot_hide_noncanonical_keys_counts_or_lengths(){
 let(bytes,_)=encode_transaction(base(),&[put(1,1,&[])],Limits::default()).unwrap();let mut noncanonical=bytes.clone();noncanonical.splice(65..66,[0x81,0]);seal(&mut noncanonical);assert_eq!(decode_frame(&noncanonical,Limits::default()),Err(StoreError::Malformed));
 let mut overflow=bytes.clone();overflow.splice(66..67,[0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,2]);seal(&mut overflow);assert_eq!(decode_frame(&overflow,Limits::default()),Err(StoreError::Malformed));
 let mut table=bytes.clone();table.splice(65..66,[0x80,0x80,4]);seal(&mut table);assert_eq!(decode_frame(&table,Limits::default()),Err(StoreError::Malformed));
 let mut count=bytes.clone();count[60..64].copy_from_slice(&1000u32.to_le_bytes());seal(&mut count);assert_eq!(decode_frame(&count,Limits::default()),Err(StoreError::Truncated));
 let mut length=bytes.clone();length[67]=127;seal(&mut length);assert_eq!(decode_frame(&length,Limits::default()),Err(StoreError::Truncated));
 let mut kind=bytes;kind[64]=2;seal(&mut kind);assert_eq!(decode_frame(&kind,Limits::default()),Err(StoreError::Malformed));
}
#[test]
fn duplicate_unsorted_keys_and_limits_fail_before_encoding(){
 assert_eq!(encode_transaction(base(),&[put(1,1,b"a"),put(1,1,b"b")],Limits::default()),Err(StoreError::UnsortedKeys));assert_eq!(encode_transaction(base(),&[put(2,1,b"a"),put(1,1,b"b")],Limits::default()),Err(StoreError::UnsortedKeys));
 let limits=Limits{max_records:2,max_transaction_records:1,max_record_bytes:1,..Limits::default()};assert_eq!(encode_transaction(base(),&[put(1,1,b"ab")],limits),Err(StoreError::RecordTooLarge));assert_eq!(encode_transaction(base(),&[put(1,1,b"a"),put(1,2,b"b")],limits),Err(StoreError::RecordLimit));assert!(encode_checkpoint(base(),&[put(1,1,b"a"),put(1,2,b"b")],limits).is_ok());
 let limits=Limits{max_transaction_bytes:96,..limits};assert!(encode_transaction(base(),&[],limits).is_ok());assert_eq!(encode_transaction(base(),&[put(1,1,&[])],limits),Err(StoreError::FrameTooLarge));
 let limits=Limits{max_records:0,..limits};assert_eq!(encode_transaction(base(),&[],limits),Err(StoreError::InvalidLimits));
}
#[test]
fn history_sequence_and_digest_replay_checks_are_separate(){
 let(bytes,mark)=encode_transaction(base(),&[],Limits::default()).unwrap();let frame=decode_frame(&bytes,Limits::default()).unwrap();assert_eq!(frame.successor(JournalMark::initial([8;16]).unwrap()),Err(StoreError::WrongHistory));assert_eq!(frame.successor(mark),Err(StoreError::SequenceGap));
 let(bytes,_)=encode_transaction(mark,&[],Limits::default()).unwrap();let frame=decode_frame(&bytes,Limits::default()).unwrap();let bad=JournalMark{digest:[1;32],..mark};assert_eq!(frame.successor(bad),Err(StoreError::ChainMismatch));assert_eq!(encode_transaction(JournalMark{sequence:u64::MAX,..mark},&[],Limits::default()),Err(StoreError::SequenceExhausted));assert_eq!(JournalMark::initial([0;16]),Err(StoreError::InvalidHistory));assert_eq!(encode_transaction(JournalMark{digest:[1;32],..base()},&[],Limits::default()),Err(StoreError::Malformed));
}
