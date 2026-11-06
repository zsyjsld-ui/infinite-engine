use infinite_core::*;
use std::num::{NonZeroU16, NonZeroU32};
pub fn n(value:u32)->NonZeroU32{NonZeroU32::new(value).unwrap()}
pub fn s(value:u16)->NonZeroU16{NonZeroU16::new(value).unwrap()}
pub fn full_view()->ViewRect{ViewRect{min:GridPosition{x:-1,y:-1},max:GridPosition{x:2048,y:2048}}}
fn appearance(weapon:u32)->ComponentRecord{
    let mut record=AppearanceComponent{model:1,weapon,armor:1,palette:0,style:0}.record();
    record.fields.set_reference(0,1).unwrap();record
}

#[derive(Default,Debug)]
pub struct Activity {
    pub skills:usize,pub pulses:usize,pub basic_hits:usize,pub area_hits:usize,
    pub projectile_hits:usize,pub births:usize,pub removals:usize,pub ai_visits:usize,
}
struct RegionProbe { target:EntityId, actors:Vec<EntityId>, slept_before:bool, acquired:bool, slept_after:bool }
pub struct Simulation {
    pub world:World,pub ids:Vec<EntityId>,pub combat:CombatFrame,pub components:ComponentFrame,
    pub events:Vec<CombatEvent>,pub activity:Activity,pub ai_cancellations:usize,
    ai:AiSystem,actions:CombatActions,projectiles:Projectiles,players:usize,actors:usize,
    group:Vec<EntityId>,drop:EntityId,template:SpawnTemplate,skills:Vec<SkillEvent>,effects:Vec<EffectEvent>,
    attributes:Vec<AttributeChange>,shields:Vec<ShieldExpired>,life:Vec<LifecycleEvent>,
    decisions:Vec<AiEvent>,ends:Vec<ProjectileEnd>,basic:Vec<CombatEvent>,changes:Vec<HealthChange>,
    region:Option<RegionProbe>,
}
impl Simulation {
    pub fn new(count:usize)->Self{
        assert!((64..=3000).contains(&count));
        let mut map=CollisionMap::new(GridPosition::default(),n(96),n(96),n(16)).unwrap();
        for y in 0..40{map.set_blocked(32,y,true).unwrap();}map.prepare_navigation();
        let mut world=World::with_map(DEFAULT_TICK_MS,map);
        world.define_public_component(1,&[0]).unwrap();world.define_public_component(2,b"synthetic item").unwrap();
        let players=(count/32).clamp(8,64);let actors=(count/16).clamp(8,128);
        let mut ids=Vec::new();
        for i in 0..count{
            let mut position=GridPosition{x:(i%40)as i32*24+8,y:((i/40)%40)as i32*24+8};
            if !world.collision_map().unwrap().is_walkable(position){position.x+=64;}
            let id=world.spawn(position).unwrap();world.configure_health(id,1_000_000,n(1_000_000)).unwrap();
            world.set_component(id,appearance(1)).unwrap();
            if i<players+actors{
                world.configure_attributes(id,CombatAttributes{attack:10,break_power:1,defense:3,
                    maximum_health:n(1_000_000),maximum_resource:10000,level:s(10)}).unwrap();
            }
            if i<players{
                world.register_ai_target(id,true).unwrap();
                world.learn_skill(id,SkillSpec{id:SkillId(1),resource_cost:1,cooldown_ticks:12,
                    windup_ticks:2,recovery:None,ingress_grace_ticks:0}).unwrap();
            }
            ids.push(id);
        }
        world.resolve_attributes(&mut Vec::new());
        for &id in &ids[..players+actors]{world.configure_resource(id,10000,10000).unwrap();}
        for &id in &ids[players..players+actors]{
            world.register_ai_actor(id,AiProfile{aggro_radius:768,leash_radius:2048,bonus_radius:1024,
                attack:AttackProfile{range:32,damage:n(3),cooldown_ticks:s(6),windup_ticks:2},
                attack_break_range:64,chase:ChaseConfig{speed:s(320),..ChaseConfig::default()}}).unwrap();
        }
        world.resize_population(1).unwrap();
        let template=SpawnTemplate{maximum_health:n(20),corpse_ticks:2,respawn_ticks:5,ai:None};
        let mut group=Vec::new();world.spawn_group(1,None,template,&[GridPosition{x:1200,y:1000},GridPosition{x:1224,y:1000}],&mut SpawnBudget::new(2,1).unwrap(),&mut group).unwrap();
        let drop=world.spawn(GridPosition{x:1300,y:1000}).unwrap();
        let h=world.health(ids[0]).unwrap();
        let dummy=CombatEvent{attacker:ids[0],target:ids[1],kind:CombatEventKind::Ended(CombatEndReason::TargetGone)};
        Self{world,ids,combat:CombatFrame::new(n(128)),components:ComponentFrame::new(n(128)),
            events:Vec::new(),activity:Activity{births:2,..Activity::default()},ai_cancellations:0,ai:AiSystem::new(n(128)),
            actions:CombatActions::new(n(128)),projectiles:Projectiles::new(n(128)),players,actors,
            group,drop,template,skills:Vec::new(),effects:Vec::new(),attributes:Vec::new(),shields:Vec::new(),
            life:Vec::new(),decisions:Vec::new(),ends:Vec::new(),basic:vec![dummy;actors+16],
            changes:vec![HealthChange{entity:EntityId{index:0,generation:0},before:h,after:h};actors+16],region:None}
    }

