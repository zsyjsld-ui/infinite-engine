//! Synthetic loopback protocol check; reads only the explicitly supplied demo account.
use futures_util::{SinkExt,StreamExt};
use infinite_server::{service,wire};
use infinite_protocol::{state_delta::StateHistory,visibility};
use infinite_runtime::{Input,Phase};
use tokio_tungstenite::tungstenite::{Message,client::IntoClientRequest};
#[tokio::main(flavor="current_thread")]
async fn main()->Result<(),Box<dyn std::error::Error>>{
 let args:Vec<_>=std::env::args().collect();if args.len()!=3{return Err("usage: smoke ws://127.0.0.1:PORT/ws .local/demo-.../accounts.txt".into());}
 let accounts=service::accounts(&std::fs::read_to_string(&args[2])?)?;
 let mut request=args[1].as_str().into_client_request()?;
 if request.uri().host()!=Some("127.0.0.1"){return Err("synthetic smoke only accepts loopback".into());}
 let port=request.uri().port_u16().ok_or("missing port")?;request.headers_mut().insert("Origin","http://127.0.0.1:8080".parse()?);
 let stream=tokio::net::TcpStream::connect(("127.0.0.1",port)).await?;let(mut socket,_)=tokio_tungstenite::client_async(request,stream).await?;
 socket.send(Message::Binary(wire::hello(accounts[0].secret).into())).await?;
 let mut history=StateHistory::default();let mut visible:Option<infinite_core::Replica>=None;let mut start=None;let mut epoch=0;let mut stop_sent=false;let mut stopped=None;let mut stable=0;
 tokio::time::timeout(std::time::Duration::from_secs(10),async{
  while let Some(message)=socket.next().await{let Message::Binary(b)=message? else{continue;};
   if b.starts_with(b"IS"){let r=visible.as_mut().ok_or("missing visibility welcome")?;let result=r.apply(&b).map_err(|e|format!("{e:?}"))?;socket.send(Message::Binary(result.acknowledgement().to_vec().into())).await?;continue;}
   if b.get(4)==Some(&132){let(stream,_)=visibility::decode_welcome(&b)?;visible=Some(infinite_core::Replica::new(stream));continue;}
   if b.get(4)==Some(&128){if let wire::ServerFrame::Welcome{epoch:e,..}=wire::decode_server(&b)?{epoch=e;}continue;}
   if !matches!(b.get(4),Some(129|131)){continue;}
   let full=history.expand(&b)?;let wire::ServerFrame::State(s)=wire::decode_server(&full)? else{return Err("state kind".into());};history.commit(full)?;
   socket.send(Message::Binary(wire::ack(epoch,s.tick,s.intent_seq).into())).await?;
   if start.is_none(){start=Some(s.position);socket.send(Message::Binary(wire::command(epoch,1,Input::Direction{x:32767,y:0})?.into())).await?;}
   else if !stop_sent&&Some(s.position)!=start{socket.send(Message::Binary(wire::command(epoch,2,Input::Stop)?.into())).await?;stop_sent=true;}
   else if stop_sent&&s.intent_seq==2&&s.phase==Phase::Cancelled{if let Some(p)=stopped{if p!=s.position{return Err("drift after stop".into());}}else{stopped=Some(s.position);}stable+=1;if stable==3{socket.close(None).await?;println!("PASS: authenticated NM02, movement, ACK delta, visibility ACK and stable stop");return Ok::<(),Box<dyn std::error::Error>>(());}}
  }Err("connection closed before verification".into())
 }).await??;Ok(())
}
