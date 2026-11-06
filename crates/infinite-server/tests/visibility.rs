use infinite_core::{GridPosition as P,ViewRect,World,Replica,NetworkBudget};
use infinite_server::visibility::Visibility;use tokio::sync::mpsc;
fn view(x:i32)->ViewRect{ViewRect{min:P{x,y:0},max:P{x:x+1000,y:1000}}}
#[test]
fn visibility_boundaries_delayed_ack_and_resend_preserve_state(){
 let mut w=World::default();let a=w.spawn(P{x:200,y:200}).unwrap();let b=w.spawn(P{x:1200,y:200}).unwrap();let mut v=Visibility::new(90,2).unwrap();let(tx,mut rx)=mpsc::channel(2);let(id,stream)=v.attach(view(0),tx).unwrap();let mut replica=Replica::new(stream);
 let tick=|v:&mut Visibility,w:&World,now|{v.capture(w).unwrap();for _ in 0..4{v.poll(now,&mut NetworkBudget{work:2,bytes:4096}).unwrap();}};
 tick(&mut v,&w,0);let packet=rx.try_recv().unwrap();let ack=replica.apply(&packet).unwrap().acknowledgement();assert_eq!(replica.states().len(),1);assert_eq!(replica.states()[0].entity,a);
 w.set_position(a,P{x:1100,y:200}).unwrap();w.set_position(b,P{x:900,y:200}).unwrap();w.advance_tick().unwrap();tick(&mut v,&w,250);let retry=rx.try_recv().unwrap();assert_eq!(packet,retry);assert!(replica.apply(&retry).unwrap().duplicate);
 v.acknowledge(id,&ack).unwrap();tick(&mut v,&w,300);let next=rx.try_recv().unwrap();let report=replica.apply(&next).unwrap();assert_eq!(replica.states().len(),1);assert_eq!(replica.states()[0].entity,b);v.acknowledge(id,&report.acknowledgement()).unwrap();assert!(v.acknowledge(id,&[0;20]).is_err());
}
#[test]
fn full_socket_queue_does_not_block_other_connection_or_exceed_budget(){
 let mut w=World::default();let e=w.spawn(P{x:200,y:200}).unwrap();let mut v=Visibility::new(100,2).unwrap();let(tx,mut slow)=mpsc::channel(1);let(a,_)=v.attach(view(0),tx).unwrap();let(tx,mut fast)=mpsc::channel(1);let(b,stream)=v.attach(view(0),tx).unwrap();let mut replica=Replica::new(stream);let mut received=0;let mut blocked=0;
 for t in 0..40{w.set_position(e,P{x:200+t as i32,y:200}).unwrap();w.advance_tick().unwrap();v.capture(&w).unwrap();let mut budget=NetworkBudget{work:1,bytes:32};let r=v.poll(t*50,&mut budget).unwrap();blocked+=r.blocked;assert!(r.bytes<=32&&r.visited<=1);if let Ok(packet)=fast.try_recv(){let ack=replica.apply(&packet).unwrap().acknowledgement();v.acknowledge(b,&ack).unwrap();received+=1;}}
 assert!(received>=3&&blocked>0);assert!(slow.try_recv().is_ok());v.remove(a).unwrap();assert!(v.acknowledge(a,&[0;20]).is_err());v.remove(b).unwrap();
}
