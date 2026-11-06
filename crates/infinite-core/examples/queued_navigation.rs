//! Minimal application adapter: commands -> World -> user-facing feedback.
//! Interactive terminal input with manual logical ticks; no real-time scheduler.
use std::io::{self, Write};
use std::num::{NonZeroU16, NonZeroU32};
use infinite_core::{
    CollisionMap, DEFAULT_TICK_MS, EntityId, EntityState, GridPosition, MIN_NAVIGATION_WORK_SLICE,
    MovementCommand, MovementStatus, NavigationCancelReason, NavigationConfig, NavigationPhase,
    NavigationStatus, PathError, Tick, World, WorldError,
};

const ACTORS: usize = 4;
const LABELS: [&str; ACTORS] = ["角色1", "角色2", "角色3", "角色4"];
const MAP_WIDTH: i32 = 100_000;
const MAP_HEIGHT: i32 = 5;
const MAX_TICKS_PER_COMMAND: u32 = 1000;
// Cell centers in integer simulation units. Each cell is 16 units, not 16 pixels.
fn cell(x: i32, y: i32) -> GridPosition { GridPosition { x: x * 16 + 8, y: y * 16 + 8 } }

struct NavigationApp {
    world: World,
    entities: [EntityId; ACTORS],
    previous: [Option<EntityState>; ACTORS],
    results: [Result<(), WorldError>; ACTORS],
    states: [Result<EntityState, WorldError>; ACTORS],
    dropped_cancellations: u64,
}

impl NavigationApp {
    fn new() -> Result<Self, String> {
        let mut map = CollisionMap::new(
            GridPosition { x: 0, y: 0 }, NonZeroU32::new(MAP_WIDTH as u32).unwrap(),
            NonZeroU32::new(MAP_HEIGHT as u32).unwrap(), NonZeroU32::new(16).unwrap(),
        ).map_err(|error| format!("创建地图失败：{error:?}"))?;
        // A small isolated component allows proving failure without searching
        // the whole long map. The requested destination itself is walkable.
        for y in 0..MAP_HEIGHT as u32 {
            map.set_blocked(10, y, true).map_err(|error| format!("设置阻挡失败：{error:?}"))?;
        }
        let config = NavigationConfig {
            work_per_tick: 8192, work_per_slice: MIN_NAVIGATION_WORK_SLICE,
            queued_work_reserve: MIN_NAVIGATION_WORK_SLICE, queue_capacity: ACTORS,
            max_wait_ticks: NonZeroU32::new(3),
        };
        let mut world = World::with_map_and_navigation_config(DEFAULT_TICK_MS, map, config)
            .map_err(|error| format!("导航配置无效：{error:?}"))?;
        let mut spawn = |position| world.spawn(position).map_err(|error| format!("创建角色失败：{error:?}"));
        let entities = [spawn(cell(12, 1))?, spawn(cell(13, 2))?, spawn(cell(1, 1))?, spawn(cell(14, 3))?];
        Ok(Self {
            world, entities, previous: [None; ACTORS], results: [Ok(()); ACTORS],
            states: [Err(WorldError::InvalidEntity); ACTORS], dropped_cancellations: 0,
        })
    }

