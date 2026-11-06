use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(x:u32)->NonZeroU32{NonZeroU32::new(x).unwrap()}
fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn view()->ViewRect{ViewRect{min:p(-512),max:p(512)}}
fn spawn(w:&mut World,x:i32)->EntityId{let id=w.spawn(p(x)).unwrap();w.configure_health(id,1000,n(1000)).unwrap();id}
fn set_health(w:&mut World,id:EntityId,hp:u32,max:u32){
    let before=w.health(id).unwrap();let mut commands=vec![HealthCommand::SetMaximum{entity:id,maximum:n(max)},HealthCommand::Damage{entity:id,amount:u32::MAX}];if hp>0{commands.push(HealthCommand::Revive{entity:id,current:n(hp)});}
    let mut out=vec![Err(HealthError::Unconfigured);commands.len()];let mut changes=vec![HealthChange{entity:id,before,after:before};commands.len()];w.apply_health_commands(&commands,&mut out,&mut changes).unwrap();assert!(out.iter().all(Result::is_ok));
}
fn phase(w:&mut World)->Vec<CombatEvent>{
    w.advance_tick().unwrap();let id=EntityId{index:0,generation:0};let h=HealthState{current:1,maximum:n(1)};
    let mut e=vec![CombatEvent{attacker:id,target:id,kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};w.active_combatants()];let mut c=vec![HealthChange{entity:id,before:h,after:h};w.active_combatants()];let r=w.resolve_combat(&mut e,&mut c).unwrap();e.truncate(r.events_written);e
}
fn drain(s:&mut CombatSyncSender,c:&mut CombatReplica)->(usize,Vec<CombatFeedback>){let(mut size,mut events)=(0,Vec::new());while let Some(b)=s.prepare().unwrap(){size+=b.len();let r=c.apply(b).unwrap();events.extend(c.drain_feedback());s.acknowledge_packet(&r.acknowledgement()).unwrap();}(size,events)}
#[test]
fn compact_matches_legacy_through_visibility_health_windup_and_reuse(){
    let mut w=World::default();let mut ids:Vec<_>=(0..32).map(|i|spawn(&mut w,(i-16)*32)).collect();let attack=AttackProfile{range:128,damage:n(10),cooldown_ticks:NonZeroU16::new(2).unwrap(),windup_ticks:2};
    for i in (0..32).step_by(2){w.start_attack(ids[i],ids[i+1],attack).unwrap();}
    let config=CombatSyncConfig{events_per_packet:2,..CombatSyncConfig::default()};let(mut old,mut new)=(CombatSyncSender::new(17,config).unwrap(),CombatSyncSender::new_compact(17,config).unwrap());let(mut a,mut b)=(CombatReplica::new(17),CombatReplica::new(17));let mut f=CombatFrame::new(n(64));let(mut old_bytes,mut new_bytes)=(0,0);
    for tick in 1..=64{
        if tick%3==0{w.set_position(ids[3],p(if tick%6==0{4096}else{0})).unwrap();}
        if tick%7==0{set_health(&mut w,ids[7],u32::MAX-tick,u32::MAX);}
        if tick%11==0{w.despawn(ids[9]).unwrap();ids[9]=w.spawn(p(64)).unwrap();}
        let events=phase(&mut w);f.capture(&w,&events).unwrap();let x=if tick%8<4{-256}else{0};let v=ViewRect{min:p(x),max:p(x+512)};
        for s in [&mut old,&mut new]{s.observe(&f,v).unwrap();}let(x,ea)=drain(&mut old,&mut a);let(y,eb)=drain(&mut new,&mut b);old_bytes+=x;new_bytes+=y;assert_eq!(ea,eb);assert_eq!(a.states(),b.states());
        let mut expected:Vec<_>=w.motion_states().filter(|s|v.contains(s.position)).map(|s|s.entity).collect();expected.sort_by_key(|e|(e.index,e.generation));assert_eq!(b.states().iter().map(|s|s.entity).collect::<Vec<_>>(),expected);
        for state in b.states(){assert_eq!(state.health,w.health(state.entity).ok());}
    }
    assert!(new_bytes<old_bytes/2);
}
// Independent IC v2 fixtures; no production encoder is used below.
fn var(mut x:u64)->Vec<u8>{let mut out=Vec::new();loop{let low=(x&127)as u8;x>>=7;out.push(low|if x==0{0}else{128});if x==0{return out;}}}
fn packet(sequence:u64,base:u64,now:u64,reset:bool,common:u32,rows:&[Vec<u8>],events:&[Vec<u8>])->Vec<u8>{let mut b=b"IC\x02".to_vec();b.push(u8::from(reset));for x in [17,sequence,base,now]{b.extend(x.to_le_bytes());}b.extend((rows.len()as u16).to_le_bytes());b.extend((events.len()as u16).to_le_bytes());b.extend(var(common.into()));for row in rows{b.extend(row);}for e in events{b.extend(e);}b}
fn reject(c:&mut CombatReplica,b:&[u8]){let before=c.states().to_vec();let seq=c.sequence();assert!(c.apply(b).is_err(),"accepted {b:?}");assert_eq!(c.states(),before);assert_eq!(c.sequence(),seq);assert_eq!(c.drain_feedback().count(),0);}
#[test]
fn common_enter_fast_paths_keep_integer_and_atomic_validation(){
    for version in [2,3]{
        let seed=packet(1,0,10,true,50,&[vec![0,0,200]],&[]);
        let mut c=CombatReplica::new(17);c.apply(&seed).unwrap();
        for value in [0,127,128,16383,16384,u32::MAX]{
            let mut row=vec![0,0,73];row.extend(var(u64::from(value)*2));
            let mut good=packet(2,0,11,true,u32::MAX,&[row],&[]);good[2]=version;
            for len in 0..good.len(){reject(&mut c,&good[..len]);}
            let mut decoded=CombatReplica::new(17);decoded.apply(&good).unwrap();assert_eq!(decoded.states()[0].health.unwrap(),HealthState{current:value,maximum:n(u32::MAX)});
        }
        for code in [1,3,102,u64::from(u32::MAX)*2+2,u64::MAX-1]{
            let mut row=vec![0,0,73];row.extend(var(code));let mut bad=packet(2,0,11,true,50,&[row],&[]);bad[2]=version;reject(&mut c,&bad);
        }
        for row in [vec![0,0,200],vec![0,0,73,0]]{let mut bad=packet(2,0,11,true,0,&[row],&[]);bad[2]=version;reject(&mut c,&bad);}
        let mut duplicate=packet(2,1,11,false,50,&[vec![0,0,200]],&[]);duplicate[2]=version;reject(&mut c,&duplicate);
    }
}
#[test]
fn windup_lookup_reuse_does_not_hide_self_stale_or_removed_targets(){
    for version in [2,3]{
        // Two remote-target references, then adjacent-target and backward cases.
        let rows=vec![vec![0,0,204,1,7,0,0,2],vec![1,0,204,1,7,0,0,2],vec![1,0,204,1,3,0,0,2],vec![1,0,200],vec![4,0,204,1,0,0,0,2]];
        let mut seed=packet(1,0,10,true,50,&rows,&[]);seed[2]=version;let mut c=CombatReplica::new(17);c.apply(&seed).unwrap();assert_eq!(c.states().len(),5);
        for edits in [vec![vec![7,0,16]],vec![vec![1,0,4,1,7,1,0,2]],vec![vec![7,0,4,1,7,0,0,2]]]{let mut bad=packet(2,1,10,false,0,&edits,&[]);bad[2]=version;reject(&mut c,&bad);}
        // The first reference establishes a cached target; that target itself
        // must still fail its own self-reference check later in the same scan.
        let mut bad_rows=rows.clone();bad_rows[2]=vec![1,0,200];bad_rows[4]=vec![4,0,204,1,7,0,0,2];let mut bad=packet(2,0,10,true,50,&bad_rows,&[]);bad[2]=version;reject(&mut c,&bad);
        let mut clear=packet(2,0,11,true,0,&[],&[]);clear[2]=version;c.apply(&clear).unwrap();assert!(c.states().is_empty());
    }
}
#[test]
fn compact_independent_fixtures_are_atomic_canonical_and_checked(){
    let seed=packet(1,0,10,true,50,&[vec![0,0,200],vec![1,0,200]],&[]);let mut c=CombatReplica::new(17);c.apply(&seed).unwrap();assert_eq!(seed.len(),47);
    let good=packet(2,1,11,false,0,&[vec![1,0,1,3]],&[vec![2,0,0,0,1,0,1,49]]);
    for len in 0..good.len(){reject(&mut c,&good[..len]);}
    for row in [vec![1,0,129,0],vec![1,0,66,1],vec![1,0,32|64],vec![1,0,1,128,0],vec![1,0,1,255,255,255,255,255,255,255,255,255,2],vec![2,0,8|1|64,3]]{reject(&mut c,&packet(2,1,11,false,50,&[row],&[]));}
    let mut unchanged=CombatReplica::new(17);unchanged.apply(&seed).unwrap();unchanged.apply(&packet(2,1,11,false,50,&[vec![1,0,64]],&[])).unwrap();assert_eq!(unchanged.states(),c.states());
    reject(&mut c,&packet(2,1,11,false,0,&[vec![1,0,64]],&[]));
    let mut tail=good.clone();tail.push(0);reject(&mut c,&tail);
    for event in [vec![2,12,0,0,1,0,1,49],vec![0,0,0,0,1,0,255,255,255,255,255,255,255,255,255,1],vec![1,0,0,0,1,0,12],vec![2,0,0,0,0,0,1,49]]{reject(&mut c,&packet(2,1,11,false,0,&[],&[event]));}
    let report=c.apply(&good).unwrap();assert_eq!(c.states()[1].health.unwrap().current,49);assert!(c.apply(&good).unwrap().duplicate);let events:Vec<_>=c.drain_feedback().collect();assert_eq!(events[0].kind,CombatFeedbackKind::Hit{damage:1,remaining:49});assert_eq!(report.acknowledgement().len(),20);
    let mut big=var(u32::MAX.into());big.extend(var(u32::MAX.into()));big.push(200);let reset=packet(3,0,u64::MAX,true,u32::MAX,&[big.clone()],&[]);c.apply(&reset).unwrap();assert_eq!(c.states()[0].entity,EntityId{index:u32::MAX,generation:u32::MAX});assert_eq!(c.states()[0].health.unwrap().current,u32::MAX);
    reject(&mut c,&packet(4,0,u64::MAX,true,u32::MAX,&[big,vec![1,0,40]],&[]));
}
#[test]
fn compact_ack_wait_retransmission_and_delayed_events_use_confirmed_state(){
    let mut w=World::default();let a=spawn(&mut w,0);let b=spawn(&mut w,64);w.start_attack(a,b,AttackProfile{range:96,damage:n(10),cooldown_ticks:NonZeroU16::new(2).unwrap(),windup_ticks:2}).unwrap();
    let mut f=CombatFrame::new(n(64));let mut s=CombatSyncSender::new_compact(17,CombatSyncConfig{events_per_packet:1,..CombatSyncConfig::default()}).unwrap();let mut c=CombatReplica::new(17);
    let e=phase(&mut w);f.capture(&w,&e).unwrap();s.observe(&f,view()).unwrap();let first=s.prepare().unwrap().unwrap().to_vec();assert_eq!(first[2],2);let ack=c.apply(&first).unwrap().acknowledgement();assert!(c.apply(&first).unwrap().duplicate);let mut actual:Vec<_>=c.drain_feedback().collect();
    for _ in 2..=8{let e=phase(&mut w);f.capture(&w,&e).unwrap();s.observe(&f,view()).unwrap();assert_eq!(s.pending_packet().unwrap(),first);assert_eq!(s.prepare(),Err(SyncError::WaitingForAck.into()));}
    let mut wrong=ack;wrong[4]^=1;assert!(s.acknowledge_packet(&wrong).is_err());assert_eq!(s.pending_packet().unwrap(),first);s.acknowledge_packet(&ack).unwrap();actual.extend(drain(&mut s,&mut c).1);assert_eq!(actual.iter().map(|e|e.tick.0).collect::<Vec<_>>(),vec![1,3,5,7]);assert_eq!(c.states()[1].health.unwrap().current,980);
    s.request_full().unwrap();let reset=s.prepare().unwrap().unwrap().to_vec();assert_eq!(reset[3],1);let r=c.apply(&reset).unwrap();s.acknowledge_packet(&r.acknowledgement()).unwrap();assert!(c.apply(&first).is_err());
}
#[test]
fn compact_health_extremes_full_flags_component_removal_and_reentry(){
    let mut w=World::default();let id=spawn(&mut w,0);let mut f=CombatFrame::new(n(64));let mut s=CombatSyncSender::new_compact(17,CombatSyncConfig::default()).unwrap();let mut c=CombatReplica::new(17);
    for (hp,max)in [(u32::MAX,u32::MAX),(u32::MAX-1,u32::MAX),(0,u32::MAX),(u32::MAX,u32::MAX),(1,1),(0,1),(1,u32::MAX)]{set_health(&mut w,id,hp,max);w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();s.observe(&f,view()).unwrap();drain(&mut s,&mut c);assert_eq!(c.states()[0].health.unwrap(),HealthState{current:hp,maximum:n(max)});}
    for x in [4096,0]{w.set_position(id,p(x)).unwrap();w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();s.observe(&f,view()).unwrap();drain(&mut s,&mut c);assert_eq!(c.states().len(),usize::from(x==0));}
    w.despawn(id).unwrap();let replacement=w.spawn(p(0)).unwrap();w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();s.observe(&f,view()).unwrap();drain(&mut s,&mut c);assert_eq!(c.states(),&[VitalState{entity:replacement,health:None,windup:None}]);
}
#[test]
fn compact_full_visibility_replacement_is_bounded(){
    let mut w=World::default();let ids:Vec<_>=(0..8192).map(|i|spawn(&mut w,i)).collect();let mut f=CombatFrame::new(n(64));let mut s=CombatSyncSender::new_compact(17,CombatSyncConfig::default()).unwrap();let mut c=CombatReplica::new(17);
    for start in [0,4096,0]{w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();s.observe(&f,ViewRect{min:p(start),max:p(start+4095)}).unwrap();let bytes=s.prepare().unwrap().unwrap().to_vec();assert!(bytes.len()<=MAX_COMBAT_SYNC_PACKET_BYTES);let r=c.apply(&bytes).unwrap();s.acknowledge_packet(&r.acknowledgement()).unwrap();assert_eq!(c.states().len(),4096);assert_eq!(c.states()[0].entity,ids[start as usize]);}
}

