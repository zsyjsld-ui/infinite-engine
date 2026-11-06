#![cfg(feature="fault-injection")]
use infinite_store::*;use std::{fs,path::PathBuf,process::Command,thread,time::{Duration,Instant}};
#[test]
fn process_exit_at_fourteen_commit_and_checkpoint_boundaries_recovers_atomically(){
 let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m6/test-data");fs::create_dir_all(&root).unwrap();let stamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();let opts=JournalOptions::default();let phases=["BeforeCreate","AfterWrite","AfterFileSync","BeforePublish","AfterPublish","BeforeDirectorySync","BeforePrune","AfterPrune"];let mut cases=0;
 for kind in ["transaction","checkpoint"]{for phase in phases{if kind=="transaction"&&matches!(phase,"BeforePrune"|"AfterPrune"){continue;}let path=root.join(format!("process-{kind}-{phase}-{}-{stamp}",std::process::id()));let mut j=Journal::create(&path,[17;16],opts).unwrap();j.commit(j.mark(),&[Change{key:RecordKey{table:1,id:1},value:Some(b"before".as_slice().into())}]).unwrap();j.close().unwrap();
  let mut child=Command::new(env!("CARGO_BIN_EXE_store-crash-worker")).arg(&path).args([kind,phase]).spawn().unwrap();let until=Instant::now()+Duration::from_secs(10);let status=loop{if let Some(s)=child.try_wait().unwrap(){break s;}if Instant::now()>=until{child.kill().unwrap();child.wait().unwrap();panic!("fixture did not terminate: {kind} {phase}");}thread::sleep(Duration::from_millis(5));};assert_eq!(status.code(),Some(73),"{kind} {phase}");
  assert!(path.join("writer.lock").exists());let expected=if kind=="checkpoint"||matches!(phase,"AfterPublish"|"BeforeDirectorySync"){2}else{1};let r=recover(&path,opts).unwrap();assert_eq!(r.mark.sequence,expected,"{kind} {phase}");assert_eq!(r.records.len(),expected as usize);assert_eq!(r.records[&RecordKey{table:1,id:1}].as_ref(),if expected==1{b"before".as_slice()}else{b"after".as_slice()});if expected==2{assert_eq!(r.records[&RecordKey{table:1,id:2}].as_ref(),b"new");}
  assert!(matches!(Journal::open(&path,opts),Err(JournalError::Locked)));
  // Only this exact fixture child owned the lock; wait above proved it exited.
  fs::remove_file(path.join("writer.lock")).unwrap();let j=Journal::open(&path,opts).unwrap();assert_eq!(j.mark(),r.mark);assert_eq!(j.records(),&r.records);j.close().unwrap();cases+=1;
 }}assert_eq!(cases,14);
}
