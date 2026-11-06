//! Local reference host: authenticated synthetic actors, bounded socket work,
//! and one simulation owner. No application or network thread mutates World.
use std::{collections::{BTreeMap,VecDeque},path::PathBuf,sync::{Arc,atomic::{AtomicBool,AtomicU64,Ordering}},time::{Duration,Instant}};
use futures_util::{SinkExt,StreamExt};
use infinite_core::{GridPosition,RouteCheckpoint,ConnectionId,NetworkBudget,ViewRect,MAX_SYNC_PACKET_BYTES};
use infinite_runtime::{ActorView,Config,ControlToken,MovementRuntime};
use tokio::{net::{TcpListener,TcpStream},sync::{mpsc,oneshot,watch,Semaphore},time::timeout};
use tokio_tungstenite::{accept_hdr_async_with_config,tungstenite::{Message,protocol::WebSocketConfig,handshake::server::{Request,Response,ErrorResponse}}};
use crate::{asset::MapAsset,wire::{self,ClientFrame}};
use infinite_protocol::state_delta::StateEncoder;

const CONNECTIONS:usize=64;
const INPUT_QUEUE:usize=32;
const INPUTS_PER_TICK:usize=32;
const SEND_TIMEOUT:Duration=Duration::from_millis(500);
const ACK_TIMEOUT:Duration=Duration::from_secs(5);

