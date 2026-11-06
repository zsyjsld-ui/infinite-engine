use infinite_store::*;use std::{fs,path::PathBuf,thread};
fn dir(label:&str)->PathBuf{let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m6/test-data");fs::create_dir_all(&root).unwrap();let stamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();root.join(format!("cp-{label}-{}-{stamp}",std::process::id()))}
fn put(id:u64,value:&[u8])->Change{Change{key:RecordKey{table:1,id},value:Some(value.into())}}
#[test]
fn old_prepared_checkpoint_keeps_newer_transactions_and_recovers_latest(){
 let path=dir("concurrent");let opts=JournalOptions::default();let mut j=Journal::create(&path,[1;16],opts).unwrap();j.commit(j.mark(),&[put(1,b"before"),put(2,b"keep")]).unwrap();let job=j.begin_checkpoint().unwrap().unwrap();assert_eq!(job.mark().sequence,1);assert!(matches!(j.begin_checkpoint(),Err(JournalError::CheckpointBusy)));let worker=thread::spawn(move||job.prepare());
 j.commit(j.mark(),&[put(1,b"after"),put(3,b"new")]).unwrap();j.commit(j.mark(),&[Change{key:RecordKey{table:1,id:2},value:None}]).unwrap();let prepared=worker.join().unwrap().unwrap();assert_eq!(prepared.mark().sequence,1);j.finish_checkpoint(prepared).unwrap();assert_eq!(j.retained_log_files(),2);assert!(path.join("00000000000000000002.txn").exists());assert!(path.join("00000000000000000003.txn").exists());assert!(!path.join("00000000000000000001.txn").exists());let expected=j.records().clone();let mark=j.mark();j.close().unwrap();let r=recover(&path,opts).unwrap();assert_eq!(r.mark,mark);assert_eq!(r.records,expected);assert_eq!(r.replayed,2);
}
#[test]
fn snapshot_lease_keeps_writer_exclusion_after_journal_drop(){
 let path=dir("lease");let opts=JournalOptions::default();let mut j=Journal::create(&path,[2;16],opts).unwrap();j.commit(j.mark(),&[put(1,b"data")]).unwrap();let job=j.begin_checkpoint().unwrap().unwrap();drop(j);assert!(matches!(Journal::open(&path,opts),Err(JournalError::Locked)));let prepared=job.prepare().unwrap();assert!(path.join("00000000000000000001.checkpoint.tmp").exists());drop(prepared);assert!(!path.join("00000000000000000001.checkpoint.tmp").exists());let j=Journal::open(&path,opts).unwrap();assert_eq!(j.mark().sequence,1);j.close().unwrap();
}
#[test]
fn foreign_job_rejected_without_publishing_either_history(){
 let opts=JournalOptions::default();let a_path=dir("a");let b_path=dir("b");let mut a=Journal::create(&a_path,[3;16],opts).unwrap();let mut b=Journal::create(&b_path,[4;16],opts).unwrap();a.commit(a.mark(),&[put(1,b"a")]).unwrap();let job=a.begin_checkpoint().unwrap().unwrap().prepare().unwrap();assert!(matches!(b.finish_checkpoint(job),Err(JournalError::ForeignCheckpoint)));assert_eq!(a.retained_log_files(),1);assert_eq!(b.mark().sequence,0);assert!(!a_path.join("00000000000000000001.checkpoint").exists());a.checkpoint().unwrap();a.close().unwrap();b.close().unwrap();assert_eq!(recover(a_path,opts).unwrap().replayed,0);
}
#[test]
fn closing_with_live_job_cannot_release_its_writer_lock(){
 let path=dir("close");let opts=JournalOptions::default();let mut j=Journal::create(&path,[5;16],opts).unwrap();j.commit(j.mark(),&[put(1,b"a")]).unwrap();let job=j.begin_checkpoint().unwrap().unwrap();assert!(matches!(j.close(),Err(JournalError::CheckpointBusy)));assert!(matches!(Journal::open(&path,opts),Err(JournalError::Locked)));drop(job);Journal::open(&path,opts).unwrap().close().unwrap();
}
