//! Deterministic local server integration. Synthetic storage only; the transport
//! models ordered bytes and faulty application ACKs, not actual WAN packet loss.
mod simulation;
mod transport;
mod oracle;
mod metrics;
mod dormant;
use infinite_core::*;
use infinite_store::*;
use simulation::Simulation;
use transport::Transport;
use std::{collections::BTreeMap,path::PathBuf,sync::Arc,time::{Instant,Duration}};
fn main(){
    let args:Vec<String>=std::env::args().collect();
    let count:usize=args.get(1).map_or(256,|v|v.parse().unwrap());
    let peers:usize=args.get(2).map_or(8,|v|v.parse().unwrap());
    let ticks:u64=args.get(3).map_or(80,|v|v.parse().unwrap());
    let tcp=args.iter().any(|v|v=="--tcp");
    let wall_clock=args.iter().any(|v|v=="--wall-clock");
    let limited=args.iter().any(|v|v=="--budget-limited");
    let regions=args.iter().any(|v|v=="--regions");let jitter=args.iter().any(|v|v=="--jitter");
    let maps=4usize;assert!(count>=64&&count<=3000&&peers>0&&peers<=64&&(40..=12000).contains(&ticks));
    let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/server-core/m7/data");std::fs::create_dir_all(&root).unwrap();
    let nonce=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let path=root.join(format!("server-{}-{nonce}",std::process::id()));let options=JournalOptions::default();
    let journal=Journal::create(&path,nonce.to_le_bytes(),options).unwrap();
    let mut store=JournalService::start(journal,ServiceOptions{outstanding:16,retained_bytes:1024*1024}).unwrap();
    let mut simulations:Vec<_>=(0..maps).map(|_|Simulation::new(count)).collect();
    if regions{simulations[3].enable_region_probe();}
    let mut dormant=regions.then(dormant::DormantMap::new);
    let mut transports:Vec<_>=simulations.iter().enumerate().map(|(i,sim)|Transport::new(sim,peers,1+i as u64*1000,[20,80,200,80][i],if i%2==0{100}else{20},tcp)).collect();
    for net in &mut transports{net.jitter=jitter;}
    let lanes=[
        ServerLaneBudget{unit_cost:1000,units_per_tick:32,quantum:4,max_wait_ticks:2},
        ServerLaneBudget{unit_cost:1,units_per_tick:1000,quantum:100,max_wait_ticks:2},
        ServerLaneBudget{unit_cost:10,units_per_tick:16,quantum:2,max_wait_ticks:5},
        ServerLaneBudget{unit_cost:1,units_per_tick:(maps*peers*60)as u32,quantum:(peers*6)as u32,max_wait_ticks:1},
        ServerLaneBudget{unit_cost:10,units_per_tick:4,quantum:1,max_wait_ticks:10}];
    let mut budget=ServerWorkBudget::new(ServerBudgetConfig{maps,weighted_units_per_tick:if limited{2000}else{100000},lanes}).unwrap();
    let request=|budget:&mut ServerWorkBudget,map,lane,units| match budget.request(map,lane,units){
        Ok(_)=>(),Err(ServerBudgetError::AlreadyPending) if limited=>(),other=>panic!("work request: {other:?}"),
    };
    let(mut deferred_ticks,mut overdue_ticks,mut overload_ticks,mut oldest_wait,mut peak_public_cache)=(0usize,0usize,0usize,0u64,0usize);
    let mut navigation_granted=[0u64;4];
    let mut navigation=[0u32;4];let mut network=[0usize;4];let mut network_bytes=[0usize;4];
    let mut dirty=BTreeMap::<RecordKey,Arc<[u8]>>::new();let mut pending=BTreeMap::<u64,Vec<Change>>::new();let mut durable=Records::new();
    let(mut confirmed,mut pressure,mut checkpoints,mut peak_store)=(0usize,0usize,0usize,0usize);
    let mut checkpoint_pending=false;let end=(ticks-1)*50;let started=Instant::now();let mut finished=None;
    let mut simulation_ms=0.;let mut transport_ms=0.;let mut persistence_ms=0.;
    let mut samples=vec![[0f64;7];ticks as usize];let mut submitted=BTreeMap::new();let mut commit_latency=Vec::new();
    let(mut missed_deadlines,mut max_lateness_ms)=(0usize,0f64);
    for now in (0..=end+30000).step_by(5){
        if wall_clock{
            let due=started+Duration::from_millis(now);
            if let Some(wait)=due.checked_duration_since(Instant::now()){std::thread::sleep(wait);}
            if now<=end&&now%50==0{
                let late=started.elapsed().as_secs_f64()*1000.-now as f64;
                max_lateness_ms=max_lateness_ms.max(late);missed_deadlines+=usize::from(late>50.);
            }
        }
        let iteration=Instant::now();
        let before=[simulation_ms,persistence_ms,transports.iter().map(|n|n.traffic.sender_ms).sum(),
            transports.iter().map(|n|n.traffic.ack_ms).sum(),transports.iter().map(|n|n.traffic.client_model_ms).sum(),
            transports.iter().map(|n|n.traffic.oracle_capture_ms).sum()];
        let timeout=if wall_clock{Duration::from_millis(ticks*50+120000)}else{Duration::from_secs(120)};
        assert!(started.elapsed()<timeout,"integration wall timeout");
        if now%50==0{
            let tick=now/50;budget.begin_tick(Tick(tick)).unwrap();navigation.fill(0);network.fill(0);network_bytes.fill(peers*65536);
            let mut spawn=[0usize;4];let mut save=false;
            if now<=end{
                budget.record_authority((maps*(count/16))as u64).unwrap();
                for index in 0..maps{
                    request(&mut budget,index,ServerWorkLane::Spawn,2);
                    request(&mut budget,index,ServerWorkLane::Navigation,4);
                }
            }
            for index in 0..maps{request(&mut budget,index,ServerWorkLane::Network,(peers*60)as u32);}
            request(&mut budget,0,ServerWorkLane::Persistence,1);
            while let Some(grant)=budget.next_grant(){match grant.lane{
                ServerWorkLane::Navigation=>{navigation[grant.map]+=grant.units*MIN_NAVIGATION_WORK_SLICE;navigation_granted[grant.map]+=u64::from(grant.units);},
                ServerWorkLane::Spawn=>spawn[grant.map]+=grant.units as usize,
                ServerWorkLane::Network=>network[grant.map]+=grant.units as usize,
                ServerWorkLane::Persistence=>save=true,
                ServerWorkLane::Perception=>unreachable!(),
            }}
            if now<=end{
                let start=Instant::now();
                if let Some(idle)=&mut dormant{
                    budget.record_authority(u64::from(idle.update(Tick(tick)))).unwrap();
                    if tick%5==0||tick+1==ticks{dirty.insert(RecordKey{table:5,id:1},idle.record().into());}
                }
                for (index,sim) in simulations.iter_mut().enumerate(){
                    assert_eq!(sim.world.tick(),Tick(tick));
                    // Both synchronous chase and queued search share this cap.
                    sim.world.restrict_navigation_work(navigation[index]);
                    sim.spawn_due(spawn[index]);sim.authority();
                    budget.record_authority(sim.events.len()as u64).unwrap();
                    transports[index].observe(sim);
                    if tick%5==0||tick+1==ticks{
                        for (row,&id) in sim.ids.iter().take(8).enumerate(){let p=sim.world.position(id).unwrap();
                            let hp=sim.world.health(id).unwrap().current;let mut bytes=Vec::with_capacity(20);
                            bytes.extend_from_slice(&tick.to_le_bytes());bytes.extend_from_slice(&p.x.to_le_bytes());bytes.extend_from_slice(&p.y.to_le_bytes());bytes.extend_from_slice(&hp.to_le_bytes());
                            dirty.insert(RecordKey{table:index as u16+1,id:row as u64+1},bytes.into());
                        }
                    }
                }
                simulation_ms+=start.elapsed().as_secs_f64()*1000.;
            }
            if save&&!dirty.is_empty(){let start=Instant::now();let changes:Vec<_>=dirty.iter().map(|(&key,value)|Change{key,value:Some(value.clone())}).collect();
                        match store.try_commit(None,changes.clone()){
                            Ok(ticket)=>{submitted.insert(ticket.id(),Instant::now());pending.insert(ticket.id(),changes);dirty.clear();},
                            Err(error)=>{assert_eq!(error.reason,SubmitError::Full);pressure+=1;}
                        }persistence_ms+=start.elapsed().as_secs_f64()*1000.;
            }
            let report=budget.report().unwrap();
            deferred_ticks+=usize::from(report.pending>0);overdue_ticks+=usize::from(report.overdue>0);
            overload_ticks+=usize::from(report.over_budget);oldest_wait=oldest_wait.max(report.oldest_wait_ticks);
            if !limited{assert_eq!(report.pending,0);assert!(!report.over_budget);}
            if now<end{let start=Instant::now();for (index,sim) in simulations.iter_mut().enumerate(){sim.finish_tick(navigation[index]);}simulation_ms+=start.elapsed().as_secs_f64()*1000.;}
        }
        if now==1000{for net in &mut transports{net.disconnect_and_resume();}}
        let start=Instant::now();
        for i in 0..maps{
            let work=network[i].min(peers*6);let bytes=network_bytes[i].min(peers*8192);
            let report=transports[i].poll(&simulations[i],now,work,bytes,true);
            network[i]-=report.visited;network_bytes[i]-=report.bytes;
        }
        transport_ms+=start.elapsed().as_secs_f64()*1000.;
        peak_public_cache=peak_public_cache.max(simulations.iter().map(|s|s.components.public_view_cache_bytes()).sum());
        let start=Instant::now();while let Some(completion)=store.try_poll().unwrap(){match completion.kind{
            CompletionKind::Commit(Ok(receipt))=>{confirmed+=1;assert_eq!(receipt.mark.sequence,confirmed as u64);
                commit_latency.push(submitted.remove(&completion.ticket).unwrap().elapsed().as_secs_f64()*1000.);
                for change in pending.remove(&completion.ticket).unwrap(){if let Some(value)=change.value{durable.insert(change.key,value);}else{durable.remove(&change.key);}}
            },CompletionKind::Checkpoint(Ok(_))=>{checkpoint_pending=false;checkpoints+=1;},other=>panic!("unexpected persistence result {other:?}"),
        }}
        if now>0&&now%1000==0&&!checkpoint_pending&&store.confirmed_mark().sequence>0{
            if store.try_checkpoint().is_ok(){checkpoint_pending=true;}
        }
        peak_store=peak_store.max(store.retained_bytes());persistence_ms+=start.elapsed().as_secs_f64()*1000.;
        if now<ticks*50{
            let after=[simulation_ms,persistence_ms,transports.iter().map(|n|n.traffic.sender_ms).sum(),
                transports.iter().map(|n|n.traffic.ack_ms).sum(),transports.iter().map(|n|n.traffic.client_model_ms).sum(),
                transports.iter().map(|n|n.traffic.oracle_capture_ms).sum()];
            let delta:Vec<_>=after.iter().zip(before).map(|(&a,b)|a-b).collect();
            let row=&mut samples[(now/50)as usize];
            // Main-thread wall cost excluding client simulation and oracle.
            // Background journal CPU/IO is represented by commit latency below.
            row[0]+=(iteration.elapsed().as_secs_f64()*1000.-delta[4]-delta[5]).max(0.);
            row[1]+=(delta[0]-delta[5]).max(0.);row[2]+=delta[2];row[3]+=delta[3];
            row[4]+=delta[1];row[5]+=delta[4];row[6]+=delta[5];
        }
        if now>=ticks*50&&dirty.is_empty()&&pending.is_empty()&&store.outstanding()==0
            &&transports.iter().all(|net|net.drained(Tick(ticks-1))){finished=Some(now);break;}
        if now>end&&store.outstanding()>0{std::thread::sleep(Duration::from_millis(1));}
    }
    let finished=finished.expect("combined server did not drain");
    let elapsed_ms=started.elapsed().as_secs_f64()*1000.;
    for (sim,net) in simulations.iter().zip(&transports){net.verify(sim);assert!(sim.activity.skills>0&&sim.activity.pulses>0&&sim.activity.area_hits>0&&sim.activity.projectile_hits>0&&sim.activity.births>2&&sim.activity.removals>0,"missing exercised phase: {:?}",sim.activity);}
    // Different transport conditions must not change identical map authority.
    // Optional navigation can legitimately finish on different ticks under a
    // restricted global budget. Each map still has its own exact network oracle.
    for sim in simulations[1..].iter().take(if regions{2}else{3}).filter(|_|!limited){
        for (&a,&b) in simulations[0].ids.iter().zip(&sim.ids){
            assert_eq!(simulations[0].world.entity_state(a).unwrap(),sim.world.entity_state(b).unwrap());
            assert_eq!(simulations[0].world.health(a).unwrap(),sim.world.health(b).unwrap());
            assert_eq!(simulations[0].world.attack_state(a).unwrap(),sim.world.attack_state(b).unwrap());
            assert_eq!(simulations[0].world.components(a).unwrap(),sim.world.components(b).unwrap());
        }
    }
    let ready_ticks=(0..ticks).filter(|t|t%12==2).count();
    let pulse_ticks=(0..ticks).filter(|t|[1,4,7].contains(&(t%12))).count();
    for sim in &simulations{assert_eq!(sim.activity.skills,ready_ticks*8);assert_eq!(sim.activity.pulses,pulse_ticks*8);sim.verify_region();}
    if limited{assert!(deferred_ticks>0&&oldest_wait>0);assert!(navigation_granted.iter().all(|&n|n>0),"starved maps: {navigation_granted:?}");}
    let mark=store.confirmed_mark();let closed=store.shutdown();closed.worker.unwrap();assert!(closed.completions.is_empty());
    let recovered=recover(&path,options).unwrap();assert_eq!(recovered.mark,mark);assert_eq!(recovered.records,durable);
    if let Some(idle)=&dormant{idle.verify(ticks);assert_eq!(recovered.records[&RecordKey{table:5,id:1}].as_ref(),idle.record());println!("dormant_map entities=64 wakes={} sleeping_ticks={} restored=true",idle.wakes,idle.sleeping_ticks);}
    for (index,sim) in simulations.iter().enumerate(){for (row,&id) in sim.ids.iter().take(8).enumerate(){
        let bytes=&recovered.records[&RecordKey{table:index as u16+1,id:row as u64+1}];
        assert_eq!(u64::from_le_bytes(bytes[..8].try_into().unwrap()),ticks-1);
        let position=sim.world.position(id).unwrap();
        assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()),position.x);
        assert_eq!(i32::from_le_bytes(bytes[12..16].try_into().unwrap()),position.y);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()),sim.world.health(id).unwrap().current);
    }}
    println!("maps={maps} entities_per_map={count} peers_per_map={peers} ticks={ticks} tcp={tcp} wall_clock={wall_clock} elapsed_ms={elapsed_ms:.3} missed_deadlines={missed_deadlines} max_lateness_ms={max_lateness_ms:.3} finished_ms={finished} simulation_ms={simulation_ms:.3} transport_ms={transport_ms:.3} persistence_main_ms={persistence_ms:.3} confirmed={confirmed} checkpoints={checkpoints} backpressure={pressure} peak_store_reservation={peak_store} restored={} verified=true",recovered.records.len());
    for (index,(sim,net)) in simulations.iter().zip(&transports).enumerate(){println!("map={index} activity={:?} traffic={:?}",sim.activity,net.traffic);}
    for(index,net)in transports.iter().enumerate(){println!("write_gaps map={index} ms={:?}",net.write_gaps());}
    println!("synthetic_store={}",path.display());
    println!("scenarios regions={regions} jitter={jitter} verified=true");
    println!("ai_cancellations={:?}",simulations.iter().map(|sim|sim.ai_cancellations).collect::<Vec<_>>());
    println!("work_budget limited={limited} deferred_ticks={deferred_ticks} overdue_ticks={overdue_ticks} overload_ticks={overload_ticks} oldest_wait={oldest_wait} navigation_granted={navigation_granted:?} peak_public_cache={peak_public_cache}");
    let warm=20usize;let columns:Vec<Vec<f64>>=(0..7).map(|i|samples[warm..].iter().map(|r|r[i]).collect()).collect();
    println!("metrics={{\"maps\":{maps},\"entitiesPerMap\":{count},\"peersPerMap\":{peers},\"ticks\":{ticks},\"warm\":{warm},\"tcp\":{tcp},\"wallClock\":{wall_clock},\"elapsedMs\":{elapsed_ms},\"missedDeadlines\":{missed_deadlines},\"maxLatenessMs\":{max_lateness_ms},\"serverThreadWall\":{},\"simulationObserve\":{},\"send\":{},\"ack\":{},\"persistenceMain\":{},\"clientAndValidation\":{},\"oracleCapture\":{},\"commitLatency\":{},\"samples\":{samples:?},\"verified\":true}}",
        metrics::stats(&columns[0]),metrics::stats(&columns[1]),metrics::stats(&columns[2]),metrics::stats(&columns[3]),metrics::stats(&columns[4]),metrics::stats(&columns[5]),metrics::stats(&columns[6]),metrics::stats(&commit_latency));
}
