use infinite_core::*;use std::num::NonZeroU32;
fn n(v:u32)->NonZeroU32{NonZeroU32::new(v).unwrap()}
fn main(){
 let mut w=World::default();let caster=w.spawn(GridPosition::default()).unwrap();let target=w.spawn(GridPosition{x:16,y:0}).unwrap();for id in [caster,target]{w.configure_health(id,100,n(100)).unwrap();}w.configure_resource(caster,40,40).unwrap();
 w.learn_skill(caster,SkillSpec{id:SkillId(1),resource_cost:20,cooldown_ticks:6,windup_ticks:2,recovery:None,ingress_grace_ticks:0}).unwrap();
 let request=CastRequest{caster,request_id:1,skill:SkillId(1),target:CastTarget::Entity(target)};
 println!("admitted {:?}",w.start_cast(request).unwrap());println!("retry {:?}",w.start_cast(request).unwrap());
 let mut tasks=Vec::new();let mut events=Vec::new();let mut actions=CombatActions::new(n(32));
 for _ in 0..3{w.advance_tick().unwrap();let report=w.resolve_casts(32,&mut tasks).unwrap();println!("resolved {report:?}");for &event in &tasks{
   if let SkillEvent::Ready(cast)=event{let center=match cast.request.target{CastTarget::Entity(id)=>w.position(id).unwrap(),CastTarget::Point(p)=>p};let mut budget=CombatQueryBudget::new(100);
    let hit=actions.begin(&mut w,&mut budget,&mut events,4096).unwrap().damage_area(AreaDamage{attacker:caster,area:CombatArea::Circle{center,radius:8},damage:n(17),require_sight:false},|_,id|id==target).unwrap();println!("impact {hit:?}");
   }
 }}
 println!("resource={:?}, target={:?}, hits={}",w.resource(caster).unwrap(),w.health(target).unwrap(),events.len());
}
