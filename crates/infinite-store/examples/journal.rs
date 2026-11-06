use infinite_store::*;
use std::path::PathBuf;
fn put(id:u64,value:&[u8])->Change{Change{key:RecordKey{table:1,id},value:Some(value.into())}}
fn main(){
 let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m6");
 std::fs::create_dir_all(&root).unwrap();
 let nonce=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
 let path=root.join(format!("example-{}-{nonce}",std::process::id()));
 let options=JournalOptions::default();let mut journal=Journal::create(&path,nonce.to_le_bytes(),options).unwrap();
 let first=journal.commit(journal.mark(),&[put(1,&100u32.to_le_bytes()),put(2,&200u32.to_le_bytes())]).unwrap();
 journal.commit(first.mark,&[put(1,&90u32.to_le_bytes()),put(2,&210u32.to_le_bytes())]).unwrap();
 journal.checkpoint().unwrap();
 journal.commit(journal.mark(),&[Change{key:RecordKey{table:1,id:1},value:None},put(3,b"")]).unwrap();
 let expected=journal.records().clone();let mark=journal.mark();journal.close().unwrap();
 let recovered=recover(&path,options).unwrap();assert_eq!(recovered.mark,mark);assert_eq!(recovered.records,expected);assert_eq!(recovered.replayed,1);
 let reopened=Journal::open(&path,options).unwrap();assert_eq!(reopened.mark(),mark);reopened.close().unwrap();
 println!("sequence={} records={} replayed={} live_bytes={} durability={:?} atomic_transfer=true empty_distinct_from_deleted=true recovered=true path={}",mark.sequence,recovered.records.len(),recovered.replayed,recovered.live_bytes,first.durability,path.display());
}