#[derive(Clone)]
pub struct Account { pub id:u64, pub secret:[u8;32], pub position:GridPosition, pub speed:u16 }
pub struct Options {
    pub map:MapAsset, pub accounts:Vec<Account>, pub origin:String, pub port:u16,
    pub instance:u64, pub ready:PathBuf, pub stop:PathBuf, pub metrics:PathBuf,
}
pub fn accounts(text:&str)->Result<Vec<Account>,String> {
    let mut out=Vec::<Account>::new();
    for line in text.lines().filter(|s|!s.trim().is_empty()) {
        let p:Vec<_>=line.split_whitespace().collect();
        if p.len()!=5||p[1].len()!=64||!p[1].is_ascii(){return Err("account format".into());}
        let mut secret=[0;32];
        for (i,b) in secret.iter_mut().enumerate(){*b=u8::from_str_radix(&p[1][i*2..i*2+2],16).map_err(|_|"account token")?;}
        let a=Account {id:p[0].parse().map_err(|_|"account id")?,secret,
            position:GridPosition{x:p[2].parse().map_err(|_|"spawn x")?,y:p[3].parse().map_err(|_|"spawn y")?},speed:p[4].parse().map_err(|_|"speed")?};
        if a.id==0||a.secret==[0;32]||out.iter().any(|v|v.id==a.id||v.secret==a.secret){return Err("duplicate/zero account".into());}
        out.push(a);
        if out.len()>256{return Err("account capacity".into());}
    }
    if out.is_empty(){return Err("no accounts".into());}Ok(out)
}
struct Snapshot { view:ActorView, cursor:Option<RouteCheckpoint>, sample_us:u64 }
enum Inbound{Control(ClientFrame),VisibilityAck([u8;20])}
struct Join { account:Account, token:ControlToken, inputs:mpsc::Receiver<Inbound>, states:watch::Sender<Option<Arc<Snapshot>>>, visible:mpsc::Sender<Vec<u8>>, reply:oneshot::Sender<Result<[u8;24],String>> }
struct Peer { token:ControlToken, inputs:mpsc::Receiver<Inbound>, states:watch::Sender<Option<Arc<Snapshot>>>, visibility:ConnectionId }
// Trusted reference-host camera envelope, independent of client-reported position.
fn view_at(p:GridPosition)->ViewRect{ViewRect{min:GridPosition{x:p.x.saturating_sub(16384),y:p.y.saturating_sub(12288)},max:GridPosition{x:p.x.saturating_add(16384),y:p.y.saturating_add(12288)}}}
#[derive(Default)]
struct Counters { accepted:AtomicU64, rejected:AtomicU64, closed:AtomicU64, received_bytes:AtomicU64, sent_bytes:AtomicU64, sent_states:AtomicU64, acked_states:AtomicU64, delta_states:AtomicU64, state_encode_ns:AtomicU64, state_history_peak:AtomicU64 }
#[derive(Default)]
struct Metrics { ticks:u64, commands:u64, work:u64, moved:u64, rejected_inputs:u64, joins:u64, expired:u64, max_peers:usize, max_input_depth:usize, late:u64, reset_clock:u64, total_ns:u128, max_ns:u128, histogram:Vec<u64> }
impl Metrics {
    fn record(&mut self,elapsed:Duration){let ns=elapsed.as_nanos();self.total_ns+=ns;self.max_ns=self.max_ns.max(ns);self.histogram[(ns/50_000).min(4000)as usize]+=1;}
    fn percentile_us(&self,p:u64)->u64 {let target=(self.ticks*p+99)/100;let mut n=0;for(i,&v)in self.histogram.iter().enumerate(){n+=v;if n>=target{return(i as u64+1)*50;}}0}
    fn json(&self,n:&Counters)->String {
        let read=|a:&AtomicU64|a.load(Ordering::Relaxed);
        let json=format!("{{\"ticks\":{},\"commands\":{},\"navigation_work\":{},\"moved\":{},\"rejected_inputs\":{},\"joins\":{},\"expired\":{},\"max_peers\":{},\"max_input_depth\":{},\"late_over_50ms\":{},\"clock_resets\":{},\"step_mean_us\":{},\"step_p50_us_upper\":{},\"step_p95_us_upper\":{},\"step_p99_us_upper\":{},\"step_max_us\":{},\"accepted\":{},\"rejected\":{},\"closed\":{},\"received_payload_bytes\":{},\"sent_payload_bytes\":{},\"sent_states\":{},\"acked_states\":{}}}\n",
            self.ticks,self.commands,self.work,self.moved,self.rejected_inputs,self.joins,self.expired,self.max_peers,self.max_input_depth,self.late,self.reset_clock,self.total_ns as f64/self.ticks.max(1)as f64/1000.0,self.percentile_us(50),self.percentile_us(95),self.percentile_us(99),self.max_ns as f64/1000.0,read(&n.accepted),read(&n.rejected),read(&n.closed),read(&n.received_bytes),read(&n.sent_bytes),read(&n.sent_states),read(&n.acked_states));
        format!("{},\"delta_states\":{},\"state_encode_ns\":{},\"state_history_peak_bytes_per_connection\":{}}}\n",json.trim_end_matches("}\n"),read(&n.delta_states),read(&n.state_encode_ns),read(&n.state_history_peak))
    }
}
fn simulate(mut runtime:MovementRuntime,mut joins:mpsc::Receiver<Join>,stop:Arc<AtomicBool>,start:Instant,first_stream:u64)->Result<Metrics,String> {
    let mut next=start+Duration::from_millis(50);
    let mut peers=BTreeMap::<u64,Peer>::new();let mut m=Metrics{histogram:vec![0;4001],..Metrics::default()};
    let mut visibility=crate::visibility::Visibility::new(first_stream,CONNECTIONS).map_err(|e|format!("visibility: {e:?}"))?;
    let mut dormant=BTreeMap::<u64,GridPosition>::new();let mut owner_cursor=0;
    while !stop.load(Ordering::Acquire) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        let now=Instant::now();if now.duration_since(next)>Duration::from_millis(50){m.late+=1;}
        if now.duration_since(next)>Duration::from_millis(200){next=now;m.reset_clock+=1;}
        next+=Duration::from_millis(50);let begin=Instant::now();
        // Bound lifecycle admission too; a reconnect storm is not unlimited tick work.
        for _ in 0..16 {
            let Ok(j)=joins.try_recv() else{break;};if j.reply.is_closed(){continue;}
            if let Some(old)=peers.remove(&j.token.id){visibility.remove(old.visibility).map_err(|e|format!("visibility replacement: {e:?}"))?;}
            let result=if runtime.view(j.token.id).is_ok(){runtime.renew(j.token)}else{runtime.attach(j.token,dormant.remove(&j.token.id).unwrap_or(j.account.position),j.account.speed).map(|_|())};
            match result {
                Ok(())=>{let v=runtime.view(j.token.id).map_err(|e|format!("joined view: {e:?}"))?;let(id,stream)=visibility.attach(view_at(v.position),j.visible).map_err(|e|format!("visibility admission: {e:?}"))?;peers.insert(j.token.id,Peer{token:j.token,inputs:j.inputs,states:j.states,visibility:id});m.joins+=1;let _=j.reply.send(Ok(infinite_protocol::visibility::welcome(stream,v.entity)));},
                Err(e)=>{let _=j.reply.send(Err(format!("admission: {e:?}")));},
            }
        }
        let mut remove=Vec::new();
        for (&id,p) in &mut peers {
            m.max_input_depth=m.max_input_depth.max(p.inputs.len());
            let mut invalid=false;
            for _ in 0..INPUTS_PER_TICK {match p.inputs.try_recv(){
                Ok(Inbound::Control(ClientFrame::Input{epoch,seq,input}))=>{if runtime.submit(ControlToken{id,epoch},seq,input).is_err(){m.rejected_inputs+=1;invalid=true;break;}},
                Ok(Inbound::VisibilityAck(ack))=>{if visibility.acknowledge(p.visibility,&ack).is_err(){invalid=true;break;}},
                Ok(_)=>{invalid=true;break;},
                Err(mpsc::error::TryRecvError::Empty)=>break,
                Err(mpsc::error::TryRecvError::Disconnected)=>{invalid=true;break;},
            }}
            if invalid||p.states.is_closed(){runtime.revoke(p.token).map_err(|e|format!("revoke: {e:?}"))?;remove.push(id);}
        }
        for id in remove {if let Some(p)=peers.remove(&id){visibility.remove(p.visibility).map_err(|e|format!("visibility remove: {e:?}"))?;dormant.insert(id,runtime.view(id).map_err(|e|format!("retire view: {e:?}"))?.position);runtime.detach(id).map_err(|e|format!("retire: {e:?}"))?;}}
        let r=runtime.step().map_err(|e|format!("simulation halted: {e:?}"))?;
        m.ticks+=1;m.commands+=r.commands as u64;m.work+=u64::from(r.navigation_work);m.moved+=r.moved as u64;m.expired+=r.expired as u64;m.max_peers=m.max_peers.max(peers.len());
        let sample_us=start.elapsed().as_micros()as u64;
        // One admission budget for self authority and nearby state. The latest
        // self slot is replaceable; IS frames use the existing fair scheduler.
        let mut budget=NetworkBudget{work:CONNECTIONS*4,bytes:256*1024};let ids:Vec<_>=peers.keys().copied().collect();let first=owner_cursor;
        for offset in 0..ids.len() {let index=(first+offset)%ids.len();let id=ids[index];let p=&peers[&id];
            let view=runtime.view(id).map_err(|e|format!("view: {e:?}"))?;
            let cursor=runtime.checkpoint(id).map_err(|e|format!("cursor: {e:?}"))?;
            let cost=wire::STATE_HEADER+cursor.as_ref().map_or(0,|c|40+c.waypoints.len()*8);if cost>budget.bytes{break;}budget.bytes-=cost;owner_cursor=(index+1)%ids.len();
            p.states.send_replace(Some(Arc::new(Snapshot{view,cursor,sample_us})));
        }
        visibility.capture(runtime.world()).map_err(|e|format!("spatial capture: {e:?}"))?;
        for (&id,p) in &peers{visibility.view(p.visibility,view_at(runtime.view(id).map_err(|e|format!("camera: {e:?}"))?.position)).map_err(|e|format!("camera: {e:?}"))?;}
        let mut disconnected=Vec::new();for _ in 0..4{if budget.work==0||budget.bytes==0{break;}visibility.poll(sample_us/1000,&mut budget).map_err(|e|format!("visibility send: {e:?}"))?;disconnected.extend(visibility.notices().iter().map(|n|n.connection));}
        for connection in disconnected{if let Some(id)=peers.iter().find_map(|(&id,p)|(p.visibility==connection).then_some(id)){let p=peers.remove(&id).unwrap();visibility.remove(p.visibility).map_err(|e|format!("visibility close: {e:?}"))?;dormant.insert(id,runtime.view(id).map_err(|e|format!("timeout view: {e:?}"))?.position);runtime.detach(id).map_err(|e|format!("timeout retire: {e:?}"))?;}}
        m.record(begin.elapsed());
    }
    Ok(m)
}

