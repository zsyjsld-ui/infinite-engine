use infinite_store::*;use std::{fs,path::PathBuf,sync::atomic::{AtomicU64,Ordering}};
fn dir(label:&str)->PathBuf{static NEXT:AtomicU64=AtomicU64::new(0);let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m6/test-data");fs::create_dir_all(&root).unwrap();let run=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();root.join(format!("{label}-{}-{run}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)))}
fn put(id:u64,data:&[u8])->Change{Change{key:RecordKey{table:1,id},value:Some(data.into())}}
#[test]
fn seeded_checkpoint_has_zero_sequence_and_preflights_before_directory_creation(){
 let path=dir("seed");let mut records=Records::new();records.insert(RecordKey{table:1,id:1},b"a".as_slice().into());records.insert(RecordKey{table:1,id:2},b"bc".as_slice().into());let opts=JournalOptions{max_live_bytes:3,..JournalOptions::default()};let j=Journal::create_with_records(&path,[20;16],records.clone(),opts).unwrap();assert_eq!(j.mark().sequence,0);j.close().unwrap();let r=recover(&path,opts).unwrap();assert_eq!(r.records,records);assert_eq!(r.live_bytes,3);assert_eq!(r.replayed,0);let rejected=dir("seed-limit");assert!(matches!(Journal::create_with_records(&rejected,[20;16],records,JournalOptions{max_live_bytes:2,..opts}),Err(JournalError::LiveLimit)));assert!(!rejected.exists());
}
#[test]
fn atomic_changes_empty_delete_and_reopen_preserve_state(){
 let path=dir("roundtrip");let opts=JournalOptions::default();let mut j=Journal::create(&path,[1;16],opts).unwrap();let first=j.commit(j.mark(),&[put(1,b"a"),put(2,b"b")]).unwrap();let second=j.commit(first.mark,&[Change{key:RecordKey{table:1,id:1},value:None},put(2,b"")]).unwrap();assert_eq!(second.mark.sequence,2);assert_eq!(j.records().len(),1);assert!(j.records()[&RecordKey{table:1,id:2}].is_empty());let expected=j.records().clone();let mark=j.mark();j.close().unwrap();let r=recover(&path,opts).unwrap();assert_eq!(r.mark,mark);assert_eq!(r.records,expected);assert_eq!(r.replayed,2);assert_eq!(r.live_bytes,0);let j=Journal::open(&path,opts).unwrap();assert_eq!(j.retained_log_files(),2);assert_eq!(j.records(),&expected);j.close().unwrap();
}
#[test]
fn checkpoint_prunes_only_covered_history_and_allows_new_commits(){
 let path=dir("checkpoint");let opts=JournalOptions::default();let mut j=Journal::create(&path,[2;16],opts).unwrap();for i in 1..=3{j.commit(j.mark(),&[put(i,&[i as u8])]).unwrap();}j.checkpoint().unwrap();assert_eq!(j.retained_log_files(),0);assert_eq!(j.retained_log_bytes(),0);assert!(path.join("00000000000000000003.checkpoint").exists());assert!(!path.join("00000000000000000000.checkpoint").exists());j.commit(j.mark(),&[put(2,b"new")]).unwrap();let expected=j.records().clone();j.close().unwrap();let r=recover(&path,opts).unwrap();assert_eq!(r.records,expected);assert_eq!(r.mark.sequence,4);assert_eq!(r.replayed,1);
}
#[test]
fn high_water_requires_checkpoint_and_live_admission_is_atomic(){
 let path=dir("bounded");let opts=JournalOptions{max_log_files:1,max_live_bytes:3,..JournalOptions::default()};let mut j=Journal::create(&path,[3;16],opts).unwrap();j.commit(j.mark(),&[put(1,b"a")]).unwrap();let mark=j.mark();assert!(matches!(j.commit(mark,&[put(2,b"b")]),Err(JournalError::HighWater)));assert_eq!(j.mark(),mark);assert!(!j.stopped());j.checkpoint().unwrap();assert!(matches!(j.commit(mark,&[put(1,b"aa"),put(2,b"bb")]),Err(JournalError::LiveLimit)));assert_eq!(j.records().len(),1);assert_eq!(j.records()[&RecordKey{table:1,id:1}].as_ref(),b"a");j.commit(mark,&[put(1,b""),put(2,b"bbb")]).unwrap();j.close().unwrap();assert_eq!(recover(path,opts).unwrap().live_bytes,3);
}
#[test]
fn locks_existing_directories_and_foreign_entries_are_not_overwritten(){
 let path=dir("lock");let opts=JournalOptions::default();let j=Journal::create(&path,[4;16],opts).unwrap();assert!(matches!(Journal::open(&path,opts),Err(JournalError::Locked)));assert!(Journal::create(&path,[5;16],opts).is_err());j.close().unwrap();fs::write(path.join("foreign.txt"),b"untouched").unwrap();assert!(matches!(recover(&path,opts),Err(JournalError::ForeignEntry)));assert!(matches!(Journal::open(&path,opts),Err(JournalError::ForeignEntry)));assert_eq!(fs::read(path.join("foreign.txt")).unwrap(),b"untouched");assert!(!path.join("writer.lock").exists());
}
#[test]
fn published_truncation_and_gaps_fail_read_only_recovery(){
 let path=dir("corrupt");let opts=JournalOptions::default();let mut j=Journal::create(&path,[6;16],opts).unwrap();j.commit(j.mark(),&[put(1,b"a")]).unwrap();j.commit(j.mark(),&[put(2,b"b")]).unwrap();j.close().unwrap();let log=path.join("00000000000000000001.txn");let original=fs::read(&log).unwrap();fs::write(&log,&original[..original.len()-1]).unwrap();let broken=fs::read(&log).unwrap();assert!(recover(&path,opts).is_err());assert_eq!(fs::read(&log).unwrap(),broken);fs::write(&log,&original).unwrap();fs::remove_file(log).unwrap();assert!(matches!(recover(&path,opts),Err(JournalError::Codec(StoreError::SequenceGap))));
}
#[test]
fn unfinished_temp_is_ignored_read_only_then_cleaned_by_exclusive_open(){
 let path=dir("temporary");let opts=JournalOptions::default();let mut j=Journal::create(&path,[7;16],opts).unwrap();j.commit(j.mark(),&[put(1,b"safe")]).unwrap();let mark=j.mark();j.close().unwrap();let temp=path.join("00000000000000000002.txn.tmp");fs::write(&temp,b"partial").unwrap();assert_eq!(recover(&path,opts).unwrap().mark,mark);assert!(temp.exists());let j=Journal::open(&path,opts).unwrap();assert!(!temp.exists());assert_eq!(j.mark(),mark);j.close().unwrap();
}
#[test]
fn stale_mark_and_duplicate_keys_never_create_a_transaction(){
 let path=dir("preflight");let opts=JournalOptions::default();let mut j=Journal::create(&path,[8;16],opts).unwrap();let old=j.mark();j.commit(old,&[put(1,b"a")]).unwrap();assert!(matches!(j.commit(old,&[put(1,b"b")]),Err(JournalError::Codec(StoreError::ChainMismatch))));assert!(matches!(j.commit(j.mark(),&[put(1,b"a"),put(1,b"b")]),Err(JournalError::Codec(StoreError::UnsortedKeys))));assert_eq!(j.mark().sequence,1);assert!(!path.join("00000000000000000002.txn").exists());j.close().unwrap();
}
