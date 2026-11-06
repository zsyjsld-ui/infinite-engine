//! A finite, synchronous demonstration, not a test or performance benchmark.
//! Run only when requested: cargo run -p infinite-core --example movement

use std::num::{NonZeroU16, NonZeroU32};

use infinite_core::{
    CollisionMap, DEFAULT_TICK_MS, GridPosition, MovementCommand,
    POSITION_UNITS_PER_PIXEL, World, WorldError,
};

// Small example coordinates in world pixels, converted once at the boundary.
fn at(x: i32, y: i32) -> GridPosition {
    let scale = POSITION_UNITS_PER_PIXEL as i32;
    GridPosition { x: x * scale, y: y * scale }
}

fn main() -> Result<(), String> {
    let mut map = CollisionMap::new(
        at(0, 0),
        NonZeroU32::new(20).unwrap(),
        NonZeroU32::new(12).unwrap(),
        NonZeroU32::new(16 * POSITION_UNITS_PER_PIXEL).unwrap(),
    ).map_err(|error| format!("map creation: {error:?}"))?;
    for y in 1..10 {
        map.set_blocked(7, y, true).map_err(|error| format!("wall placement: {error:?}"))?;
    }
    let mut world = World::with_map(DEFAULT_TICK_MS, map);
    let blocked = world.spawn(at(24, 40)).map_err(|error| format!("spawn: {error:?}"))?;
    let arriving = world.spawn(at(24, 184)).map_err(|error| format!("spawn: {error:?}"))?;
    let stopped = world.spawn(at(24, 8)).map_err(|error| format!("spawn: {error:?}"))?;

    // Application example speeds: running 200 and walking 140 world pixels/s.
    let run = NonZeroU16::new((200 * POSITION_UNITS_PER_PIXEL) as u16).unwrap();
    let walk = NonZeroU16::new((140 * POSITION_UNITS_PER_PIXEL) as u16).unwrap();
    let initial = [
        MovementCommand::MoveTo { entity: blocked, target: at(216, 40), speed: run },
        MovementCommand::MoveTo { entity: arriving, target: at(72, 184), speed: walk },
        MovementCommand::MoveTo { entity: stopped, target: at(280, 8), speed: run },
    ];
    let stop = [MovementCommand::Stop { entity: stopped }];
    let entities = [blocked, arriving, stopped];
    let labels = ["wall", "destination", "manual-stop"];
    let mut command_results = [Ok(()); 3];
    let mut states = [Err(WorldError::InvalidEntity); 3];

    println!("20 logical ticks; coordinates printed in 1/16 world-pixel units.");
    println!("The loop does not sleep and does not measure real-time performance.");
    for frame in 0..20 {
        let commands: &[MovementCommand] = match frame {
            0 => &initial,
            4 => &stop,
            _ => &[],
        };
        let report = world.step(commands, &mut command_results, &entities, &mut states)
            .map_err(|error| format!("step rejected: {error:?}"))?;
        println!("tick={} moving={} accepted={} rejected={}",
            report.tick.0, report.moving_entities, report.accepted_commands, report.rejected_commands);
        for (index, result) in command_results[..commands.len()].iter().enumerate() {
            if let Err(error) = result {
                println!("  command {index}: {error:?}");
            }
        }
        for (label, result) in labels.iter().zip(states.iter()) {
            match result {
                Ok(state) => println!("  {label}: ({}, {}) {:?}",
                    state.position.x, state.position.y, state.movement),
                Err(error) => println!("  {label}: {error:?}"),
            }
        }
    }
    Ok(())
}