    /// An otherwise idle remote region in this map; unrelated maps keep running.
    pub fn enable_region_probe(&mut self) {
        let actors=self.ids[self.ids.len()-8..].to_vec();let target=self.ids[self.ids.len()-9];
        for (index,&id) in actors.iter().enumerate(){
            self.world.set_position(id,GridPosition{x:1000+(index%4)as i32*8,y:1400+(index/4)as i32*8}).unwrap();
            self.world.register_ai_actor(id,AiProfile{aggro_radius:96,leash_radius:128,bonus_radius:128,
                attack:AttackProfile{range:24,damage:n(1),cooldown_ticks:s(6),windup_ticks:2},
                attack_break_range:32,chase:ChaseConfig{speed:s(320),..ChaseConfig::default()}}).unwrap();
        }
        self.world.register_ai_target(target,true).unwrap();
        self.region=Some(RegionProbe{target,actors,slept_before:false,acquired:false,slept_after:false});
    }
    pub fn verify_region(&self) {
        if let Some(region)=&self.region{assert!(region.slept_before&&region.acquired&&region.slept_after,"region must sleep, acquire, and sleep again");assert!(self.ai_cancellations>0,"region target invalidation must exercise windup cancellation");}
    }

    /// Run authority at the current logical tick, then capture all three channels.
    /// Optional navigation is processed by finish_tick under its global grant.
    pub fn authority(&mut self){
        let tick=self.world.tick().0;self.events.clear();self.ends.clear();
        if let Some(region)=&self.region{
            if tick==20{self.world.set_position(region.target,GridPosition{x:1000,y:1360}).unwrap();}
            if tick==30{self.world.set_ai_target_eligible(region.target,false).unwrap();}
        }
        if tick%12==0{
            for i in 0..self.players.min(8){
                // Fixture targeting must continue exercising visible area hits
                // when count changes; a fixed row offset can move every target
                // behind the wall. Stable distance/id ordering, integer only.
                let origin=self.world.position(self.ids[i]).unwrap();
                let target=self.ids[self.players..self.players+self.actors].iter().copied()
                    .filter(|&id|self.world.collision_map().unwrap().segment_is_clear(origin,self.world.position(id).unwrap()))
                    .min_by_key(|&id|{let p=self.world.position(id).unwrap();
                        let dx=i64::from(p.x)-i64::from(origin.x);let dy=i64::from(p.y)-i64::from(origin.y);
                        (dx*dx+dy*dy,id.index)})
                    .expect("synthetic map must contain a visible skill target");
                self.world.start_cast(CastRequest{caster:self.ids[i],request_id:tick/12+1,
                    skill:SkillId(1),target:CastTarget::Entity(target)}).unwrap();
                let mut modifiers=BuffModifiers::default();modifiers.flat[0]=2;
                self.world.apply_buff(AttributeBuff{source:self.ids[i],target:self.ids[i],key:1,duration:s(6),modifiers}).unwrap();
                self.world.apply_periodic(PeriodicEffect{source:self.ids[i],target,key:2,
                    kind:PeriodicKind::Damage,amount:n(2),first_delay:1,interval:s(3),pulses:s(3),refresh:PeriodicRefresh::Replace}).unwrap();
                self.world.grant_shield(ShieldSpec{source:self.ids[i],target:self.ids[i],duration:Some(s(8)),profile:ShieldProfile::Absorb{capacity:n(5)}}).unwrap();
            }
        }
        if tick%20==0{
            for i in 0..self.players{
                let position=GridPosition{x:(i%20)as i32*24+8,y:if tick%40==0{80}else{160}};
                self.world.move_to(self.ids[i],position,s(320)).unwrap();
            }
        }
        self.world.resolve_attributes(&mut self.attributes);
        let shield=self.world.resolve_shields(MAX_SHIELD_WORK,&mut self.shields).unwrap();assert!(shield.overdue.is_none());
        let effects=self.world.resolve_effects(MAX_EFFECT_WORK,&mut self.effects,&mut self.events).unwrap();
        assert!(effects.overdue.is_none()&&effects.blocked.is_none());self.activity.pulses+=effects.pulses;
        self.world.resolve_attributes(&mut self.attributes);
        let skills=self.world.resolve_casts(MAX_ACTIVE_CASTS,&mut self.skills).unwrap();assert!(skills.overdue.is_none());
        for event in &self.skills{
            if let SkillEvent::Ready(ready)=event{
                self.activity.skills+=1;let caster=ready.request.caster;
                let CastTarget::Entity(target)=ready.request.target else{unreachable!()};
                let center=self.world.position(target).unwrap();let attributes=self.world.combat_attributes(caster).unwrap();
                let damage=n(physical_damage(attributes.attack,3,attributes.break_power,attributes.level,0,
                    ArmorCurve{base:n(100),per_level:10,armor_rate_bps:10000}).unwrap());
                let hit=self.actions.begin(&mut self.world,&mut CombatQueryBudget::new(1_000_000),&mut self.events,MAX_ACTIVE_COMBATANTS).unwrap()
                    .damage_area(AreaDamage{attacker:caster,area:CombatArea::Circle{center,radius:32},damage,require_sight:true},|_,id|id!=caster).unwrap();
                self.activity.area_hits+=hit.hits;
                let cast=self.projectiles.create_cast(&self.world,caster,s(1),8).unwrap();
                self.projectiles.launch(&self.world,ProjectileLaunch{cast,from:self.world.position(caster).unwrap(),to:center,
                    activate_at:self.world.tick(),travel_ticks:s(3),radius:8,max_hits:s(8),damage:n(2),strict_ends:false,require_sight:true}).unwrap();
                self.projectiles.seal_cast(cast).unwrap();
            }
        }
        // Active intent validation is authority, not a deferred perception grant.
        let ai=self.ai.resolve(&mut self.world,MAX_AI_WORK,&mut CombatQueryBudget::new(1_000_000),&mut self.decisions).unwrap();
        assert_eq!(ai.remaining,0);assert!(ai.deferred.is_none());self.activity.ai_visits+=ai.visited;
        // AI owns intent cancellation; the host forwards its feedback before
        // the following combat phase. A snapshot alone cannot replace this event.
        for decision in &self.decisions{if let Some(windup)=decision.cancelled_windup{
            self.events.push(CombatEvent{attacker:decision.actor,target:windup.target,kind:CombatEventKind::WindupCancelled{windup}});self.ai_cancellations+=1;
        }}
        if let Some(region)=&mut self.region{
            let idle=region.actors.iter().all(|&id|{let state=self.world.ai_state(id).unwrap();state.mode==AiMode::Idle&&state.target.is_none()});
            if tick==19{assert!(idle&&ai.sleeping>=8);region.slept_before=true;}
            if tick==21{assert!(region.actors.iter().all(|&id|self.world.ai_state(id).unwrap().target==Some(region.target)));region.acquired=true;}
            if tick>=32&&idle&&ai.sleeping>=8{region.slept_after=true;}
        }
        let basic=self.world.resolve_combat(&mut self.basic,&mut self.changes).unwrap();
        self.events.extend_from_slice(&self.basic[..basic.events_written]);self.activity.basic_hits+=basic.hits;
        let shots=self.projectiles.begin(&mut self.world).unwrap().step(MAX_PROJECTILES,&mut CombatQueryBudget::new(1_000_000),
            &mut self.events,MAX_ACTIVE_COMBATANTS,&mut self.ends,|_,hit|Some(hit.base_damage)).unwrap();
        assert_eq!(shots.pending,0);self.activity.projectile_hits+=shots.hits;
        if tick%40==10{for &id in &self.group{self.world.apply_damage(id,20).unwrap();}}
        let life=self.world.resolve_lifecycle(MAX_LIFECYCLE_WORK,&mut self.life).unwrap();
        assert!(life.overdue.is_none()&&life.blocked.is_none());self.activity.removals+=life.removed;
        for i in 0..self.players+self.actors{
            let id=self.ids[i];self.world.publish_attribute_component(id).unwrap();
            self.world.publish_effect_components(id,ComponentScope::Nearby).unwrap();
            if i<self.players{self.world.publish_skill_component(id,SkillId(1)).unwrap();}
        }
        if tick%10==0{for i in (0..self.ids.len()).step_by(32){self.world.set_component(self.ids[i],appearance(1+(tick/10%4)as u32)).unwrap();}}
        if tick%40==0{
            self.world.define_public_component(1,&[(tick/40)as u8]).unwrap();
            let mut item=GroundItemComponent{definition:2,count:n(2),position:GridPosition{x:1300,y:1000},height:0,
                source:self.ids[0],from_monster:true,exclusive_owner:Some(42),exclusive_until:Tick(tick+10),expires_at:Tick(tick+20)}.record();
            item.fields.set_reference(0,2).unwrap();self.world.set_component(self.drop,item).unwrap();
        }else if tick%40==20{self.world.remove_component(self.drop,ComponentKey{kind:ComponentKind::GroundItem,instance:0}).unwrap();}
        self.combat.capture(&self.world,&self.events).unwrap();self.components.capture(&self.world).unwrap();
    }

