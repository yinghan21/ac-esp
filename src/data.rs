//! 【本文件职责】本文件实现：**数据线程**（读内存 → 快照）、实体扫描、自检信息。
//!
//! 数据线程：后台不断读游戏内存，把「可以直接拿去画的数据」塞进共享快照。
//!
//! # 为什么要单独开一个线程
//! 1. 渲染回调跑在游戏的渲染线程（wglSwapBuffers）上，
//!    在里面读内存 + 算一堆东西会直接拖慢游戏帧率；
//! 2. 数据更新频率和帧率解耦：游戏掉帧时 ESP 也不会跟着卡住。
//!
//! # 数据怎么流动
//! ```text
//!   数据线程（每 2ms）                 渲染线程（每帧）
//!   read_into(&mut snapshot)   ──写──►  Arc<RwLock<Snapshot>>  ──读──►  esp::draw()
//! ```
//!
//! # 找不到敌人时，问题一定出在本文件的三个环节之一
//! ```text
//!   1. 数量对不对？    PLAYER_COUNT 偏移
//!   2. 数组在哪？      实体列表布局（内联数组 vs 指针/vector）★ 最常见
//!   3. 实体是不是人？  坐标偏移 + 合理性校验
//! ```
//! 所以本文件会把每一环的原始读数记进 `DebugInfo`，菜单「自检」区直接能看到。

use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::config::EntityLayout;
use crate::memory::{self, Vec3};
use crate::offset;

/// 读取频率：2ms ≈ 500Hz，比任何显示器刷新率都高，
/// 而每次只读 32 个实体，CPU 占用可以忽略。
const READ_INTERVAL: Duration = Duration::from_millis(2);

/// 自动检测布局时最多抽样几个槽位（够判断了，不用扫完）
const DETECT_SAMPLE: usize = 8;

/// 自检面板里最多保留几个实体样本
const MAX_SAMPLES: usize = 5;

/// 菜单里手动指定的布局。0 = 自动，1 = 内联数组，2 = 指针数组
static FORCED_LAYOUT: AtomicU8 = AtomicU8::new(0);

/// 【调参用】菜单里临时指定的状态字段偏移（0 = 用 offset.rs 里的常量）。
/// 数据线程在另一个线程里，所以只能靠这种全局变量把菜单的值传过去。
static STATE_OFFSET: AtomicI32 = AtomicI32::new(0);

/// 由渲染线程每帧同步一次
pub fn set_state_offset(offset: i32) {
    STATE_OFFSET.store(offset, Ordering::Relaxed);
}


/// 当前生效的状态字段偏移：
/// 菜单里填了就用菜单的，没填就用 `offset.rs` 里的（也可能是 None）。
fn state_offset() -> Option<usize> {
    let manual = STATE_OFFSET.load(Ordering::Relaxed);
    if manual > 0 {
        Some(manual as usize)
    } else {
        offset::STATE
    }
}

/// 自动检测出来的布局缓存。0 = 还没检测过，1 = 内联，2 = 指针
static AUTO_LAYOUT: AtomicU8 = AtomicU8::new(0);

/// 由渲染线程每帧同步一次（菜单里可以改）
pub fn set_layout(choice: EntityLayout) {
    FORCED_LAYOUT.store(
        match choice {
            EntityLayout::Auto => 0,
            EntityLayout::Inline => 1,
            EntityLayout::Pointer => 2,
        },
        Ordering::Relaxed,
    );
}

/// 一个玩家的「渲染用快照」。
///
/// 这里存的都是**已经加工好的纯数据**，渲染线程拿到就能画，不需要再碰游戏内存。
#[derive(Clone, Copy, Debug, Default)]
pub struct Player {
    /// 头部世界坐标
    pub head: Vec3,
    /// 脚部世界坐标
    pub feet: Vec3,
    /// 血量；`None` 表示没读到或数字离谱（偏移不对），此时不画血条
    pub health: Option<i32>,
    /// 是否活着（数据线程算好的，见下面的 `alive` 那行）。
    /// ESP 靠它过滤尸体 —— 尸体在实体列表里还会存留到玩家复活。
    pub alive: bool,
    /// 是不是本地玩家自己（需要 LOCAL_PLAYER 偏移，没配置时永远是 false）
    pub is_local: bool,
}

