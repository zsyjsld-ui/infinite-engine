use infinite_core::*;
use std::num::NonZeroU32;

fn main() {
    let mut world = World::default();
    let actor = world.spawn(GridPosition{x:0,y:0}).unwrap();
    world.configure_health(actor,100,NonZeroU32::new(100).unwrap()).unwrap();
    let initial = world.health(actor).unwrap();
    let commands = [
        HealthCommand::Damage{entity:actor,amount:20},
        HealthCommand::Damage{entity:actor,amount:30},
        HealthCommand::Heal{entity:actor,amount:10},
        HealthCommand::Damage{entity:actor,amount:u32::MAX},
        HealthCommand::Heal{entity:actor,amount:50}, // Cannot revive a depleted entity.
        HealthCommand::Revive{entity:actor,current:NonZeroU32::new(1).unwrap()},
    ];
    let mut results = [Err(HealthError::Unconfigured);6];
    let mut changes = [HealthChange{entity:actor,before:initial,after:initial};6];
    let report = world.apply_health_commands(&commands,&mut results,&mut changes).unwrap();
    for (command,result) in commands.iter().zip(&results[..report.processed]) {
        match result {
            Ok(outcome) => println!("{command:?}: {} -> {}, depleted={}, revived={}",
                outcome.before.current,outcome.after.current,outcome.depleted(),outcome.revived()),
            Err(error) => println!("{command:?}: rejected {error:?}"),
        }
    }
    println!("tick={}, accepted={}, rejected={}, final_changes={}",report.tick.0,
        report.accepted,report.rejected,report.changed_entities);
    for change in &changes[..report.changed_entities] { println!("final: {change:?}"); }
    // Consuming ordered results is where the application would settle kills,
    // cancel AI/movement, or emit events. The final change list is state only.
}