    pub fn spawn_due(&mut self, members:usize){
        let state=self.world.population_state();
        if members>=2&&state.alive==0&&state.next_due.is_some_and(|due|due<=self.world.tick()){
            self.world.spawn_group(1,None,self.template,&[GridPosition{x:1200,y:1000},GridPosition{x:1224,y:1000}],
                &mut SpawnBudget::new(members,1).unwrap(),&mut self.group).unwrap();self.activity.births+=2;
        }
    }
    pub fn finish_tick(&mut self,navigation:u32){
        self.world.restrict_navigation_work(navigation);self.world.advance_tick().unwrap();
        assert!(self.world.last_navigation_budget().used()<=navigation);
    }
    pub fn expected(&self,view:ViewRect)->Vec<CombatFeedback>{
        let contains=|p:GridPosition|p.x>=view.min.x&&p.x<=view.max.x&&p.y>=view.min.y&&p.y<=view.max.y;
        self.events.iter().filter(|e|self.world.position(e.attacker).ok().is_some_and(contains)
            &&self.world.position(e.target).ok().is_some_and(contains)).map(|e|CombatFeedback{tick:self.world.tick(),
            attacker:e.attacker,target:e.target,kind:match e.kind{
                CombatEventKind::WindupStarted{windup,..}=>CombatFeedbackKind::WindupStarted{started_at:windup.started_at,hit_at:windup.hit_at},
                CombatEventKind::WindupCancelled{windup}=>CombatFeedbackKind::WindupCancelled{started_at:windup.started_at},
                CombatEventKind::Hit{health,..}=>CombatFeedbackKind::Hit{damage:health.before.current-health.after.current,remaining:health.after.current},
                CombatEventKind::Ended(reason)=>CombatFeedbackKind::Ended(reason),
            }}).collect()
    }
}