/// 一个实体的原始读数（故意不做加工，就是内存里长什么样），给自检面板看。
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub addr: usize,
    pub head: Vec3,
    pub feet: Vec3,
    /// 血量字段的**原始读数**。
    ///
    /// 注意这里是「没经过任何检查」的值：
    /// 一旦过滤过，你就看不出偏移到底对不对了（显示 `-` 到底是偏移错、还是血量真的是 0？）。
    pub health: Option<i32>,
    /// 原始读数是否落在“像血量”的范围（-100~200）内。
    /// **false 就说明 HEALTH 偏移很可能填错了**。
    pub health_valid: bool,
    /// 状态字段读数（`None` = 没配 STATE 偏移，或读到的是离谱值）
    pub state: Option<i32>,
    /// 是否通过了「像不像玩家」的校验
    pub ok: bool,
}

/// 自检信息：回答"到底读到了什么"。
///
/// ESP 画不出来有很多种原因（人数偏移错、布局错、坐标偏移错、矩阵错……），
/// 光看屏幕根本分不出来。把这些原始值直接显示在菜单里，一秒就能定位。
#[derive(Clone, Debug, Default)]
pub struct DebugInfo {
    /// `基址 + PLAYER_COUNT` 处读到的原始值
    pub raw_count: u32,
    /// `基址 + ENTITY_LIST` 处读到的原始值（指针布局下它就是数组首地址）
    pub list_raw: usize,
    /// 实际使用的布局描述
    pub layout: &'static str,
    /// 实际使用的数组基地址
    pub array_base: usize,
    /// 本次扫描了多少个槽位
    pub scanned: usize,
    /// 通过校验的实体数（= players.len()）
    pub valid: usize,
    /// 前几个实体的原始样本
    pub samples: Vec<Sample>,
    /// 实体列表前几个槽位里存的指针（0 = 空槽位）。
    /// 用来看清「列表里到底有几个实体」——有时人数偏移说有 3 个，实际只有 1 个非空。
    pub slots: Vec<usize>,
}

impl DebugInfo {
    /// 复位（每一帧开始读取前调用）。
    ///
    /// 【为什么不用 `DebugInfo::default()`】
    /// `default()` 会新建一个空的 `Vec`，等于**每帧丢一次堆内存**。
    /// 数据线程是 500Hz 的，这种「热循环里反复分配」是新手最常见的性能坑。
    /// 用 `clear()` 只把长度归零，**已经申请到的容量会保留下来复用**。
    pub fn reset(&mut self) {
        self.raw_count = 0;
        self.list_raw = 0;
        self.layout = "";
        self.array_base = 0;
        self.scanned = 0;
        self.valid = 0;
        self.samples.clear();
        self.slots.clear();
    }
}

/// 某一帧的完整数据快照。
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// 视图矩阵（16 个 f32）；`valid == false` 时不要使用
    pub view_matrix: [f32; 16],
    /// 这一帧数据是否有效。
    /// 站在主菜单、换地图、读取失败时都是 false，渲染端看到就不画东西。
    pub valid: bool,
    pub players: Vec<Player>,
    pub debug: DebugInfo,
}

