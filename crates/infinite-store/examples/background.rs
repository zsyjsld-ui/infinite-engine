use infinite_store::*;use std::{collections::BTreeMap,path::PathBuf,thread,time::{Duration,Instant}};
fn main(){
 let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m6");std::fs::create_dir_all(&root).unwrap();let nonce=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();let path=root.join(format!("background-{}-{nonce}",std::process::id()));let options=JournalOptions::default();let journal=Journal::create(&path,nonce.to_le_bytes(),options).unwrap();let mut service=JournalService::start(journal,ServiceOptions{outstanding:4,retained_bytes:4096}).unwrap();let mut drafts=BTreeMap::new();let(mut submitted,mut published,mut checkpoint,mut ticks,mut peak,mut backpressure)=(0u64,0usize,false,0usize,0usize,0usize);let until=Instant::now()+Duration::from_secs(10);
 while published<24||!checkpoint||service.outstanding()!=0{
  assert!(Instant::now()<until,"background work did not complete");ticks+=1;
  // One application admission and at most two receipts per simulated host turn.
  if submitted<24{let next=submitted+1;let changes=vec![Change{key:RecordKey{table:1,id:next},value:Some((next as u32*10).to_le_bytes().as_slice().into())}];match service.try_commit(None,changes){Ok(ticket)=>{drafts.insert(ticket.id(),next);submitted=next;},Err(error)=>{assert_eq!(error.reason,SubmitError::Full);backpressure+=1;}}}
  if published>=12&&!checkpoint{if service.try_checkpoint().is_ok(){checkpoint=true;}}
  peak=peak.max(service.outstanding());for _ in 0..2{let Some(receipt)=service.try_poll().unwrap()else{break;};match receipt.kind{CompletionKind::Commit(Ok(commit))=>{let id=drafts.remove(&receipt.ticket).unwrap();assert_eq!(commit.mark.sequence,id);published+=1;},CompletionKind::Checkpoint(Ok(mark))=>assert!(mark.sequence>=12),other=>panic!("unexpected receipt: {other:?}")}}
  thread::sleep(Duration::from_millis(1));
 }
 let final_mark=service.confirmed_mark();let shutdown=service.shutdown();shutdown.worker.unwrap();assert!(shutdown.completions.is_empty());let restored=recover(&path,options).unwrap();assert_eq!(restored.mark,final_mark);assert_eq!(restored.records.len(),24);for(key,value)in &restored.records{assert_eq!(u32::from_le_bytes(value.as_ref().try_into().unwrap()),key.id as u32*10);}
 println!("submitted={submitted} published_after_receipt={published} checkpoints=1 host_turns={ticks} peak_outstanding={peak} backpressure={backpressure} restored_records={} sequence={} verified=true path={}",restored.records.len(),restored.mark.sequence,path.display());
}
