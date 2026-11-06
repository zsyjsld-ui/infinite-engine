use infinite_core::{World,GridPosition as P,SpatialFrame,ViewRect,SyncSender,Replica};
use std::{num::NonZeroU32,io::{Read,Write},net::{TcpListener,TcpStream},sync::mpsc};
fn configure(socket:&TcpStream){
    let timeout=Some(std::time::Duration::from_secs(5));
    socket.set_nodelay(true).unwrap();socket.set_read_timeout(timeout).unwrap();socket.set_write_timeout(timeout).unwrap();
}
fn main(){
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();let address=listener.local_addr().unwrap();
    let (tx,rx)=mpsc::channel();
    let client=std::thread::spawn(move || {
        let mut socket=TcpStream::connect(address).unwrap();configure(&socket);
        let mut replica=Replica::new(7);let mut bytes=Vec::new();
        loop {
            let mut length=[0u8;4];match socket.read_exact(&mut length){Ok(())=>{},Err(e)if e.kind()==std::io::ErrorKind::UnexpectedEof=>break,Err(e)=>panic!("{e}")}
            let length=u32::from_le_bytes(length) as usize;assert!(length<=infinite_core::MAX_SYNC_PACKET_BYTES);bytes.resize(length,0);socket.read_exact(&mut bytes).unwrap();
            let report=replica.apply(&bytes).unwrap();socket.write_all(&report.acknowledgement()).unwrap();tx.send(replica.states().to_vec()).unwrap();
        }
    });
    let (mut socket,_)=listener.accept().unwrap();configure(&socket);
    let mut world=World::default();let actor=world.spawn(P{x:0,y:0}).unwrap();
    world.spawn(P{x:100000,y:100000}).unwrap();
    let view=ViewRect{min:P{x:-4096,y:-4096},max:P{x:4096,y:4096}};
    let mut frame=SpatialFrame::new(NonZeroU32::new(1024).unwrap());
    let mut sender=SyncSender::new(7);let mut received=Vec::new();
    for tick in 0..4{
        if tick==1 {world.set_position(actor,P{x:10,y:0}).unwrap();world.set_position(actor,P{x:20,y:0}).unwrap();}
        if tick==3 {world.set_position(actor,P{x:9000,y:0}).unwrap();}
        world.advance_tick().unwrap();frame.capture(&world).unwrap();
        if let Some(packet)=sender.prepare(&frame,view).unwrap(){
            socket.write_all(&(packet.len() as u32).to_le_bytes()).unwrap();socket.write_all(packet).unwrap();
            println!("tick={} payload={} framing=4 ack=20",world.tick().0,packet.len());
            let mut ack=[0u8;20];socket.read_exact(&mut ack).unwrap();sender.acknowledge_packet(&ack).unwrap();received=rx.recv().unwrap();
        }else{println!("tick={} unchanged: zero bytes",world.tick().0);}
        assert_eq!(received,frame.visible(view).unwrap());
    }
    drop(socket);client.join().unwrap();
}