/// 启动数据线程。
pub fn spawn(module_base: usize, shared: Arc<RwLock<Snapshot>>) {
    std::thread::spawn(move || {
        loop {
            // 临界区很小（就是几十次内存读取），写端也只有这一个线程，
            // 所以直接用 write() 不会成为瓶颈；渲染端拿不到锁时晚一帧即可。
            {
                let mut snapshot = match shared.write() {
                    Ok(guard) => guard,
                    // 锁被"毒化"（另一个线程 panic 过）也要继续干活，不要跟着一起炸
                    Err(poisoned) => poisoned.into_inner(),
                };
                read_into(module_base, &mut snapshot);
            }

            std::thread::sleep(READ_INTERVAL);
        }
    });
}

/// 读一帧数据写进 `snapshot`。
fn read_into(module_base: usize, snapshot: &mut Snapshot) {
    // 先标记无效：中途任何一步失败就直接 return，
    // 渲染端看到 valid == false 就不画，避免出现「上一帧的鬼影」。
    snapshot.valid = false;
    // clear() 只把长度归零，已经申请到的内存会留着复用 —— 避免每帧都重新分配
    snapshot.players.clear();

    // 上一帧通过校验的实体数：布局自动检测要靠它判断"要不要重新检测"。
    // 注意必须在 reset() 之前读，否则拿到手的就是已经被清掉的 0。
    let previous_valid = snapshot.debug.valid;
    snapshot.debug.reset();

    // ---------------- 1) 视图矩阵 ----------------
    let Some(view_matrix) = memory::read::<[f32; 16]>(module_base + offset::VIEW_MATRIX) else {
        return; // 读不到：可能还在读盘 / 换图中
    };

    // ---------------- 2) 玩家人数 ----------------
    let raw_count = memory::read::<u32>(module_base + offset::PLAYER_COUNT).unwrap_or(0);
    snapshot.debug.raw_count = raw_count; // 原样记下来给自检面板
    // 0 说明没进对局（或偏移不对）；超过上限说明偏移不对。
    // 宁可什么都不画，也不要拿着错误的数字去扫数组 —— 那会读到一大片垃圾内存。
    if raw_count == 0 || raw_count as usize > offset::MAX_PLAYERS {
        return;
    }

    // ---------------- 3) 实体列表：先确定布局 ----------------
    let list_slot = module_base + offset::ENTITY_LIST;
    let list_raw = memory::read::<u32>(list_slot).unwrap_or(0) as usize;
    snapshot.debug.list_raw = list_raw;

    let (layout_name, array_base, scan_count) =
        choose_layout(list_slot, list_raw, raw_count as usize, previous_valid);
    // 先把诊断信息填上，这样即使下面提前 return，自检面板也有内容可看
    snapshot.debug.layout = layout_name;
    snapshot.debug.array_base = array_base;
    snapshot.debug.scanned = scan_count;

    if !memory::is_readable(array_base, scan_count * 4) {
        return; // 数组地址本身不可读 → 布局判断错了
    }

    // ---------------- 4) 本地玩家 ----------------
    // LOCAL_PLAYER 未配置时这里是 0，下面就会把所有实体都当作"不是自己"。
    let local_addr = offset::LOCAL_PLAYER
        .and_then(|off| memory::read::<u32>(module_base + off))
        .unwrap_or(0) as usize;

    // ---------------- 5) 扫描实体列表 ----------------
    for i in 0..scan_count {
        // 数组里每个元素是 4 字节的 `entity*`，第 i 个元素的地址要乘 4
        let slot = array_base + i * 4;

        // 槽位里存的就是 `entity*`（4 字节），0 表示这个槽位没人
        let slot_raw = memory::read::<u32>(slot).unwrap_or(0) as usize;
        if snapshot.debug.slots.len() < 8 {
            snapshot.debug.slots.push(slot_raw);
        }
        if slot_raw == 0 {
            continue; // 空槽位
        }
        let entity_addr = slot_raw;

        let head = memory::read::<Vec3>(entity_addr + offset::HEAD_POSITION);
        let feet = memory::read::<Vec3>(entity_addr + offset::FEET_POSITION);
        // 读两份：原始值只用来显示，过滤过的值才参与判定
        let health_raw = read_health_raw(entity_addr);
        // 只有「像血量」的值才敢拿去判断生死，多离谱算离谱见下面 health_is_plausible
        let health = health_raw.filter(|hp| health_is_plausible(*hp));
        let state = read_state(entity_addr);

        // ★ 最关键的一步：只有「头脚坐标合理、且两点间距像个人」才当玩家。
        //   偏移填错、数组越界、玩家刚退出，都会在这里被刷掉，
        //   而不是拿着垃圾坐标去画一个满屏乱飞的方框。
        let pose_ok = match (head, feet) {
            (Some(h), Some(f)) => pose_is_sane(h, f),
            _ => false,
        };

        // 记个原始样本，菜单里能看到"到底读到了什么"
        if snapshot.debug.samples.len() < MAX_SAMPLES {
            snapshot.debug.samples.push(Sample {
                addr: entity_addr,
                head: head.unwrap_or_default(),
                feet: feet.unwrap_or_default(),
                health: health_raw,
                health_valid: health.is_some(),
                state,
                ok: pose_ok,
            });
        }

        let (head, feet) = match (head, feet) {
            (Some(h), Some(f)) if pose_ok => (h, f),
            _ => continue,
        };

        let is_local = local_addr != 0 && entity_addr == local_addr;

        // ★ 生死判定（决定要不要画这个实体）。
        //   优先信 state 字段（配了的话）：它在死亡瞬间变 1，最干脆。
        //   没配 STATE 偏移时退回用血量：实测活人 100、尸体 -10，
        //   也就是说 <= 0 就是死了。
        //   两个都读不到时不下结论（当作活着）—— 宁可多画一个，
        //   也不要把“偏移填错”变成“一个敌人都看不见”。
        let alive = match state {
            Some(s) => s == 0,
            None => health.map_or(true, |hp| hp > 0),
        };

        snapshot.players.push(Player {
            head,
            feet,
            health,
            alive,
            is_local,
        });
    }

    // ---------------- 6) 全部成功，公开给渲染线程 ----------------
    snapshot.debug.valid = snapshot.players.len();
    snapshot.view_matrix = view_matrix;
    snapshot.valid = true;
}