struct Network { accounts:Vec<Account>, origin:String, instance:u64, map:[u8;32], epoch:AtomicU64, joins:mpsc::Sender<Join>, counters:Arc<Counters>,start:Instant }
async fn connection(stream:TcpStream,n:Arc<Network>)->Result<(),&'static str> {
    stream.set_nodelay(true).map_err(|_|"tcp options")?;
    let origin=n.origin.clone();
    let verify=move |req:&Request,response:Response|->Result<Response,ErrorResponse>{
        if req.uri().path()!="/ws"||req.uri().query().is_some()||req.headers().get("origin").and_then(|h|h.to_str().ok())!=Some(origin.as_str()) {
            return Err(tokio_tungstenite::tungstenite::http::Response::builder().status(403).body(Some("origin/path".into())).unwrap());
        }Ok(response)
    };
    let config=WebSocketConfig::default().read_buffer_size(4096).write_buffer_size(0).max_write_buffer_size(MAX_SYNC_PACKET_BYTES*2).max_message_size(Some(256)).max_frame_size(Some(256));
    let mut ws=timeout(Duration::from_secs(3),accept_hdr_async_with_config(stream,verify,Some(config))).await.map_err(|_|"handshake timeout")?.map_err(|_|"handshake")?;
    let message=timeout(Duration::from_secs(3),ws.next()).await.map_err(|_|"hello timeout")?.ok_or("hello closed")?.map_err(|_|"hello socket")?;
    let Message::Binary(bytes)=message else{return Err("hello binary");};
    n.counters.received_bytes.fetch_add(bytes.len()as u64,Ordering::Relaxed);
    let ClientFrame::Hello(secret)=wire::decode(&bytes)? else{return Err("hello kind");};
    let account=n.accounts.iter().find(|a|a.secret.iter().zip(secret).fold(0u8,|v,(a,b)|v|(a^b))==0).cloned().ok_or("authentication")?;
    let epoch=n.epoch.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|v|v.checked_add(1)).map_err(|_|"epoch exhausted")?;
    let token=ControlToken{id:account.id,epoch};
    let(tx,inputs)=mpsc::channel(INPUT_QUEUE);let(states,mut rx)=watch::channel(None);let(reply,result)=oneshot::channel();
    let(visible,mut visible_rx)=mpsc::channel(2);
    n.joins.try_send(Join{account,token,inputs,states,visible,reply}).map_err(|_|"join capacity")?;
    let visibility_welcome=timeout(Duration::from_secs(2),result).await.map_err(|_|"join timeout")?.map_err(|_|"simulation unavailable")?.map_err(|_|"admission")?;
    let welcome=wire::welcome(epoch,token.id,n.instance,&n.map);let len=welcome.len();
    timeout(SEND_TIMEOUT,ws.send(Message::Binary(welcome.into()))).await.map_err(|_|"welcome timeout")?.map_err(|_|"welcome socket")?;
    n.counters.sent_bytes.fetch_add(len as u64,Ordering::Relaxed);
    let visibility_stream=infinite_protocol::visibility::decode_welcome(&visibility_welcome)?.0;
    timeout(SEND_TIMEOUT,ws.send(Message::Binary(visibility_welcome.to_vec().into()))).await.map_err(|_|"visibility welcome timeout")?.map_err(|_|"visibility welcome socket")?;
    n.counters.sent_bytes.fetch_add(24,Ordering::Relaxed);let(mut last_visible_sent,mut last_visible_ack)=(0u64,0u64);
    n.counters.accepted.fetch_add(1,Ordering::Relaxed);
    let mut rate_start=Instant::now();let mut rate=0u32;let mut ack_at=Instant::now();let mut last_ack=(0,0);
    let mut sent=VecDeque::<(u64,u64)>::with_capacity(128);
    let mut encoder=StateEncoder::default();
    let mut guard=tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            _=guard.tick()=>{if ack_at.elapsed()>ACK_TIMEOUT{return Err("ack timeout");}},
            packet=visible_rx.recv()=>{let packet=packet.ok_or("visibility ended")?;let seq=u64::from_le_bytes(packet[12..20].try_into().unwrap());let len=packet.len();timeout(SEND_TIMEOUT,ws.send(Message::Binary(packet.into()))).await.map_err(|_|"visibility send timeout")?.map_err(|_|"visibility send socket")?;last_visible_sent=seq;n.counters.sent_bytes.fetch_add(len as u64,Ordering::Relaxed);},
            changed=rx.changed()=>{
                changed.map_err(|_|"ownership ended")?;
                let snapshot=rx.borrow_and_update().clone();let Some(s)=snapshot else{continue;};
                let encode_start=Instant::now();let full:Arc<[u8]>=wire::state(s.view,s.cursor.as_ref(),s.sample_us).into();let bytes=encoder.encode(&full)?;let len=bytes.len();let delta=bytes[4]==131;
                n.counters.state_encode_ns.fetch_add(encode_start.elapsed().as_nanos().min(u128::from(u64::MAX))as u64,Ordering::Relaxed);
                timeout(SEND_TIMEOUT,ws.send(Message::Binary(bytes.into()))).await.map_err(|_|"send timeout")?.map_err(|_|"send socket")?;
                encoder.commit_sent(full)?;n.counters.state_history_peak.fetch_max(encoder.retained_bytes()as u64,Ordering::Relaxed);if delta{n.counters.delta_states.fetch_add(1,Ordering::Relaxed);}
                n.counters.sent_bytes.fetch_add(len as u64,Ordering::Relaxed);n.counters.sent_states.fetch_add(1,Ordering::Relaxed);
                if sent.len()==128{sent.pop_front();}sent.push_back((s.view.tick.0,s.view.intent_seq));
            },
            message=ws.next()=>{
                let message=message.ok_or("closed")?.map_err(|_|"socket")?;
                if rate_start.elapsed()>=Duration::from_secs(1){rate_start=Instant::now();rate=0;}
                rate+=1;if rate>96{return Err("rate limit");}
                match message {
                    Message::Binary(bytes)=>{
                        n.counters.received_bytes.fetch_add(bytes.len()as u64,Ordering::Relaxed);
                        if bytes.starts_with(b"IA"){if bytes.len()!=20||&bytes[..4]!=b"IA\x01\x00"||u64::from_le_bytes(bytes[4..12].try_into().unwrap())!=visibility_stream{return Err("visibility ACK identity");}let seq=u64::from_le_bytes(bytes[12..20].try_into().unwrap());if seq==last_visible_ack&&seq!=0{continue;}if seq==0||seq!=last_visible_sent{return Err("visibility ACK not sent");}tx.try_send(Inbound::VisibilityAck(bytes.as_ref().try_into().unwrap())).map_err(|_|"visibility ACK capacity")?;last_visible_ack=seq;continue;}
                        match wire::decode(&bytes)? {
                            ClientFrame::Ping{sent_us}=>{
                                let pong=wire::pong(sent_us,n.start.elapsed().as_micros()as u64);let len=pong.len();
                                timeout(SEND_TIMEOUT,ws.send(Message::Binary(pong.into()))).await.map_err(|_|"clock timeout")?.map_err(|_|"clock socket")?;
                                n.counters.sent_bytes.fetch_add(len as u64,Ordering::Relaxed);
                            },
                            frame@ClientFrame::Input{epoch:e,..} if e==epoch=>tx.try_send(Inbound::Control(frame)).map_err(|_|"input capacity/ownership")?,
                            ClientFrame::Ack{epoch:e,tick,intent} if e==epoch=>{
                                if (tick,intent)==last_ack{continue;}
                                if tick<=last_ack.0||!sent.contains(&(tick,intent)){return Err("ack not sent");}
                                encoder.acknowledge(epoch,tick)?;
                                last_ack=(tick,intent);ack_at=Instant::now();n.counters.acked_states.fetch_add(1,Ordering::Relaxed);
                                while sent.front().is_some_and(|v|v.0<=tick){sent.pop_front();}
                            },
                            _=>return Err("frame identity/kind"),
                        }
                    },
                    Message::Close(_)=>return Ok(()),
                    Message::Ping(_)=>{timeout(SEND_TIMEOUT,ws.flush()).await.map_err(|_|"pong timeout")?.map_err(|_|"pong socket")?;},
                    Message::Pong(_)=>{},
                    _=>return Err("binary required"),
                }
            }
        }
    }
}

