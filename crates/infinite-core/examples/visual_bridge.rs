//! Local-only visual adapter. External map bytes and artwork are not bundled.
//! Grid file: five little-endian u32s (width, height, cell units, spawn x/y),
//! then one occupancy byte per cell. Stdin commands -> one JSON state per line.
//! Optional second argument: staggered row height (cell units become column width).
use std::io::{self, BufRead, Write};
use std::num::{NonZeroU16, NonZeroU32};
use infinite_core::{CollisionMap, DEFAULT_TICK_MS, GridPosition, NavigationConfig, NavigationStatus, World};

fn main() -> Result<(), String> {
    let path = std::env::args().nth(1).ok_or("grid file required")?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < 20 { return Err("short grid header".into()); }
    let number = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let (width, height, size) = (number(0), number(4), number(8));
    if u64::from(width) * u64::from(height) != (bytes.len() - 20) as u64 {
        return Err("grid length mismatch".into());
    }
    let spawn = GridPosition { x: i32::try_from(number(12)).map_err(|e| e.to_string())?,
        y: i32::try_from(number(16)).map_err(|e| e.to_string())? };
    let nz = |n| NonZeroU32::new(n).ok_or("zero grid dimension");
    let row_height = std::env::args().nth(2).map(|v| v.parse::<u32>()).transpose().map_err(|e| e.to_string())?;
    let origin = GridPosition { x: 0, y: 0 };
    let mut map = match row_height {
        Some(row) => CollisionMap::new_staggered(origin, nz(width)?, nz(height)?, nz(size)?, nz(row)?),
        None => CollisionMap::new(origin, nz(width)?, nz(height)?, nz(size)?),
    }
        .map_err(|e| format!("{e:?}"))?;
    for y in 0..height {
        for x in 0..width {
            if bytes[20 + (y * width + x) as usize] != 0 {
                map.set_blocked(x, y, true).map_err(|e| format!("{e:?}"))?;
            }
        }
    }
    map.prepare_navigation(); // Load-time only; one connection byte per cell.
    // Use the normal bounded budget, not the deliberately small waiting demo budget.
    let config = NavigationConfig { queue_capacity: 4, ..NavigationConfig::default() };
    let mut world = World::with_map_and_navigation_config(DEFAULT_TICK_MS, map, config)
        .map_err(|e| format!("{e:?}"))?;
    let entity = world.spawn(spawn).map_err(|e| format!("{e:?}"))?;
    let speed = NonZeroU16::new(2240).unwrap(); // 140 world pixels / second.
    for line in io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        let words: Vec<_> = line.split_whitespace().collect();
        let mut error = String::new();
        let result = match words.as_slice() {
            ["tick"] => world.advance_tick().map(|_| ()),
            ["status"] => Ok(()),
            ["stop"] => world.stop_moving(entity),
            ["cancel"] => world.cancel_navigation(entity).map(|_| ()),
            ["reset"] => world.set_position(entity, spawn),
            ["position", x, y] => match (x.parse::<i32>(), y.parse::<i32>()) {
                (Ok(x), Ok(y)) => world.set_position(entity, GridPosition { x, y }),
                _ => { error = "InvalidInput".into(); Ok(()) }
            },
            ["nav", x, y] => match (x.parse::<i32>(), y.parse::<i32>()) {
                (Ok(x), Ok(y)) => world.queue_navigation(entity, GridPosition { x, y }, speed),
                _ => { error = "InvalidInput".into(); Ok(()) }
            },
            _ => { error = "InvalidInput".into(); Ok(()) }
        };
        if let Err(e) = result { error = format!("{e:?}"); }
        let state = world.entity_state(entity).map_err(|e| format!("{e:?}"))?;
        let (navigation, goal) = match state.navigation {
            NavigationStatus::Idle => ("Idle", state.target),
            NavigationStatus::Pending { target } => ("Pending", Some(target)),
            NavigationStatus::Ready { target } => ("Ready", Some(target)),
            NavigationStatus::Failed { target, error: e } => { error = format!("{e:?}"); ("Failed", Some(target)) },
            NavigationStatus::Cancelled { .. } => ("Cancelled", state.target),
        };
        let goal = goal.map_or("null".into(), |p| format!("[{},{}]", p.x, p.y));
        let mut cancellation = String::new();
        while let Some(event) = world.pop_navigation_cancellation() { cancellation = format!("{:?}", event.reason); }
        println!("{{\"tick\":{},\"x\":{},\"y\":{},\"movement\":\"{:?}\",\"navigation\":\"{}\",\"goal\":{},\"wait\":{},\"error\":\"{}\",\"cancellation\":\"{}\"}}",
            world.tick().0, state.position.x, state.position.y, state.movement, navigation, goal,
            state.navigation_wait.map_or(0, |w| w.waited_ticks), error, cancellation);
        io::stdout().flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}