#[test]
fn compact_event_only_keeps_snapshot_and_empty_reset_clears_it(){
    let seed=packet(1,0,10,true,50,&[vec![0,0,204,1,1,0,0,2],vec![1,0,200]],&[]);
    let mut c=CombatReplica::new(17);c.apply(&seed).unwrap();let original=c.states().to_vec();
    let event_only=packet(2,1,11,false,0,&[],&[vec![2,0,0,0,1,0,1,49]]);c.apply(&event_only).unwrap();assert_eq!(c.states(),original);assert_eq!(c.drain_feedback().count(),1);
    reject(&mut c,&packet(3,2,12,false,0,&[],&[vec![2,0,0]]));assert!(c.apply(&event_only).unwrap().duplicate);assert_eq!(c.states(),original);
    c.apply(&packet(3,0,12,true,0,&[],&[])).unwrap();assert!(c.states().is_empty());
    c.apply(&packet(4,3,13,false,0,&[],&[vec![2,1,0,0,1,0,1,48]])).unwrap();assert!(c.states().is_empty());assert_eq!(c.drain_feedback().count(),1);
}

#[test]
fn compact_short_id_paths_and_fallbacks_preserve_generations_and_truncation_checks(){
    for index in [1,127,128,16383,16384,u32::MAX]{for generation in [0,127,128,u32::MAX]{
        let mut owner=vec![0];owner.extend(var(generation.into()));owner.extend([204,1]);owner.extend(var(index.into()));owner.extend(var(generation.into()));owner.extend([0,3]);
        let mut target=var(index.into());target.extend(var(generation.into()));target.push(200);
        let mut event=vec![0,0,0];event.extend(var(generation.into()));event.extend(var(index.into()));event.extend(var(generation.into()));event.push(3);
        let bytes=packet(1,0,2000,true,u32::MAX,&[owner,target],&[event]);let mut c=CombatReplica::new(17);
        for len in 0..bytes.len(){reject(&mut c,&bytes[..len]);}c.apply(&bytes).unwrap();let target=EntityId{index,generation};assert_eq!(c.states()[1].entity,target);assert_eq!(c.states()[0].windup,Some(WindupView{target,started_at:Tick(2000),hit_at:Tick(2003)}));
        let events:Vec<_>=c.drain_feedback().collect();assert_eq!(events[0].target,target);assert_eq!(events[0].attacker,EntityId{index:0,generation});
    }}
}