    fn label(&self, entity: EntityId) -> &'static str {
        self.entities.iter().position(|&id| id == entity).map_or("已移除角色", |index| LABELS[index])
    }

    // Input changes intent only. Planning, movement and timeout advance only
    // through tick, so even a freshly accepted request can be cancelled first.
    fn submit(&mut self, command: MovementCommand) -> Result<(), String> {
        let cancellation_noop = match command {
            MovementCommand::CancelNavigation { entity } =>
                self.world.navigation_wait(entity).is_ok_and(|wait| wait.is_none()),
            _ => false,
        };
        let commands = [command];
        let tick = self.world.tick();
        self.world.apply_movement_commands(&commands, &mut self.results)
            .map_err(|error| format!("指令未提交：{error:?}"))?;
        self.world.read_entity_states(&self.entities, &mut self.states)
            .map_err(|error| format!("状态读取失败：{error:?}"))?;
        self.present_feedback(&commands, tick, tick);
        if cancellation_noop && self.results[0].is_ok() {
            println!("当前没有待规划请求，未取消任何路线；如需停止已有移动，请使用stop。");
        }
        Ok(())
    }

    fn advance(&mut self, count: u32) -> Result<(), String> {
        self.world.tick().0.checked_add(u64::from(count)).ok_or("逻辑时钟容量不足，未推进")?;
        for _ in 0..count {
            let report = self.world.step(&[], &mut self.results, &self.entities, &mut self.states)
                .map_err(|error| format!("应用帧未执行：{error:?}"))?;
            self.present_feedback(&[], report.tick, report.tick);
        }
        println!("已推进{count}个tick，当前tick {}；待规划请求{}个。",
            self.world.tick().0, self.world.pending_navigation_count());
        self.show_status();
        Ok(())
    }

    fn present_feedback(&mut self, commands: &[MovementCommand], command_tick: Tick, state_tick: Tick) {
        for (command, result) in commands.iter().zip(self.results.iter()) {
            let (entity, accepted) = match *command {
                MovementCommand::QueueNavigation { entity, .. } => (entity, "导航请求已受理"),
                MovementCommand::CancelNavigation { entity } => (entity, "取消指令已处理"),
                MovementCommand::Stop { entity } => (entity, "停止指令已处理"),
                MovementCommand::MoveTo { entity, .. } | MovementCommand::NavigateTo { entity, .. } =>
                    (entity, "移动目标已受理；尚不表示到达"),
            };
            let message = match result { Ok(()) => accepted, Err(error) => command_error(*error) };
            println!("[tick {}][{}] {message}", command_tick.0, self.label(entity));
        }
        // Cancellation records are the notification source: a snapshot may
        // already contain a replacement request, or the entity may be gone.
        while let Some(event) = self.world.pop_navigation_cancellation() {
            println!("[tick {}][{}] 待规划请求已取消：{}；目标位置单位({}, {})，已等待{} tick",
                event.at_tick.0, self.label(event.entity), cancel_reason(event.reason),
                event.target.x, event.target.y, event.waited_ticks);
        }
        let dropped = self.world.dropped_navigation_cancellations();
        let lost_records = dropped != self.dropped_cancellations;
        if lost_records {
            println!("部分取消记录未及时读取（累计丢失{dropped}条），以下以当前状态为准。");
            self.dropped_cancellations = dropped;
        }
        for (index, result) in self.states.iter().enumerate() {
            match result {
                Ok(state) => {
                    present_state(LABELS[index], state_tick, self.previous[index], *state, lost_records);
                    self.previous[index] = Some(*state);
                }
                Err(error) => {
                    println!("[tick {}][{}] 无法读取角色：{}", state_tick.0, LABELS[index], command_error(*error));
                    self.previous[index] = None;
                }
            }
        }
    }

    // Read-only refresh, not a replay of completion/cancellation notifications.
    fn show_status(&self) {
        println!("当前状态（tick {}，位置单位为1/16世界像素）：", self.world.tick().0);
        for (label, &entity) in LABELS.iter().zip(self.entities.iter()) {
            match self.world.entity_state(entity) {
                Ok(state) => {
                    let navigation = match state.navigation {
                        NavigationStatus::Idle => "无待处理导航",
                        NavigationStatus::Pending { .. } => "等待导航规划",
                        NavigationStatus::Ready { .. } => "导航已准备完成",
                        NavigationStatus::Failed { error, .. } => path_error(error),
                        NavigationStatus::Cancelled { reason, .. } => cancel_reason(reason),
                    };
                    println!("  {label}：位置单位({}, {})，所在格({}, {})；{}；{navigation}",
                        state.position.x, state.position.y, state.position.x / 16, state.position.y / 16,
                        movement_label(state.movement));
                    if let Some(target) = state.target {
                        println!("    已安装的移动目标格({}, {})", target.x / 16, target.y / 16);
                    }
                    if let NavigationStatus::Pending { target } = state.navigation {
                        println!("    待规划目标格({}, {})", target.x / 16, target.y / 16);
                    }
                    if let Some(wait) = state.navigation_wait {
                        println!("    {}；已等待{} tick，轮转位置{}，累计工作量{}，旧移动暂挂={}",
                            phase_label(wait.phase), wait.waited_ticks, wait.queue_position + 1,
                            wait.work_used, wait.movement_suspended);
                    }
                }
                Err(error) => println!("  {label}：{}", command_error(error)),
            }
        }
    }
}

