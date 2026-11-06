//! NM02: exact movement replica contract, explicit endian fields.
use infinite_core::{GridPosition,RouteCheckpoint,DirectionRemainder};
use infinite_runtime::{ActorView,Input,Phase};

pub const STATE_HEADER:usize=116;
pub const MAX_STATE:usize=STATE_HEADER+40+1024*8;
#[derive(Debug,PartialEq,Eq)]
pub enum ClientFrame { Hello([u8;32]), Input{epoch:u64,seq:u64,input:Input}, Ack{epoch:u64,tick:u64,intent:u64}, Ping{sent_us:u64} }
pub fn decode(bytes:&[u8])->Result<ClientFrame,&'static str> {
    if bytes.len()<8||&bytes[..4]!=b"NM02"||bytes[5..8]!=[0,0,0] {return Err("frame header");}
    let wide=|at|u64::from_le_bytes(bytes[at..at+8].try_into().unwrap());
    let signed=|at|i32::from_le_bytes(bytes[at..at+4].try_into().unwrap());
    match bytes[4] {
        1 if bytes.len()==40=>Ok(ClientFrame::Hello(bytes[8..40].try_into().unwrap())),
        2 if bytes.len()==40=>{
            if bytes[25..28]!=[0,0,0]||bytes[36..40]!=[0,0,0,0] {return Err("input reserved");}
            let(x,y)=(signed(28),signed(32));let input=match bytes[24] {
                0 if x==0&&y==0=>Input::Stop,
                1=>{let x=i16::try_from(x).map_err(|_|"direction")?;let y=i16::try_from(y).map_err(|_|"direction")?;
                    if i64::from(x)*i64::from(x)+i64::from(y)*i64::from(y)>32767_i64.pow(2) {return Err("direction norm");}Input::Direction{x,y}},
                2=>Input::Goal(GridPosition{x,y}),3 if x==0&&y==0=>Input::KeepAlive,_=>return Err("input kind"),
            };
            if wide(8)==0||wide(16)==0{return Err("input identity");}
            Ok(ClientFrame::Input{epoch:wide(8),seq:wide(16),input})
        },
        3 if bytes.len()==32=>Ok(ClientFrame::Ack{epoch:wide(8),tick:wide(16),intent:wide(24)}),
        4 if bytes.len()==16=>Ok(ClientFrame::Ping{sent_us:wide(8)}),
        _=>Err("frame kind/length"),
    }
}
fn base(kind:u8,len:usize)->Vec<u8>{let mut out=vec![0;len];out[..4].copy_from_slice(b"NM02");out[4]=kind;out}
fn u64at(b:&mut[u8],at:usize,v:u64){b[at..at+8].copy_from_slice(&v.to_le_bytes());}
fn u32at(b:&mut[u8],at:usize,v:u32){b[at..at+4].copy_from_slice(&v.to_le_bytes());}
pub fn welcome(epoch:u64,id:u64,instance:u64,map:&[u8;32])->Vec<u8>{
    let mut b=base(128,72);u64at(&mut b,8,epoch);u64at(&mut b,16,id);u64at(&mut b,24,instance);u32at(&mut b,32,50);u32at(&mut b,36,16);b[40..72].copy_from_slice(map);b
}
pub fn phase_code(phase:Phase)->u8{match phase{Phase::Idle=>0,Phase::Waiting=>1,Phase::Running=>2,Phase::Arrived=>3,Phase::Blocked=>4,Phase::Failed=>5,Phase::Cancelled=>6,Phase::Paused=>7}}
pub fn pong(sent_us:u64,server_us:u64)->Vec<u8>{let mut b=base(130,24);u64at(&mut b,8,sent_us);u64at(&mut b,16,server_us);b}
pub fn ping(sent_us:u64)->Vec<u8>{let mut b=base(4,16);u64at(&mut b,8,sent_us);b}
pub fn hello(token:[u8;32])->Vec<u8>{let mut b=base(1,40);b[8..].copy_from_slice(&token);b}
pub fn ack(epoch:u64,tick:u64,intent:u64)->Vec<u8>{let mut b=base(3,32);u64at(&mut b,8,epoch);u64at(&mut b,16,tick);u64at(&mut b,24,intent);b}
pub fn command(epoch:u64,seq:u64,input:Input)->Result<Vec<u8>,&'static str>{
    let mut b=base(2,40);u64at(&mut b,8,epoch);u64at(&mut b,16,seq);
    let (kind,x,y)=match input{Input::Stop=>(0,0,0),Input::Direction{x,y}=>(1,i32::from(x),i32::from(y)),Input::Goal(p)=>(2,p.x,p.y),Input::KeepAlive=>(3,0,0)};
    b[24]=kind;u32at(&mut b,28,x as u32);u32at(&mut b,32,y as u32);decode(&b)?;Ok(b)
}
pub fn state(v:ActorView,cursor:Option<&RouteCheckpoint>,sample_us:u64)->Vec<u8>{
    let len=cursor.map_or(0,|c|40+c.waypoints.len()*8);assert!(len<=40+1024*8);
    let mut b=base(129,STATE_HEADER+len);u64at(&mut b,8,v.epoch);u64at(&mut b,16,v.tick.0);
    u64at(&mut b,24,v.received_seq);u64at(&mut b,32,v.processed_seq);u64at(&mut b,40,v.intent_seq);
    u32at(&mut b,48,v.position.x as u32);u32at(&mut b,52,v.position.y as u32);
    b[56..58].copy_from_slice(&v.speed.to_le_bytes());b[58]=phase_code(v.phase);
    match v.input {Input::Stop=>{},Input::Direction{x,y}=>{b[59]=1;b[60..62].copy_from_slice(&x.to_le_bytes());b[62..64].copy_from_slice(&y.to_le_bytes());},Input::Goal(_)=>b[59]=2,Input::KeepAlive=>unreachable!()}
    u64at(&mut b,64,v.intent_seq);u32at(&mut b,72,len as u32);u64at(&mut b,76,sample_us);
    let(rx,ry)=v.direction_remainder.parts();u64at(&mut b,84,rx as u64);u64at(&mut b,92,ry as u64);
    if let Input::Goal(p)=v.input{u32at(&mut b,100,p.x as u32);u32at(&mut b,104,p.y as u32);}
    u32at(&mut b,108,v.arrival_radius);u32at(&mut b,112,v.lease_left);
    if let Some(c)=cursor {let o=STATE_HEADER;u32at(&mut b,o,1);u32at(&mut b,o+4,c.position.x as u32);u32at(&mut b,o+8,c.position.y as u32);
        u32at(&mut b,o+12,c.origin.x as u32);u32at(&mut b,o+16,c.origin.y as u32);u64at(&mut b,o+20,c.progress);
        u32at(&mut b,o+28,c.remainder);u32at(&mut b,o+32,c.radius);u32at(&mut b,o+36,c.waypoints.len()as u32);
        for(i,p)in c.waypoints.iter().enumerate(){u32at(&mut b,o+40+i*8,p.x as u32);u32at(&mut b,o+44+i*8,p.y as u32);}
    }b
}