pub async fn run(options:Options)->Result<(),String> {
    if options.instance==0||options.instance>u64::MAX-1_000_000||options.accounts.is_empty()||options.accounts.len()>256||!options.origin.starts_with("http://127.0.0.1:"){return Err("local host configuration".into());}
    let runtime=MovementRuntime::new(options.map.map,Config{max_controllers:256,commands_per_tick:256,navigation_work:65536,..Config::default()}).map_err(|e|format!("runtime: {e:?}"))?;
    let listener=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,options.port)).await.map_err(|e|format!("bind: {e}"))?;
    let port=listener.local_addr().map_err(|e|e.to_string())?.port();
    let(tx,rx)=mpsc::channel(64);let stop=Arc::new(AtomicBool::new(false));let counters=Arc::new(Counters::default());
    let start=Instant::now();
    let n=Arc::new(Network{accounts:options.accounts,origin:options.origin,instance:options.instance,map:options.map.hash,epoch:AtomicU64::new(options.instance),joins:tx,counters:counters.clone(),start});
    // Write readiness before launching threads; failed IO cannot orphan a simulation.
    tokio::fs::write(&options.ready,format!("{{\"port\":{port},\"pid\":{},\"instance\":\"{}\"}}",std::process::id(),options.instance)).await.map_err(|e|format!("ready: {e}"))?;
    let flag=stop.clone();let simulation=std::thread::Builder::new().name("infinite-simulation".into()).spawn(move||{
        let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||simulate(runtime,rx,flag.clone(),start,options.instance))).unwrap_or_else(|_|Err("simulation panic".into()));flag.store(true,Ordering::Release);result
    }).map_err(|e|e.to_string())?;
    let slots=Arc::new(Semaphore::new(CONNECTIONS));let mut tasks=tokio::task::JoinSet::new();let mut poll=tokio::time::interval(Duration::from_millis(250));
    let mut failure=None;
    loop {tokio::select!{
        _=poll.tick()=>{if stop.load(Ordering::Acquire)||tokio::fs::try_exists(&options.stop).await.unwrap_or(false){break;}},
        result=listener.accept()=>{
            match result {Ok((stream,_))=>{if let Ok(permit)=slots.clone().try_acquire_owned(){let net=n.clone();tasks.spawn(async move{let _permit=permit;if connection(stream,net.clone()).await.is_err(){net.counters.rejected.fetch_add(1,Ordering::Relaxed);}net.counters.closed.fetch_add(1,Ordering::Relaxed);});}else{counters.rejected.fetch_add(1,Ordering::Relaxed);}},Err(e)=>{failure=Some(format!("accept: {e}"));break;} }
        },
        _=tasks.join_next(),if !tasks.is_empty()=>{},
    }}
    stop.store(true,Ordering::Release);tasks.abort_all();while tasks.join_next().await.is_some(){}
    let metrics=simulation.join().map_err(|_|"simulation join")??;
    tokio::fs::write(options.metrics,metrics.json(&counters)).await.map_err(|e|format!("metrics: {e}"))?;
    if let Some(e)=failure{return Err(e);}Ok(())
}