/// 读血量的原始值（不做任何检查）—— 自检面板和判定逻辑都用它。
///
/// 【为什么必须先读原始值，再另外过滤】
/// 血量偏移填错时，读到的往往是同一段内存里的别的字段，
/// 值可能是负数或者天文数字。而「不画尸体」的判断是看血量 <= 0 ——
/// 一旦这个垃圾值恰好 <= 0，敌人在你眼里就"全部阵亡"，表现就是**一个敌人都识别不到**。
/// AC 的血量正常范围是 0~100，超出这个范围说明偏移不对，直接当作"没读到"。
///
/// 但自检面板又需要看到**原始值**（不然显示个 `-`，你分不清是偏移错还是血量真的是 0），
/// 所以这里只负责读，过滤交给调用处。
fn read_health_raw(entity_addr: usize) -> Option<i32> {
    memory::read::<i32>(entity_addr + offset::HEALTH)
}

/// 血量取值是否「还像血量」。
///
/// 【已实测确认】
/// * 活人：`100`
/// * 尸体：**`-10`**（不是 0！）
///
/// 【为什么范围是 -100~200，而不是 0~100】
/// 一开始把范围卡得很严（0~100），结果 -10 被当成「偏移错了、没读到」，
/// 然后按“没读到就算了，当作活着”处理 → 尸体又被画出来。
/// 血量和人多多少少都会飘，所以范围要留得够宽，但也不能宽到
/// 把真正的垃圾值（比如 1065353216 —— 那其实是浮点数的位模式）也当血用。
fn health_is_plausible(hp: i32) -> bool {
    (-100..=200).contains(&hp)
}

