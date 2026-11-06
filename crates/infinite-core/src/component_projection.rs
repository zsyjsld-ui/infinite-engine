//! Concrete presentation schemas. Definitions/assets remain application-owned.
//! Publish after simulation phases and before the component capture.
use crate::*;
fn low(t: Tick) -> u32 { t.0 as u32 }
fn high(t: Tick) -> u32 { (t.0 >> 32) as u32 }
fn record(kind: ComponentKind, instance: u32, scope: ComponentScope, words: &[u32]) -> ComponentRecord {
    ComponentRecord { key: ComponentKey { kind, instance }, scope,
        fields: ComponentFields::from_words(words).expect("bounded built-in schema") }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppearanceComponent { pub model: u32, pub weapon: u32, pub armor: u32, pub palette: u16, pub style: u16 }
impl AppearanceComponent {
    pub fn record(self) -> ComponentRecord {
        record(ComponentKind::Appearance, 0, ComponentScope::Nearby,
            &[self.model, self.weapon, self.armor, u32::from(self.palette), u32::from(self.style)])
    }
}
/// State of an already committed ground object, not a loot-roll/pickup command.
/// Map scope preserves the legacy same-map distribution by default. Position is
/// included because a map-visible drop can be outside the motion view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroundItemComponent {
    pub definition: u32, pub count: std::num::NonZeroU32, pub position: GridPosition, pub height: i32,
    /// Stable application participant ID, independent of entity slot reuse.
    pub source: EntityId, pub from_monster: bool, pub exclusive_owner: Option<u64>,
    pub exclusive_until: Tick, pub expires_at: Tick,
}
impl GroundItemComponent {
    pub fn record(self) -> ComponentRecord {
        let owner = self.exclusive_owner.unwrap_or(0);
        record(ComponentKind::GroundItem, 0, ComponentScope::Map, &[self.definition, self.count.get(),
            self.position.x as u32, self.position.y as u32, self.height as u32, u32::from(self.from_monster),
            self.source.index, self.source.generation, u32::from(self.exclusive_owner.is_some()), owner as u32,
            (owner>>32)as u32, low(self.exclusive_until), high(self.exclusive_until), low(self.expires_at), high(self.expires_at)])
    }
}
/// Matches the current application's five-member ownership snapshot. Omitted
/// and explicitly empty lists remain distinct. This never grants pickup rights.
pub const MAX_GROUND_OWNER_MEMBERS:usize=5;
pub fn ground_members_component(members:Option<&[u64]>)->Result<ComponentRecord,ComponentError>{
    let mut words=vec![u32::from(members.is_some())];
    if let Some(members)=members{
        if members.len()>MAX_GROUND_OWNER_MEMBERS{return Err(ComponentError::EntityLimit);}
        for (index,&id) in members.iter().enumerate(){if members[..index].contains(&id){return Err(ComponentError::DuplicateKey);}words.extend([id as u32,(id>>32)as u32]);}
    }
    Ok(record(ComponentKind::GroundMembers,0,ComponentScope::Map,&words))
}
impl World {
    /// No countdown rewrite: absolute tick deadlines remain stable while idle.
    pub fn publish_skill_component(&mut self, entity: EntityId, skill: SkillId) -> Result<bool, ComponentError> {
        let state = self.skill_state(entity, skill).map_err(ComponentError::Skill)?;
        let mut words = vec![u32::from(skill.0), low(state.cooldown_ready), high(state.cooldown_ready),
            low(state.recovery_ready), high(state.recovery_ready), u32::from(state.pending.is_some())];
        if let Some(cast) = state.pending {
            words.extend([cast.request.request_id as u32, (cast.request.request_id >> 32) as u32,
                low(cast.started_at), high(cast.started_at), low(cast.ready_at), high(cast.ready_at)]);
            match cast.request.target {
                CastTarget::Entity(target) => words.extend([1, target.index, target.generation, 0]),
                CastTarget::Point(point) => words.extend([2, point.x as u32, point.y as u32, 0]),
            }
        }
        self.set_component(entity, record(ComponentKind::Skill, u32::from(skill.0), ComponentScope::Owner, &words))
    }
    /// A projection of resolved attributes; Dirty is propagated rather than
    /// silently publishing a prior derived value. Does not re-resolve attributes.
    pub fn publish_attribute_component(&mut self, entity: EntityId) -> Result<bool, ComponentError> {
        let a = self.combat_attributes(entity).map_err(ComponentError::Attribute)?;
        self.set_component(entity, record(ComponentKind::Attributes, 0, ComponentScope::Owner,
            &[a.attack, a.break_power, a.defense, a.maximum_health.get(), a.maximum_resource, u32::from(a.level.get())]))
    }
    /// Replace the target's entire effect projection from real active effects.
    /// Nearby projections omit periodic amounts/remaining pulses; the app must
    /// explicitly choose public effects. Default callers should use Owner.
    pub fn publish_effect_components(&mut self, entity: EntityId, scope: ComponentScope) -> Result<bool, ComponentError> {
        if scope == ComponentScope::Map { return Err(ComponentError::InvalidScope); }
        let mut ids = Vec::new(); self.entity_effects(entity, &mut ids).map_err(ComponentError::Effect)?;
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            let state = self.effect_state(id).map_err(ComponentError::Effect)?;
            let kind = match state.kind {
                EffectStateKind::Buff(_) => 0,
                EffectStateKind::Periodic { kind, .. } => match kind {
                    PeriodicKind::Damage => 1, PeriodicKind::Heal => 2,
                    PeriodicKind::RestoreResource => 3, PeriodicKind::DrainResource => 4,
                },
            };
            let mut words = vec![u32::from(state.key), id.0.generation, state.source.index, state.source.generation,
                low(state.expires_at), high(state.expires_at), kind];
            if scope == ComponentScope::Owner {
                if let EffectStateKind::Periodic { amount, interval, remaining, .. } = state.kind {
                    words.extend([amount.get(), u32::from(interval.get()), u32::from(remaining), low(state.next_due), high(state.next_due)]);
                }
            }
            records.push(record(ComponentKind::Effect, id.0.index, scope, &words));
        }
        self.replace_component_kind(entity, ComponentKind::Effect, &records)
    }
}
