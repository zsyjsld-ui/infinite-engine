//! Incremental idle-actor index. Each sleeper occupies one radius-tier cell.
//! A changed target visits nine cells per occupied tier, then exact circles.
use std::collections::HashMap;
use super::*;

type Cell = (u8, i64, i64);
#[derive(Clone, Copy)]
pub(super) struct SleepSlot { cell: Cell, offset: usize, radius: u32 }
struct Bucket { actors: Vec<usize>, revision: u64 }
#[derive(Clone, Copy)]
struct WakeCursor {
    target: usize, revision: u64, shift: u8, neighbor: u8,
    offset: usize, bucket_revision: Option<u64>,
}
#[derive(Default)]
pub(super) struct Activation {
    pub(super) active: Vec<usize>,
    buckets: HashMap<Cell, Bucket>, levels: [u32; 25], serial: u64,
    occupied: u32,
    // Conservative cell bounds may retain removed extremes until a tier empties.
    // No scan on removal, no false-negative wake, constant per-world storage.
    bounds: [[i32; 4]; 25],
    pub(super) sleeping: usize,
    pub(super) dirty: Vec<usize>, next_dirty: usize, pending: Option<WakeCursor>,
}
impl AiColumns {
    pub(super) fn remove_active(&mut self, index: usize) {
        if let Some(slot) = self.actors[index].active.take() {
            let slot = (slot.get()-1) as usize;
            self.activation.active.swap_remove(slot);
            if let Some(&moved) = self.activation.active.get(slot) {
                self.actors[moved].active = NonZeroU32::new((slot+1) as u32);
            }
        }
    }
    pub(super) fn remove_sleep(&mut self, index: usize) {
        let Some(slot) = self.actors[index].sleeper.take() else { return; };
        self.activation.serial = self.activation.serial.wrapping_add(1);
        let bucket = self.activation.buckets.get_mut(&slot.cell).expect("indexed sleeper");
        bucket.actors.swap_remove(slot.offset);
        bucket.revision = self.activation.serial;
        if let Some(&moved) = bucket.actors.get(slot.offset) {
            self.actors[moved].sleeper.as_mut().unwrap().offset = slot.offset;
        }
        if bucket.actors.is_empty() { self.activation.buckets.remove(&slot.cell); }
        self.activation.levels[usize::from(slot.cell.0-8)] -= 1;
        if self.activation.levels[usize::from(slot.cell.0-8)]==0 { self.activation.occupied &= !(1 << (slot.cell.0-8)); }
        self.activation.sleeping -= 1;
    }
    pub(super) fn wake_actor(&mut self, index: usize) {
        if self.actors[index].active.is_some() {
            self.actors[index].last_tick=None;self.remaining=self.activation.active.len();return;
        }
        self.remove_sleep(index);
        let actor = &mut self.actors[index];
        if actor.active.is_none() {
            self.activation.active.push(index);
            actor.active = NonZeroU32::new(self.activation.active.len() as u32);
        }
        actor.last_tick = None;
        self.remaining = self.activation.active.len();
    }
    /// Only a completed ordinary idle search may sleep. Transitions to Idle
    /// (e.g. leash or lost attack target) get the old next-tick acquisition first.
    pub(super) fn park_actor(&mut self, index: usize, now: Tick, can_sleep: bool) -> bool {
        let actor = &self.actors[index];
        if actor.state.mode == AiMode::Dead { self.remove_active(index); return true; }
        if !can_sleep || actor.state.mode != AiMode::Idle || actor.state.target.is_some()
            || actor.state.controlled || actor.damage_wake { return false; }
        let mut radius = actor.profile.aggro_radius;
        if actor.threats.iter().any(|e| e.bonus > 0 && e.bonus_until > now) {
            radius = radius.max(actor.profile.bonus_radius);
        }
        let shift = (32-radius.saturating_sub(1).leading_zeros()).max(8) as u8;
        let size = 1i64 << shift;
        let cell = (shift, i64::from(actor.position.x).div_euclid(size), i64::from(actor.position.y).div_euclid(size));
        self.remove_active(index);
        self.activation.serial = self.activation.serial.wrapping_add(1);
        let bucket = self.activation.buckets.entry(cell).or_insert_with(|| Bucket { actors: Vec::new(), revision: 0 });
        self.actors[index].sleeper = Some(SleepSlot { cell, offset: bucket.actors.len(), radius });
        bucket.actors.push(index); bucket.revision = self.activation.serial;
        let level=usize::from(shift-8);let x=cell.1 as i32;let y=cell.2 as i32;
        let bounds=&mut self.activation.bounds[level];
        if self.activation.levels[level]==0 {*bounds=[x,y,x,y];}
        else {bounds[0]=bounds[0].min(x);bounds[1]=bounds[1].min(y);bounds[2]=bounds[2].max(x);bounds[3]=bounds[3].max(y);}
        self.activation.levels[level] += 1; self.activation.sleeping += 1;self.activation.occupied |= 1 << level;
        true
    }
    pub(super) fn remap_actor(&mut self, index: usize) {
        if let Some(actor) = self.actors.get(index) {
            if let Some(slot) = actor.active { self.activation.active[(slot.get()-1) as usize] = index; }
            if let Some(slot) = actor.sleeper { self.activation.buckets.get_mut(&slot.cell).unwrap().actors[slot.offset] = index; }
        }
    }
    pub(super) fn mark_target_dirty(&mut self, index: usize) {
        let target = &mut self.targets[index];
        target.revision = target.revision.wrapping_add(1);
        self.target_revision = self.target_revision.wrapping_add(1);
        // With no sleepers every actor already belongs to the normal phase.
        // Its fresh capture sees this change, so no wake job is necessary.
        if target.dirty.is_none() && self.activation.sleeping != 0 {
            self.activation.dirty.push(index);
            target.dirty = NonZeroU16::new(self.activation.dirty.len() as u16);
        }
    }
    pub(super) fn remove_dirty(&mut self, index: usize) {
        if self.activation.pending.is_some_and(|p| p.target == index) { self.activation.pending = None; }
        if let Some(slot) = self.targets[index].dirty.take() {
            let slot = usize::from(slot.get()-1);
            self.activation.dirty.swap_remove(slot);
            if let Some(&moved) = self.activation.dirty.get(slot) {
                self.targets[moved].dirty = NonZeroU16::new((slot+1) as u16);
            }
            self.activation.next_dirty = slot;
        }
    }
    pub(super) fn remap_target(&mut self, index: usize) {
        if let Some(target) = self.targets.get(index) {
            if let Some(slot) = target.dirty { self.activation.dirty[usize::from(slot.get()-1)] = index; }
            if let Some(pending) = &mut self.activation.pending {
                if pending.target == self.targets.len() { pending.target = index; }
            }
        }
    }
    pub(crate) fn position_changed(&mut self, row: usize, position: GridPosition) {
        if let Some(index) = self.actor_rows[row] {
            let index = (index.get()-1) as usize;
            if self.actors[index].position != position {
                self.actors[index].position = position; self.wake_actor(index);
            }
        }
        if let Some(index) = self.target_rows[row] {
            let index = usize::from(index.get()-1);
            if self.targets[index].position != position {
                self.targets[index].position = position; self.mark_target_dirty(index);
            }
        }
    }
    pub(super) fn wake_targets(&mut self, health: &crate::health::HealthColumns, limit: usize,
        budget: &mut CombatQueryBudget, report: &mut AiReport) {
        while report.wake_jobs < limit && !self.activation.dirty.is_empty() {
            let mut cursor = self.activation.pending.unwrap_or_else(|| {
                self.activation.next_dirty %= self.activation.dirty.len();
                let target = self.activation.dirty[self.activation.next_dirty];
                WakeCursor { target, revision: self.targets[target].revision, shift: 8, neighbor: 0, offset: 0, bucket_revision: None }
            });
            let target = self.targets[cursor.target];
            if cursor.revision != target.revision {
                cursor = WakeCursor { target: cursor.target, revision: target.revision, shift: 8, neighbor: 0, offset: 0, bucket_revision: None };
            }
            if !target.eligible || !health.read(target.row).is_ok_and(|h| !h.is_depleted()) { cursor.shift = 33; }
            while cursor.shift <= 32 {
                let occupied=self.activation.occupied & (u32::MAX << (cursor.shift-8));
                if occupied==0 {cursor.shift=33;break;}
                let next=occupied.trailing_zeros() as u8+8;
                if next!=cursor.shift {cursor.shift=next;cursor.neighbor=0;cursor.offset=0;cursor.bucket_revision=None;}
                if cursor.neighbor == 9 {
                    cursor.shift += 1; cursor.neighbor = 0; cursor.offset = 0; cursor.bucket_revision = None; continue;
                }
                let size = 1i64 << cursor.shift;
                let x=i64::from(target.position.x).div_euclid(size);let y=i64::from(target.position.y).div_euclid(size);
                let bounds=self.activation.bounds[usize::from(cursor.shift-8)];
                if x+1<i64::from(bounds[0])||y+1<i64::from(bounds[1])||x-1>i64::from(bounds[2])||y-1>i64::from(bounds[3]){
                    cursor.shift+=1;cursor.neighbor=0;cursor.offset=0;cursor.bucket_revision=None;continue;
                }
                let cell = (cursor.shift, x+i64::from(cursor.neighbor%3)-1,y+i64::from(cursor.neighbor/3)-1);
                let revision = self.activation.buckets.get(&cell).map_or(0, |b| b.revision);
                if cursor.bucket_revision != Some(revision) {
                    if let Err(error) = budget.cell() { report.deferred = Some(error); break; }
                    cursor.bucket_revision = Some(revision); cursor.offset = 0;
                }
                let candidate = self.activation.buckets.get(&cell).and_then(|b| b.actors.get(cursor.offset)).copied();
                let Some(index) = candidate else {
                    cursor.neighbor += 1; cursor.offset = 0; cursor.bucket_revision = None; continue;
                };
                if let Err(error) = budget.candidate() { report.deferred = Some(error); break; }
                let actor = &self.actors[index];
                if actor.entity != target.entity && within_attack_range(actor.position, target.position, actor.sleeper.unwrap().radius) {
                    self.wake_actor(index); report.woken += 1;
                    // Our own swap-removal leaves an unvisited tail in this slot.
                    cursor.bucket_revision = Some(self.activation.buckets.get(&cell).map_or(0, |b| b.revision));
                } else { cursor.offset += 1; }
            }
            if report.deferred.is_some() { self.activation.pending = Some(cursor); break; }
            self.remove_dirty(cursor.target); report.wake_jobs += 1;
        }
    }
}