/// 读玩家状态字段。没配偏移（`offset.rs::STATE` 是 None，菜单里也没填）时返回 None。
///
/// 同样做了范围检查：状态码只可能是 0~5，读到别的说明偏移填错了，
/// 当作没读到，而不是拿一个垃圾数字去判生死（那样可能把活人全滤掉）。
fn read_state(entity_addr: usize) -> Option<i32> {
    let off = state_offset()?;
    memory::read::<i32>(entity_addr + off).filter(|s| (0..=5).contains(s))
}

/// 头脚坐标是否合理：
/// * 是有限数、量级正常（挡掉 0 / NaN / 1e38 这类垃圾）；
/// * 头脚距离在 0.5~15 之间（AC 人物身高约 5 个方块，明显不对就说明坐标偏移错了）。
fn pose_is_sane(head: Vec3, feet: Vec3) -> bool {
    plausible(head) && plausible(feet) && (0.5..=15.0).contains(&head.distance(&feet))
}

fn plausible(v: Vec3) -> bool {
    v.x.is_finite()
        && v.y.is_finite()
        && v.z.is_finite()
        && v.x.abs() < 100_000.0
        && v.y.abs() < 100_000.0
        && v.z.abs() < 100_000.0
}

/// 决定用哪种布局读实体列表，返回 `(布局名, 数组首地址, 槽位数)`。
///
/// # 为什么需要它（识别不到敌人的头号原因）
/// 从 `基址 + ENTITY_LIST` 开始，可能是：
/// * **内联数组**：这个地址本身就是 `entity*[]` 的起点；
/// * **指针 / vector**：这个地址里存的是一个**指针**，指向真正的数组
///   （`std::vector<entity*>` 的 begin 指针就是这种）。
///
/// 两者只差一次解引用，但用错了就会读到一堆垃圾 —— 表现就是"有实体数但不画框"，
/// 或者干脆一个玩家都没有。
///
/// `previous_valid` 是上一帧通过校验的实体数：**只有它为 0 时才重新做自动检测**。
/// 两个理由：
/// * 性能：检测要抽样读十几次内存（每次都带 VirtualQuery），500Hz 下每帧都做纯属浪费；
/// * 稳定：抽样得分会随场上人数/生死状态波动，每帧重测会让布局两种之间反复横跳，
///   表现出来就是 ESP 一闪一闪的。
fn choose_layout(
    list_slot: usize,
    list_raw: usize,
    raw_count: usize,
    previous_valid: usize,
) -> (&'static str, usize, usize) {
    // 槽位数取「数量偏移」和「vector 的 end-begin」里**更大**的那个。
    //
    // 为什么要取大的：有些版本里 PLAYER_COUNT 读出来的其实是
    // std::vector 的 capacity（常见值就是 1、2、4、8），而不是玩家数量。
    // 只按它扫，就会永远只看到 1 个实体。
    // 多扫几个槽位没有副作用：没通过校验的会被直接丢掉。
    let count = raw_count
        .max(vector_len(list_slot).unwrap_or(0))
        .clamp(1, offset::MAX_PLAYERS);

    // ---- 菜单里手动指定了就用指定的（手动模式绕开缓存，方便两种对比）----
    match FORCED_LAYOUT.load(Ordering::Relaxed) {
        1 => return ("内联数组（手动）", list_slot, count),
        2 => return ("指针数组（手动）", list_raw, count),
        _ => {}
    }

    // ---- 自动模式：能用缓存就用缓存 ----
    let cached = AUTO_LAYOUT.load(Ordering::Relaxed);
    if cached != 0 && previous_valid > 0 {
        return if cached == 2 {
            ("指针数组（自动·缓存）", list_raw, count)
        } else {
            ("内联数组（自动·缓存）", list_slot, count)
        };
    }

    // ---- 需要（重新）检测：两种布局各打一次分，谁像玩家谁赢 ----
    let inline_score = score(list_slot, count);

    // list_raw 如果是指针，至少要是个像样的用户态地址（挡掉 0 和很小的垃圾值）
    let pointer_score = if list_raw > 0x1_0000 {
        score(list_raw, count)
    } else {
        0
    };

    if pointer_score > inline_score {
        AUTO_LAYOUT.store(2, Ordering::Relaxed);
        ("指针数组（自动）", list_raw, count)
    } else {
        AUTO_LAYOUT.store(1, Ordering::Relaxed);
        ("内联数组（自动）", list_slot, count)
    }
}

