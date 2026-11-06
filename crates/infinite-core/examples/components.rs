//! Real skill/effect phases -> component projections -> delayed ACK/delta decode.
//! Nearby peer sees appearance, owner alone sees skills, same-map drop is shared.
use infinite_core::*;
use std::num::{NonZeroU16,NonZeroU32};
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn p(x:i32)->GridPosition{GridPosition{x,y:0}}
fn main(){
    let mut world=World::default();let owner=world.spawn(p(0)).unwrap();let observer=world.spawn(p(10)).unwrap();let drop=world.spawn(p(5000)).unwrap();
    for id in [owner,observer]{world.configure_health(id,100,n(100)).unwrap();}
    world.configure_attributes(owner,CombatAttributes{attack:10,break_power:0,defense:3,maximum_health:n(100),maximum_resource:20,level:NonZeroU16::new(1).unwrap()}).unwrap();
    world.resolve_attributes(&mut Vec::new());world.configure_resource(owner,20,20).unwrap();
    world.learn_skill(owner,SkillSpec{id:SkillId(1),resource_cost:5,cooldown_ticks:8,windup_ticks:2,recovery:None,ingress_grace_ticks:0}).unwrap();
    world.start_cast(CastRequest{caster:owner,request_id:1,skill:SkillId(1),target:CastTarget::Entity(observer)}).unwrap();
    world.apply_periodic(PeriodicEffect{source:owner,target:owner,key:3,kind:PeriodicKind::Damage,amount:n(2),first_delay:1,interval:NonZeroU16::new(1).unwrap(),pulses:NonZeroU16::new(3).unwrap(),refresh:PeriodicRefresh::Replace}).unwrap();
    world.set_component(drop,GroundItemComponent{definition:50,count:n(2),position:p(5000),height:0,source:owner,from_monster:true,exclusive_owner:Some(42),exclusive_until:Tick(30),expires_at:Tick(100)}.record()).unwrap();
    let mut frame=ComponentFrame::new(n(64));let view=ViewRect{min:GridPosition{x:-32,y:-32},max:GridPosition{x:32,y:32}};
    let mut senders=[ComponentSender::new(1,Some(owner)),ComponentSender::new(2,Some(observer))];let mut replicas=[ComponentReplica::new(1),ComponentReplica::new(2)];
    let(mut skills,mut effects,mut combat)=(Vec::new(),Vec::new(),Vec::new());let(mut bytes,mut duplicates,mut ready)=(0usize,0usize,0usize);let mut held=None;
    for tick in 1..=6{
        world.advance_tick().unwrap();skills.clear();effects.clear();combat.clear();world.resolve_casts(16,&mut skills).unwrap();ready+=skills.iter().filter(|s|matches!(s,SkillEvent::Ready(_))).count();world.resolve_effects(16,&mut effects,&mut combat).unwrap();
        world.publish_skill_component(owner,SkillId(1)).unwrap();world.publish_attribute_component(owner).unwrap();world.publish_effect_components(owner,ComponentScope::Owner).unwrap();
        world.set_component(owner,AppearanceComponent{model:1,weapon:if tick>=2{20}else{10},armor:3,palette:0,style:0}.record()).unwrap();
        if tick==4{world.despawn(drop).unwrap();}
        frame.capture(&world).unwrap();
        for index in 0..2{
            if senders[index].pending_packet().is_none(){senders[index].prepare(&frame,view).unwrap();}
            if let Some(packet)=senders[index].pending_packet(){
                bytes+=packet.len();let report=replicas[index].apply(packet).unwrap();duplicates+=usize::from(report.duplicate);
                if index==0&&tick<3{held=Some(report.acknowledgement());}else{senders[index].acknowledge_packet(&report.acknowledgement()).unwrap();}
            }
        }
        if tick==3{senders[0].acknowledge_packet(&held.take().unwrap()).unwrap();}
        assert!(replicas[1].states().iter().all(|r|r.record.scope!=ComponentScope::Owner));
        if tick<4{assert!(replicas[1].states().iter().any(|r|r.entity==drop));}
    }
    assert_eq!(ready,1);assert_eq!(world.health(owner).unwrap().current,94);assert_eq!(world.resource(owner).unwrap().current,15);assert_eq!(duplicates,2);
    for (index,replica) in replicas.iter().enumerate(){let mut selector=ComponentSelector::new(Some(if index==0{owner}else{observer}));assert_eq!(replica.states(),selector.select(&frame,view).unwrap());assert!(!replica.states().iter().any(|r|r.entity==drop||r.record.key.kind==ComponentKind::Effect));}
    let skill=replicas[0].states().iter().find(|r|r.record.key.kind==ComponentKind::Skill).unwrap();assert_eq!(skill.record.fields.get(5),Some(0));assert_eq!(skill.record.fields.get(6),None);assert_eq!(skill.record.fields.get(1),Some(8));
    println!("skill_ready={ready} hp=94 resource=15 duplicate_packets={duplicates} owner_private=true map_drop_removed=true replicas_match=true bytes={bytes}");
}