fn present_state(label: &str, tick: Tick, previous: Option<EntityState>, state: EntityState, lost_records: bool) {
    let changed = previous.is_none_or(|old| old.navigation != state.navigation);
    match state.navigation {
        NavigationStatus::Pending { .. } => {
            if let Some(wait) = state.navigation_wait {
                let phase_changed = previous.and_then(|old| old.navigation_wait).is_none_or(|old| old.phase != wait.phase);
                let wait_advanced = previous.and_then(|old| old.navigation_wait)
                    .is_some_and(|old| old.waited_ticks != wait.waited_ticks);
                if changed || phase_changed || (wait_advanced && wait.waited_ticks % 2 == 0) {
                    let held = if wait.movement_suspended { "旧移动暂挂" } else { "角色等待规划" };
                    println!("[tick {}][{label}] {}；已等待{} tick，当前轮转位置{}，{held}",
                        tick.0, phase_label(wait.phase), wait.waited_ticks, wait.queue_position + 1);
                }
            }
        }
        NavigationStatus::Ready { .. } if changed =>
            println!("[tick {}][{label}] 导航准备完成；到达目标由移动状态另行确认", tick.0),
        NavigationStatus::Failed { error, .. } if changed =>
            println!("[tick {}][{label}] 导航失败：{}", tick.0, path_error(error)),
        NavigationStatus::Cancelled { reason, .. } if lost_records =>
            println!("[tick {}][{label}] 当前请求已取消：{}", tick.0, cancel_reason(reason)),
        _ => {} // Normal cancellations were already reported from the record buffer.
    }
    let resumed = previous.is_some_and(|old| matches!(old.navigation, NavigationStatus::Pending { .. }))
        && !matches!(state.navigation, NavigationStatus::Pending { .. });
    if !matches!(state.navigation, NavigationStatus::Pending { .. })
        && (previous.is_none_or(|old| old.movement != state.movement) || resumed)
    {
        println!("[tick {}][{label}] {}；位置单位({}, {})", tick.0,
            movement_label(state.movement), state.position.x, state.position.y);
    }
}

fn movement_label(movement: MovementStatus) -> &'static str {
    match movement {
        MovementStatus::Arrived => "已到达当前移动目标",
        MovementStatus::Moving => "已有移动路线",
        MovementStatus::Stopped => "移动已停止",
        MovementStatus::Blocked => "移动途中受阻，路线已停止",
        MovementStatus::Idle => "当前没有移动路线",
    }
}

fn phase_label(phase: NavigationPhase) -> &'static str {
    match phase {
        NavigationPhase::Queued => "等待轮转",
        NavigationPhase::CheckingDirect => "检查直达路线",
        NavigationPhase::Connecting => "连接起点和目标",
        NavigationPhase::Searching => "寻找绕障路线",
        NavigationPhase::Tracing => "整理搜索结果",
        NavigationPhase::BuildingPath => "准备移动路线",
    }
}

fn path_error(error: PathError) -> &'static str {
    match error {
        PathError::InvalidStart => "起点不合法",
        PathError::InvalidGoal => "目标不合法",
        PathError::Unreachable => "当前导航网格中没有可达路线",
        PathError::SearchLimit => "搜索达到资源上限，尚不能判断是否可达",
        PathError::PathTooLong => "路线超过路点上限",
        PathError::BudgetExhausted => "本帧可用规划预算不足，尚不能判断是否可达",
    }
}

fn cancel_reason(reason: NavigationCancelReason) -> &'static str {
    match reason {
        NavigationCancelReason::Explicit => "主动取消规划",
        NavigationCancelReason::Stopped => "收到停止指令",
        NavigationCancelReason::Replaced => "被新的移动意图替换",
        NavigationCancelReason::Relocated => "角色位置已重设",
        NavigationCancelReason::Despawned => "角色已移除",
        NavigationCancelReason::TimedOut => "等待超过设定期限",
    }
}

fn command_error(error: WorldError) -> &'static str {
    match error {
        WorldError::NavigationQueueFull => "导航队列已满，本次目标未受理",
        WorldError::BlockedPosition => "目标位置被阻挡或超出地图，本次目标未受理",
        WorldError::InvalidEntity => "角色不存在或已失效",
        WorldError::Navigation(error) => path_error(error),
        WorldError::EntityCapacityExhausted => "实体容量已满",
        WorldError::TickExhausted => "逻辑时钟已达到上限",
    }
}

enum Input {
    Command(MovementCommand),
    Tick(u32),
    Status,
    Help,
    Quit,
    Empty,
}