/// 如果 `list_slot` 处其实是个 `std::vector<entity*>`（begin / end 两个指针挨着放），
/// 就从两个指针的差值算出元素个数。
fn vector_len(list_slot: usize) -> Option<usize> {
    let begin = memory::read::<u32>(list_slot)? as usize;
    let end = memory::read::<u32>(list_slot + 4)? as usize;
    if begin == 0 || end <= begin {
        return None;
    }

    let n = (end - begin) / 4;
    if !(1..=offset::MAX_PLAYERS).contains(&n) {
        return None;
    }

    // 顺带确认这段内存真的可读，挡掉「碰巧算出个合理数字」的情况
    if !memory::is_readable(begin, n * 4) {
        return None;
    }

    Some(n)
}

/// 给某个候选布局打分：抽样看有几个槽位读出来的实体"像玩家"。
/// 分数高的就是真布局 —— 这是"让程序自己找出正确解引用次数"的土办法。
fn score(base: usize, count: usize) -> usize {
    let sample = count.min(DETECT_SAMPLE);
    if sample == 0 || !memory::is_readable(base, sample * 4) {
        return 0;
    }

    let mut good = 0;
    for i in 0..sample {
        if let Some(addr) = memory::read::<u32>(base + i * 4) {
            let addr = addr as usize;
            if addr == 0 {
                continue;
            }
            let head = memory::read::<Vec3>(addr + offset::HEAD_POSITION);
            let feet = memory::read::<Vec3>(addr + offset::FEET_POSITION);
            if let (Some(h), Some(f)) = (head, feet) {
                if pose_is_sane(h, f) {
                    good += 1;
                }
            }
        }
    }
    good
}

// ============================================================================
// 字段扫描（调试工具）—— “怎么找出血量/状态字段的偏移”
// ============================================================================

/// 扫描范围：从实体地址开始看前 0x400 字节（=256 个 int 字段）。
/// AC 的 playerent 结构就几百字节，够用了。
pub const FIELD_SCAN_RANGE: usize = 0x400;

/// 一行扫描结果：同一个偏移上，各个实体读到的 i32 值。
///
/// 【怎么用它】
/// 站在敌人旁边点一次扫描 → 把他打死 → 再点一次，
/// **哪一行的数字变了，那个偏移就是你要找的字段**（血量/状态都是这样找的）。
/// 懒得对比两次也行：活人和尸体同时在场时，直接看“0 和 1 混在一起”的那一行。
#[derive(Clone, Debug)]
pub struct FieldRow {
    pub offset: usize,
    pub values: Vec<Option<i32>>,
}

/// 对给定的几个实体地址，逐个偏移读 i32。
///
/// 【为什么这里可以在渲染线程里读内存】
/// 正常 ESP 绝不会这么干（读内存全部放在数据线程，见本文件开头），
/// 但这个函数只有你手动点一下按钮才跑，也只读几百个 int，开销可以忽略。
/// 反过来说，这也是一次很好的反面教材：**别把这种「每个偏移都读一遍」放进每帧循环**。
pub fn scan_fields(addrs: &[usize], range: usize) -> Vec<FieldRow> {
    let mut rows = Vec::with_capacity(range / 4);
    for offset in (0..range).step_by(4) {
        let values = addrs
            .iter()
            .map(|&addr| memory::read::<i32>(addr + offset))
            .collect();
        rows.push(FieldRow { offset, values });
    }
    rows
}
