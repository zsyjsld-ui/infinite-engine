//! Re-encode application-supplied authority traces with delayed ACKs. Private
//! map/trace inputs remain external; expanded bytes must match exactly.
use std::{env,fs,collections::VecDeque,time::Instant,sync::Arc};
use infinite_protocol::state_delta::{StateEncoder,StateHistory};
fn unhex(s:&str)->Vec<u8>{s.as_bytes().chunks_exact(2).map(|c|u8::from_str_radix(std::str::from_utf8(c).unwrap(),16).unwrap()).collect()}
fn hex(b:&[u8])->String{b.iter().map(|v|format!("{v:02x}")).collect()}
fn q(b:&[u8],at:usize)->u64{u64::from_le_bytes(b[at..at+8].try_into().unwrap())}
fn main(){let args:Vec<_>=env::args().collect();assert_eq!(args.len(),3,"input-traces output-traces");fs::create_dir(&args[2]).unwrap();let mut paths=fs::read_dir(&args[1]).unwrap().map(|e|e.unwrap().path()).collect::<Vec<_>>();paths.sort();let mut rows=Vec::new();
 for path in paths{let text=fs::read_to_string(&path).unwrap();let latency:u64=text.lines().next().unwrap().split_whitespace().find_map(|s|s.strip_prefix("latency=")).unwrap().parse().unwrap();let lines:Vec<_>=text.lines().collect();let mut output=String::new();let mut encoder=StateEncoder::default();let mut receiver=StateHistory::default();let mut acks=VecDeque::new();let(mut bytes_before,mut bytes_after,mut states,mut deltas,mut encode_ns,mut decode_ns,mut peak)=(0,0,0,0,0u128,0u128,0);
  for line in lines{let parts=line.split_whitespace().collect::<Vec<_>>();if parts.len()!=3||parts[1]!="F"{output.push_str(line);output.push('\n');continue;}
   let time:u64=parts[0].parse().unwrap();let full=unhex(parts[2]);if full.get(4)!=Some(&129){output.push_str(line);output.push('\n');continue;}
   let sent_at=q(&full,76);while acks.front().is_some_and(|&(at,_,_)|at<=sent_at){let(_,epoch,tick)=acks.pop_front().unwrap();encoder.acknowledge(epoch,tick).unwrap();}
   let begin=Instant::now();let packet=encoder.encode(&full).unwrap();encode_ns+=begin.elapsed().as_nanos();bytes_before+=full.len();bytes_after+=packet.len();states+=1;deltas+=usize::from(packet[4]==131);
   let begin=Instant::now();let expanded=receiver.expand(&packet).unwrap();decode_ns+=begin.elapsed().as_nanos();assert_eq!(expanded.as_ref(),full.as_slice());receiver.commit(expanded).unwrap();encoder.commit_sent(Arc::from(full.as_slice())).unwrap();peak=peak.max(encoder.retained_bytes()+receiver.retained_bytes());acks.push_back((time+latency/2,q(&full,8),q(&full,16)));
   output.push_str(&format!("{time} F {}\n",hex(&packet)));
  }
  let name=path.file_name().unwrap().to_str().unwrap();fs::write(format!("{}/{name}",args[2]),output).unwrap();rows.push(format!("{{\"name\":\"{name}\",\"rtt_us\":{latency},\"states\":{states},\"deltas\":{deltas},\"full_bytes\":{bytes_before},\"compressed_bytes\":{bytes_after},\"encode_ns\":{encode_ns},\"expand_ns\":{decode_ns},\"both_history_peak_bytes\":{peak}}}"));
 }
 println!("{{\"passed\":true,\"scope\":\"Exact full state byte equality with delayed ACK; timing is codec only, not geometry or socket IO; history bytes exclude allocation headers\",\"rows\":[{}]}}",rows.join(","));
}