#[test]
fn snapshot_rows_survive_pending_ack_dense_swaps_and_view_replacement(){
    for compact in [false,true]{
        let mut w=World::default();let mut ids:Vec<_>=(0..64).map(|i|spawn(&mut w,i)).collect();
        let mut s=if compact{CombatSyncSender::new_compact(17,CombatSyncConfig::default())}else{CombatSyncSender::new(17,CombatSyncConfig::default())}.unwrap();
        let mut c=CombatReplica::new(17);let mut f=CombatFrame::new(n(8));
        let rect=|start|ViewRect{min:p(start),max:p(start+15)};
        for start in [0,32,8]{
            w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();s.observe(&f,rect(start)).unwrap();
            let pending=s.prepare().unwrap().unwrap().to_vec();
            let expected:Vec<_>=ids.iter().copied().filter(|id|rect(start).contains(w.position(*id).unwrap())).map(|entity|VitalState{entity,health:Some(w.health(entity).unwrap()),windup:None}).collect();
            // Move to another snapshot while the earlier one is in flight,
            // including a dense swap and reuse of an entity generation.
            for _ in 0..3{
                w.despawn(ids[1]).unwrap();ids[1]=spawn(&mut w,1);set_health(&mut w,ids[12],777,1000);
                w.advance_tick().unwrap();f.capture(&w,&[]).unwrap();s.observe(&f,rect(16)).unwrap();
                assert_eq!(s.pending_packet().unwrap(),pending);assert_eq!(s.prepare(),Err(SyncError::WaitingForAck.into()));
            }
            let report=c.apply(&pending).unwrap();assert_eq!(c.states(),expected);s.acknowledge_packet(&report.acknowledgement()).unwrap();
            drain(&mut s,&mut c);let expected:Vec<_>=ids.iter().copied().filter(|id|rect(16).contains(w.position(*id).unwrap())).map(|entity|VitalState{entity,health:Some(w.health(entity).unwrap()),windup:None}).collect();assert_eq!(c.states(),expected);
            s.request_full().unwrap();drain(&mut s,&mut c);assert_eq!(c.states(),expected);
        }
    }
}
