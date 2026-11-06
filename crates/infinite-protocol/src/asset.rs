//! Immutable application-supplied collision asset. Private game data is not
//! bundled into the service or public repository.
use infinite_core::{CollisionMap,GridPosition};
use sha2::{Digest,Sha256};
use std::num::NonZeroU32;

pub struct MapAsset { pub map:CollisionMap, pub hash:[u8;32] }
impl MapAsset {
    pub fn decode(bytes:&[u8])->Result<Self,&'static str> {
        if bytes.len()<32||&bytes[..4]!=b"IMAP"||bytes[4..8]!=[1,0,3,0] {return Err("map header");}
        let word=|at|u32::from_le_bytes(bytes[at..at+4].try_into().unwrap());
        let w=word(8);let h=word(12);let cells=u64::from(w)*u64::from(h);
        if cells==0||cells>1_048_576||bytes.len()!=32+(cells as usize+7)/8 {return Err("map length");}
        if cells%8!=0&&bytes[bytes.len()-1]>>(cells%8)!=0 {return Err("map padding");}
        let nz=|v|NonZeroU32::new(v).ok_or("map dimension");
        let mut map=CollisionMap::new_staggered_connected(GridPosition{x:word(24)as i32,y:word(28)as i32},nz(w)?,nz(h)?,nz(word(16))?,nz(word(20))?).map_err(|_|"map geometry")?;
        for i in 0..cells as usize {if bytes[32+i/8]&(1<<(i%8))!=0 {map.set_blocked(i as u32%w,i as u32/w,true).map_err(|_|"map cell")?;}}
        map.prepare_navigation();Ok(Self {map,hash:Sha256::digest(bytes).into()})
    }
}
