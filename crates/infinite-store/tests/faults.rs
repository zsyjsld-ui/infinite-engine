#![cfg(feature="fault-injection")]
use infinite_store::*;use std::{fs,io,path::PathBuf,sync::Arc};
fn dir(label:&str)->PathBuf{let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m6/test-data");fs::create_dir_all(&root).unwrap();let stamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();root.join(format!("fault-{label}-{}-{stamp}",std::process::id()))}
fn put(id:u64,value:&[u8])->Change{Change{key:RecordKey{table:1,id},value:Some(value.into())}}
fn error()->io::Error{io::Error::other("injected storage failure")}
#[test]
fn prepublication_fault_keeps_prior_state_and_stops_writer(){
 for stage in [IoStage::BeforeCreate,IoStage::AfterWrite,IoStage::AfterFileSync,IoStage::BeforePublish]{let path=dir(&format!("{stage:?}"));let opts=JournalOptions::default();let mut j=Journal::create(&path,[11;16],opts).unwrap();let old=j.mark();j.set_fault_hook(Some(Arc::new(move|p|if p.stage==stage{Err(error())}else{Ok(())})));assert!(matches!(j.commit(old,&[put(1,b"a"),put(2,b"b")]),Err(JournalError::Io(_))));assert_eq!(j.mark(),old);assert!(j.records().is_empty());assert!(j.stopped());assert!(matches!(j.commit(old,&[]),Err(JournalError::Stopped)));assert_eq!(recover(&path,opts).unwrap().mark,old);assert!(!path.join("00000000000000000001.txn.tmp").exists());j.close().unwrap();}
}
#[test]
fn postpublication_fault_is_reported_as_committed_and_cannot_be_retried(){
 for stage in [IoStage::AfterPublish,IoStage::BeforeDirectorySync]{let path=dir(&format!("published-{stage:?}"));let opts=JournalOptions::default();let mut j=Journal::create(&path,[12;16],opts).unwrap();j.set_fault_hook(Some(Arc::new(move|p|if p.stage==stage{Err(error())}else{Ok(())})));let result=j.commit(j.mark(),&[put(1,b"a"),put(2,b"b")]);let Err(JournalError::Published{mark,..})=result else{panic!("published failure misclassified")};assert_eq!(mark.sequence,1);assert_eq!(j.mark(),mark);assert_eq!(j.records().len(),2);assert!(j.stopped());let r=recover(&path,opts).unwrap();assert_eq!(r.mark,mark);assert_eq!(r.records,j.records().clone());j.close().unwrap();}
}
#[test]
fn cleanup_failure_latches_writer_and_preserves_only_owned_incomplete_file(){
 let path=dir("cleanup");let opts=JournalOptions::default();let mut j=Journal::create(&path,[13;16],opts).unwrap();j.set_fault_hook(Some(Arc::new(|p|if matches!(p.stage,IoStage::BeforePublish|IoStage::BeforeCleanup){Err(error())}else{Ok(())})));assert!(matches!(j.commit(j.mark(),&[put(1,b"a")]),Err(JournalError::Cleanup{..})));assert!(j.stopped());assert!(path.join("00000000000000000001.txn.tmp").exists());assert_eq!(recover(&path,opts).unwrap().mark.sequence,0);j.close().unwrap();let j=Journal::open(&path,opts).unwrap();assert!(!path.join("00000000000000000001.txn.tmp").exists());j.close().unwrap();
}
#[test]
fn prepared_checkpoint_drop_cleanup_failure_is_visible_to_live_journal(){
 let path=dir("cp-drop");let opts=JournalOptions::default();let mut j=Journal::create(&path,[14;16],opts).unwrap();j.commit(j.mark(),&[put(1,b"a")]).unwrap();let prepared=j.begin_checkpoint().unwrap().unwrap().prepare().unwrap();j.set_fault_hook(Some(Arc::new(|p|if p.stage==IoStage::BeforeCleanup{Err(error())}else{Ok(())})));drop(prepared);assert!(j.stopped());assert!(matches!(j.commit(j.mark(),&[]),Err(JournalError::Stopped)));assert_eq!(recover(&path,opts).unwrap().mark.sequence,1);j.close().unwrap();Journal::open(&path,opts).unwrap().close().unwrap();
}
#[test]
fn checkpoint_publication_and_prune_faults_preserve_recovery_prefix(){
 for stage in [IoStage::AfterPublish,IoStage::BeforeDirectorySync,IoStage::BeforePrune,IoStage::AfterPrune]{let path=dir(&format!("cp-{stage:?}"));let opts=JournalOptions::default();let mut j=Journal::create(&path,[15;16],opts).unwrap();j.commit(j.mark(),&[put(1,b"a")]).unwrap();let job=j.begin_checkpoint().unwrap().unwrap();j.commit(j.mark(),&[put(2,b"b")]).unwrap();let prepared=job.prepare().unwrap();j.set_fault_hook(Some(Arc::new(move|p|if p.stage==stage{Err(error())}else{Ok(())})));assert!(j.finish_checkpoint(prepared).is_err());assert!(j.stopped());let r=recover(&path,opts).unwrap();assert_eq!(r.mark,j.mark());assert_eq!(r.records,j.records().clone());assert!(path.join("00000000000000000002.txn").exists());j.close().unwrap();Journal::open(&path,opts).unwrap().close().unwrap();}
}
#[test]
fn background_receipt_preserves_published_failure_and_rejects_later_work(){
 let path=dir("service");let opts=JournalOptions::default();let mut j=Journal::create(&path,[16;16],opts).unwrap();j.set_fault_hook(Some(Arc::new(|p|if p.stage==IoStage::BeforeDirectorySync{Err(error())}else{Ok(())})));let mut s=JournalService::start(j,ServiceOptions::default()).unwrap();let first=s.try_commit(None,vec![put(1,b"a")]).unwrap();let second=s.try_commit(None,vec![put(2,b"b")]).unwrap();let done=s.shutdown();done.worker.unwrap();assert_eq!(done.completions.len(),2);assert_eq!(done.completions[0].ticket,first.id());assert!(matches!(done.completions[0].kind,CompletionKind::Commit(Err(JournalError::Published{..}))));assert_eq!(done.completions[1].ticket,second.id());assert!(matches!(done.completions[1].kind,CompletionKind::Commit(Err(JournalError::Stopped))));let r=recover(path,opts).unwrap();assert_eq!(r.mark.sequence,1);assert_eq!(r.records.len(),1);
}
