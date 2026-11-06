//! Small API example; release build and output checked on 2026-11-04.
use infinite_core::*;
use std::num::NonZeroU32;

fn n(value: u32) -> NonZeroU32 { NonZeroU32::new(value).unwrap() }
fn p(x: i32, y: i32) -> GridPosition { GridPosition { x, y } }

fn main() {
    let mut map = CollisionMap::new(p(0, 0), n(8), n(8), n(16)).unwrap();
    map.set_blocked(3, 3, true).unwrap();
    let mut world = World::with_map(DEFAULT_TICK_MS, map);
    let caster = world.spawn(p(24, 56)).unwrap();
    let near = world.spawn(p(40, 56)).unwrap();
    let behind_wall = world.spawn(p(88, 56)).unwrap();
    let side = world.spawn(p(24, 88)).unwrap();
    for entity in [caster, near, behind_wall, side] {
        world.configure_health(entity, 100, n(100)).unwrap();
    }

    let mut frame = SpatialFrame::new(n(32));
    frame.capture(&world).unwrap();
    let context = CombatQueryContext::new(&frame, world.collision_map());
    let area = CombatQuery {
        area: CombatArea::Circle { center: p(24, 56), radius: 80 },
        exclude: Some(caster), sight_origin: None,
    };
    let visible_area = CombatQuery { sight_origin: Some(p(24, 56)), ..area };
    let rectangle = CombatQuery {
        area: CombatArea::Rect(ViewRect { min: p(16, 48), max: p(96, 96) }),
        ..visible_area
    };
    // Reuse these buffers and share one budget across a tick's queries.
    let mut hits = Vec::with_capacity(16);
    let mut reports = [Err(CombatQueryError::Spatial(SyncError::NoFrame)); 3];
    let mut budget = CombatQueryBudget::new(4096);
    context.collect_batch(&[area, visible_area, rectangle], &mut budget,
        &mut hits, &mut reports, |_, hit| {
            world.health(hit.entity).is_ok_and(|health| health.current > 0)
        }).unwrap();
    for (index, result) in reports.iter().enumerate() {
        match result {
            Ok(report) => println!("area {index}: {:?}, work={:?}",
                &hits[report.offset..report.offset+report.count], report.work),
            Err(error) => println!("area {index}: incomplete: {error:?}"),
        }
    }
    let mut targets = [Err(CombatQueryError::Spatial(SyncError::NoFrame)); 2];
    context.nearest_batch(&[
        CombatNearestQuery { query: visible_area, origin: p(24, 56) },
        CombatNearestQuery { query: visible_area, origin: p(24, 88) },
    ], &mut budget, &mut targets, |_, hit| {
        world.health(hit.entity).is_ok_and(|health| health.current > 0)
    }).unwrap();
    println!("nearest: {targets:?}");
    println!("sight to target behind wall: {:?}",
        context.line_of_sight(p(24, 56), world.position(behind_wall).unwrap(), &mut budget));
    println!("total work={:?}, remaining={}", budget.work(), budget.remaining());
}