#[derive(Clone,Debug)]
pub struct Authority {
    pub epoch:u64,pub tick:u64,pub received_seq:u64,pub processed_seq:u64,pub intent_seq:u64,
    pub position:GridPosition,pub speed:u16,pub phase:Phase,pub input:Input,
    pub route_version:u64,pub sample_us:u64,pub direction_remainder:DirectionRemainder,
    pub arrival_radius:u32,pub lease_left:u32,pub cursor:Option<RouteCheckpoint>,
}
#[derive(Debug)]
pub enum ServerFrame {
    Welcome{epoch:u64,actor:u64,instance:u64,map:[u8;32]},
    State(Authority),Pong{sent_us:u64,server_us:u64},
}
pub fn decode_server(b:&[u8])->Result<ServerFrame,&'static str>{
    if b.len()<8||&b[..4]!=b"NM02"||b[5..8]!=[0,0,0]{return Err("server header");}
    let q=|at|u64::from_le_bytes(b[at..at+8].try_into().unwrap());
    let d=|at|u32::from_le_bytes(b[at..at+4].try_into().unwrap());
    let p=|at|GridPosition{x:d(at)as i32,y:d(at+4)as i32};
    match b[4]{
        128 if b.len()==72=>{if q(8)==0||q(16)==0||q(24)==0||d(32)!=50||d(36)!=16{return Err("welcome contract");}
            Ok(ServerFrame::Welcome{epoch:q(8),actor:q(16),instance:q(24),map:b[40..72].try_into().unwrap()})},
        130 if b.len()==24=>Ok(ServerFrame::Pong{sent_us:q(8),server_us:q(16)}),
        129 if b.len()>=STATE_HEADER&&b.len()<=MAX_STATE=>{
            let len=d(72)as usize;if b.len()!=STATE_HEADER+len||q(8)==0||q(24)<q(32)||q(32)<q(40)||q(64)!=q(40){return Err("state identity/length");}
            let phase=match b[58]{0=>Phase::Idle,1=>Phase::Waiting,2=>Phase::Running,3=>Phase::Arrived,4=>Phase::Blocked,5=>Phase::Failed,6=>Phase::Cancelled,7=>Phase::Paused,_=>return Err("state phase")};
            let x=i16::from_le_bytes(b[60..62].try_into().unwrap());let y=i16::from_le_bytes(b[62..64].try_into().unwrap());
            let input=match b[59]{
                0 if x==0&&y==0&&p(100)==(GridPosition{x:0,y:0})=>Input::Stop,
                1 if p(100)==(GridPosition{x:0,y:0})&&i64::from(x)*i64::from(x)+i64::from(y)*i64::from(y)<=32767_i64.pow(2)=>Input::Direction{x,y},
                2 if x==0&&y==0=>Input::Goal(p(100)),_=>return Err("state input"),
            };
            let direction_remainder=DirectionRemainder::from_parts(q(84)as i64,q(92)as i64).map_err(|_|"state carry")?;
            let cursor=if len==0{None}else{
                let o=STATE_HEADER;if len<40||d(o)!=1{return Err("route header");}let count=d(o+36)as usize;
                if count==0||count>1024||len!=40+count*8||d(o+28)>=1_000_000||p(o+4)!=p(48)||d(o+32)!=d(108)||!matches!(input,Input::Goal(_)){return Err("route fields");}
                let waypoints=(0..count).map(|i|p(o+40+i*8)).collect::<Vec<_>>();if Some(waypoints.last().copied().unwrap())!=match input{Input::Goal(p)=>Some(p),_=>None}{return Err("route goal");}
                Some(RouteCheckpoint{position:p(o+4),origin:p(o+12),progress:q(o+20),remainder:d(o+28),radius:d(o+32),waypoints})
            };
            if phase==Phase::Running&&matches!(input,Input::Goal(_))&&cursor.is_none(){return Err("running route missing");}
            Ok(ServerFrame::State(Authority{epoch:q(8),tick:q(16),received_seq:q(24),processed_seq:q(32),intent_seq:q(40),position:p(48),speed:u16::from_le_bytes(b[56..58].try_into().unwrap()),phase,input,route_version:q(64),sample_us:q(76),direction_remainder,arrival_radius:d(108),lease_left:d(112),cursor}))
        },_=>Err("server kind/length"),
    }
}
