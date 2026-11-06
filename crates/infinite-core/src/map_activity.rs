//! Conservative simulation dormancy. Network/IO/application work is explicit.
use crate::{Tick,World};
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct MapWork {
    /// Commands, undelivered feedback, external simulation jobs, etc.
    pub pending:bool,
    pub next_due:Option<Tick>,
    /// Trusted host scheduling for species with zero weight or placement backoff.
    /// Only defers population attempts, never combat/effects/lifecycle deadlines.
    pub spawn_not_before:Option<Tick>,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct MapActivity {pub continuous:bool,pub pending:bool,pub next_due:Option<Tick>}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum MapSleepError {Past,Continuous,Pending,Deadline{due:Tick}}
impl World {
    pub fn map_activity(&self,app:MapWork)->MapActivity{
        let (active_ai,pending_ai)=self.ai.activity();let life=self.lifecycle_activity();let population=self.population_state();
        let continuous=self.moving_count()>0||self.pending_navigation_count()>0||self.active_combatants()>0||active_ai;
        let pending=app.pending||pending_ai||self.pending_attribute_updates()>0||life.awaiting_placement>0||life.unscheduled>0||population.blocked>0||self.has_navigation_feedback();
        let population_due=population.next_due.map(|due|due.max(app.spawn_not_before.unwrap_or(due)));
        let mut next_due=[self.skills.next_due(),self.effects.next_due(),self.shields.next_due(),life.next_due,population_due,app.next_due].into_iter().flatten().min();
        if pending{next_due=Some(next_due.map_or(self.tick(),|at|at.min(self.tick())));}
        MapActivity{continuous,pending,next_due}
    }
    /// Advance an idle simulation directly to (not past) its earliest deadline.
    /// Rechecks live state, so a previously cached sleep decision grants no right
    /// to skip new motion, births, dirty attributes, target changes or commands.
    /// The host resolves due phases at the returned tick before stepping again.
    pub fn skip_idle_to(&mut self,target:Tick,app:MapWork)->Result<MapActivity,MapSleepError>{
        if target<self.tick(){return Err(MapSleepError::Past);}let activity=self.map_activity(app);
        if target==self.tick(){return Ok(activity);}
        if activity.continuous{return Err(MapSleepError::Continuous);}if activity.pending{return Err(MapSleepError::Pending);}
        if let Some(due)=activity.next_due{if due<target{return Err(MapSleepError::Deadline{due});}}
        self.jump_idle_tick(target);Ok(self.map_activity(app))
    }
}