fn parse_input(line: &str, entities: &[EntityId; ACTORS]) -> Result<Input, String> {
    let words: Vec<_> = line.split_whitespace().collect();
    let actor = |word: &str| -> Result<EntityId, String> {
        word.parse::<usize>().ok().and_then(|id| id.checked_sub(1))
            .and_then(|index| entities.get(index)).copied()
            .ok_or_else(|| format!("角色编号须为1..{ACTORS}"))
    };
    match words.first().copied() {
        None => Ok(Input::Empty),
        Some("nav" | "move") if words.len() == 4 || words.len() == 5 => {
            let entity = actor(words[1])?;
            let x = words[2].parse::<i32>().map_err(|_| "目标格x必须是整数")?;
            let y = words[3].parse::<i32>().map_err(|_| "目标格y必须是整数")?;
            if !(0..MAP_WIDTH).contains(&x) || !(0..MAP_HEIGHT).contains(&y) {
                return Err(format!("目标格范围：x=0..{}，y=0..{}", MAP_WIDTH - 1, MAP_HEIGHT - 1));
            }
            let speed = words.get(4).copied().unwrap_or("160").parse::<u16>().ok()
                .and_then(NonZeroU16::new).ok_or("速度须为1..65535的整数")?;
            // Bounds above make this conversion safe; blocked cells are still
            // rejected by the core, preserving any existing request/movement.
            let target = cell(x, y);
            let command = if words[0] == "nav" {
                MovementCommand::QueueNavigation { entity, target, speed }
            } else {
                MovementCommand::MoveTo { entity, target, speed }
            };
            Ok(Input::Command(command))
        }
        Some("cancel") if words.len() == 2 =>
            Ok(Input::Command(MovementCommand::CancelNavigation { entity: actor(words[1])? })),
        Some("stop") if words.len() == 2 =>
            Ok(Input::Command(MovementCommand::Stop { entity: actor(words[1])? })),
        Some("tick") if words.len() == 1 || words.len() == 2 => {
            let count = words.get(1).copied().unwrap_or("1").parse::<u32>()
                .map_err(|_| "tick次数必须是正整数")?;
            if !(1..=MAX_TICKS_PER_COMMAND).contains(&count) {
                return Err(format!("每次可推进1..{MAX_TICKS_PER_COMMAND}个tick"));
            }
            Ok(Input::Tick(count))
        }
        Some("status") if words.len() == 1 => Ok(Input::Status),
        Some("help") if words.len() == 1 => Ok(Input::Help),
        Some("quit" | "exit") if words.len() == 1 => Ok(Input::Quit),
        _ => Err("未知命令或参数数量不正确；输入help查看用法".into()),
    }
}

fn show_help() {
    println!("命令：");
    println!("  nav 角色 x y [速度]   提交导航目标（格坐标，自动转换为格中心）");
    println!("  move 角色 x y [速度]  设置直线移动，途中遇墙会停止");
    println!("  cancel 角色           仅取消待规划请求，旧移动可恢复；已装入路线请用stop");
    println!("  stop 角色             取消待规划请求并停止移动");
    println!("  tick [次数]           手动推进1..{MAX_TICKS_PER_COMMAND}个逻辑tick，默认1");
    println!("  status                只读查看全部角色和等待详情");
    println!("  help / quit           查看帮助 / 退出（也支持exit或输入结束）");
    println!("角色编号1..4；目标格x=0..99999，y=0..4；x=10整列为墙，角色3位于墙左侧。");
    println!("每格16位置单位=1世界像素。速度为位置单位/秒，默认160；每tick=50ms。");
    println!("队列最多4项，总预算8192/每轮4101，保留额度4101，等待上限3 tick。");
    println!("输入命令或思考不会推进时间；只有tick推进。所有角色初始静止。");
    println!("可手动输入：nav 2 23 2 → tick 20（准备与到达分开提示）");
    println!("取消旧目标示例：move 1 50 1 → nav 1 99999 1 → tick → cancel 1 → tick");
    println!("停止示例：stop 1；不可达示例：nav 3 12 1 → tick 3");
    println!("超时示例：nav 4 99999 3 → tick 4。以上箭头表示分行输入，不是命令语法。");
}

fn main() -> Result<(), String> {
    let mut app = NavigationApp::new()?;
    println!("最小交互导航应用：命令行输入，手动逻辑时钟，无后台计时线程。");
    show_help();
    app.show_status();
    let stdin = io::stdin();
    let mut line = String::new();
    loop {
        print!("导航[tick {}]> ", app.world.tick().0);
        io::stdout().flush().map_err(|error| format!("提示输出失败：{error}"))?;
        line.clear();
        if stdin.read_line(&mut line).map_err(|error| format!("输入读取失败：{error}"))? == 0 {
            println!("输入结束，退出示例。");
            break;
        }
        match parse_input(&line, &app.entities) {
            Ok(Input::Command(command)) => app.submit(command)?,
            Ok(Input::Tick(count)) => app.advance(count)?,
            Ok(Input::Status) => app.show_status(),
            Ok(Input::Help) => show_help(),
            Ok(Input::Quit) => { println!("退出示例。"); break; }
            Ok(Input::Empty) => {}
            Err(message) => println!("输入未执行：{message}。"),
        }
    }
    Ok(())
}
