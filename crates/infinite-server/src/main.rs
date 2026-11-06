use std::{collections::BTreeMap,path::PathBuf};
use infinite_server::{asset::MapAsset,service::{self,Options}};
#[tokio::main(flavor="current_thread")]
async fn main(){if let Err(error)=start().await{eprintln!("infinite-server: {error}");std::process::exit(1);}}
async fn start()->Result<(),String>{
    let mut args=std::env::args().skip(1);let mut flags=BTreeMap::new();
    while let Some(k)=args.next(){let v=args.next().ok_or("missing flag value")?;if !["--map","--accounts","--origin","--port","--instance","--ready","--stop","--metrics"].contains(&k.as_str())||flags.insert(k,v).is_some(){return Err("unknown/duplicate flag".into());}}
    let get=|k:&str|flags.get(k).cloned().ok_or_else(||format!("missing {k}"));
    let map_path=get("--map")?;if std::fs::metadata(&map_path).map_err(|e|e.to_string())?.len()>131104{return Err("map size".into());}
    let bytes=std::fs::read(map_path).map_err(|e|e.to_string())?;
    let account_path=get("--accounts")?;if std::fs::metadata(&account_path).map_err(|e|e.to_string())?.len()>32768{return Err("accounts size".into());}
    let accounts=service::accounts(&std::fs::read_to_string(account_path).map_err(|e|e.to_string())?)?;
    let options=Options{map:MapAsset::decode(&bytes).map_err(str::to_string)?,accounts,origin:get("--origin")?,port:get("--port")?.parse().map_err(|_|"port")?,instance:get("--instance")?.parse().map_err(|_|"instance")?,ready:PathBuf::from(get("--ready")?),stop:PathBuf::from(get("--stop")?),metrics:PathBuf::from(get("--metrics")?)};
    service::run(options).await
}
