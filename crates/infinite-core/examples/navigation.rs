//! Finite navigation demonstration source; not a test or timing benchmark.
//! Run only when requested: cargo run -p infinite-core --example navigation
use std::num::{NonZeroU16, NonZeroU32};
use infinite_core::{
    CollisionMap, DEFAULT_TICK_MS, GridPosition, MovementCommand, MovementStatus,
    POSITION_UNITS_PER_PIXEL, World, WorldError,
};

fn at(x: i32, y: i32) -> GridPosition {
    GridPosition { x: x * POSITION_UNITS_PER_PIXEL as i32, y: y * POSITION_UNITS_PER_PIXEL as i32 }
}

fn main() -> Result<(), String> {
    let mut map = CollisionMap::new(
        at(0, 0), NonZeroU32::new(30).unwrap(), NonZeroU32::new(20).unwrap(),
        NonZeroU32::new(16 * POSITION_UNITS_PER_PIXEL).unwrap(),
    ).map_err(|error| format!("map: {error:?}"))?;
    // A wall with a passage at its lower end.
    for y in 0..18 {
        map.set_blocked(12, y, true).map_err(|error| format!("wall: {error:?}"))?;
    }
    // A walkable destination completely enclosed by eight blocked neighbors.
    for y in 13..=15 {
        for x in 23..=25 {
            if (x, y) != (24, 14) {
                map.set_blocked(x, y, true).map_err(|error| format!("enclosure: {error:?}"))?;
            }
        }
    }
    let mut world = World::with_map(DEFAULT_TICK_MS, map);
    let routed = world.spawn(at(24, 40)).map_err(|error| format!("spawn: {error:?}"))?;
    let direct = world.spawn(at(24, 56)).map_err(|error| format!("spawn: {error:?}"))?;
    let isolated = world.spawn(at(24, 88)).map_err(|error| format!("spawn: {error:?}"))?;
    let speed = NonZeroU16::new((200 * POSITION_UNITS_PER_PIXEL) as u16).unwrap();
    let initial = [
        MovementCommand::NavigateTo { entity: routed, target: at(360, 40), speed },
        MovementCommand::MoveTo { entity: direct, target: at(360, 56), speed },
        MovementCommand::NavigateTo { entity: isolated, target: at(392, 232), speed },
    ];
    let entities = [routed, direct, isolated];
    let labels = ["routed", "straight", "unreachable-request"];
    let mut previous = [MovementStatus::Idle; 3];
    let mut results = [Ok(()); 3];
    let mut states = [Err(WorldError::InvalidEntity); 3];
    println!("120 logical ticks, no sleep or performance measurement; positions in 1/16 pixels.");
    for frame in 0..120 {
        let commands: &[MovementCommand] = if frame == 0 { &initial } else { &[] };
        let report = world.step(commands, &mut results, &entities, &mut states)
            .map_err(|error| format!("step: {error:?}"))?;
        if !commands.is_empty() {
            println!("navigation work={}/{}, budget rejections={}",
                report.navigation_budget.used(), report.navigation_budget.limit(),
                report.navigation_budget_rejections);
        }
        for (index, result) in results[..commands.len()].iter().enumerate() {
            println!("command {index}: {result:?}");
        }
        for (index, result) in states.iter().enumerate() {
            match result {
                Ok(state) => {
                    if frame % 20 == 0 || previous[index] != state.movement || frame == 119 {
                        println!("tick={} {} ({}, {}) {:?}", report.tick.0, labels[index],
                            state.position.x, state.position.y, state.movement);
                    }
                    previous[index] = state.movement;
                }
                Err(error) => println!("{}: {error:?}", labels[index]),
            }
        }
    }
    Ok(())
}
