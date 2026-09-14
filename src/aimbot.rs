//! 【本文件职责】本文件实现：**自瞄** —— 把准星自动移向 FOV 内离准星最近的敌人。
//!
//! # 一、原理（和 ESP 共用同一套数据，只是从"读"变成了"写"）
//!
//! ESP 是：读内存 → 世界坐标投影到屏幕 → 画出来；
//! 自瞄是：读内存 → 世界坐标投影到屏幕 → **算准星和目标的像素差 → 移动鼠标**。
//!
//! 为什么用屏幕投影，而不是像传统自瞄那样直接写视角（yaw / pitch）：
//! * 直接写视角需要视角角度、灵敏度等**额外的偏移**，每个游戏版本都得重扫；
//! * 屏幕投影用的视图矩阵我们已经有了（ESP 正在用），一个偏移都不用加；
//! * 屏幕中心到目标头的像素差，就是"准星还差多少"，
//!   乘一个缩放系数（对应游戏灵敏度）再用相对鼠标移动补上即可。
//!
//! # 二、移动鼠标用什么
//!
//! `SendInput(MOUSEEVENTF_MOVE)` 发**相对**鼠标移动：
//! * AssaultCube 用 SDL 读输入，相对移动正好等价于"玩家动了一下鼠标"；
//! * `SetCursorPos` 是绝对定位，对锁光标的 FPS 基本无效 ——
//!   README 功能清单里写的是 "SetCursorPos / SendInput"，实际只有 SendInput 可用。
//!
//! # 三、和菜单输入的关系
//!
//! 菜单开着时直接跳过自瞄：一来调菜单时不希望准星乱飞，
//! 二来 hudhook 的输入过滤可能拦掉注入的鼠标事件，行为不可预期。

use std::sync::atomic::{AtomicI32, Ordering};

use imgui::Ui;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MOUSEEVENTF_MOVE, MOUSEINPUT, INPUT, INPUT_0, INPUT_MOUSE, SendInput,
    VK_RBUTTON,
};

use crate::config::{Config, SnapTarget};
use crate::data::Snapshot;
use crate::memory::{self, Vec2};

/// 每帧由 `lib.rs::render()` 调用。
pub fn run(ui: &Ui, config: &Config, snapshot: &Snapshot) {
    // 开关没开、数据无效（主菜单/换图）、菜单开着 —— 都不动鼠标
    if !config.aim_enabled || !snapshot.valid || config.menu_open {
        return;
    }

    // hold 模式：只在按住鼠标右键时才瞄，松开立刻停。
    // 和热键切换（aim_enabled）是两个独立的条件，这里是在总开关之上再加一道"闸门"。
    if config.aim_hold_mode && !right_mouse_down() {
        return;
    }

    let [screen_w, screen_h] = ui.io().display_size;
    if screen_w <= 0.0 || screen_h <= 0.0 {
        return;
    }
    let center = Vec2::new(screen_w * 0.5, screen_h * 0.5);

    // FOV 以「屏幕像素半径」表示（不是角度）：投影出来本来就是像素坐标，
    // 直接比像素距离最省事，菜单里也最好调 —— 拉多大圈一眼就能看见
    let fov = config.aim_fov;
    if !(fov > 0.0) {
        return;
    }
    let fov_sq = fov * fov;

    // ---------------- 1) 选目标：FOV 内、离屏幕中心最近的活人 ----------------
    // 「离准星最近」而不是「血最少/最近」：自瞄的本意是把准星挪过去，
    // 所以谁离准星近谁就最该被瞄 —— 这也是绝大多数自瞄的默认选法。
    let mut best: Option<(f32, Vec2)> = None;
    for player in &snapshot.players {
        // 过滤规则和 esp.rs 保持一致：不瞄自己 / 不瞄尸体
        if config.ignore_self && player.is_local {
            continue;
        }
        if !player.alive {
            continue;
        }

        // 瞄头还是瞄脚：头部命中率高（很多游戏爆头伤害翻倍），脚部只是调试用
        let world = match config.aim_target {
            SnapTarget::Head => player.head,
            SnapTarget::Feet => player.feet,
        };

        // 投影失败 = 目标在摄像机背后（w <= 0），瞄它只会把准星拽向反方向
        let Some(sp) = memory::world_to_screen(world, &snapshot.view_matrix, screen_w, screen_h)
        else {
            continue;
        };

        let dx = sp.x - center.x;
        let dy = sp.y - center.y;
        let dist_sq = dx * dx + dy * dy;
        if dist_sq > fov_sq {
            continue; // 在 FOV 圈外，不瞄
        }
        if best.map_or(true, |(d, _)| dist_sq < d) {
            best = Some((dist_sq, Vec2::new(dx, dy)));
        }
    }
    let Some((_, delta)) = best else {
        return; // FOV 内一个目标都没有
    };

    // ---------------- 2) 像素差 → 鼠标移动量 ----------------
    let mut dx = delta.x * config.aim_speed;
    let mut dy = delta.y * config.aim_speed;

    // 平滑：把每帧的移动量除以 N，分多帧累积到目标上。
    // N = 1 就是"瞬吸"，N 越大越像人手 —— 一行代码的"拟人化"。
    let smooth = config.aim_smooth.max(1.0);
    dx /= smooth;
    dy /= smooth;

    // 死区：目标基本已经套住时不再发事件，防止准星在头上原地抖
    if dx.abs() < 0.5 && dy.abs() < 0.5 {
        return;
    }

    send_move(dx, dy);
}

// ---------------------------------------------------------------------------
// 发送相对鼠标移动
// ---------------------------------------------------------------------------

/// 鼠标右键当前是否按下。`GetAsyncKeyState` 读的是全局按键状态，
/// 和窗口消息无关，所以即使我们/游戏锁了光标也能正常检测。
/// 最高位（0x8000）表示"此刻正被按下"。
fn right_mouse_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_RBUTTON as i32) as u16 & 0x8000) != 0 }
}

/// 帧间的移动量小数余量，单位 0.01 像素。
///
/// 【为什么必须攒余量】`SendInput` 只收**整数**像素。开了平滑之后，
/// 每帧的移动量经常不到 1 像素（比如 0.7），直接 `as i32` 截断成 0 ——
/// 结果就是目标很近时准星永远差最后一点，看起来像"卡住了"。
/// 把每帧的零头存起来，凑够 1 像素再发，平滑才能真正做到位。
static RESIDUAL_X: AtomicI32 = AtomicI32::new(0);
static RESIDUAL_Y: AtomicI32 = AtomicI32::new(0);

fn send_move(dx: f32, dy: f32) {
    // 用 0.01 像素做定点数：i32 足够表示 ±327 像素的帧移动量，范围绰绰有余
    let want_x = (dx * 100.0) as i32 + RESIDUAL_X.swap(0, Ordering::Relaxed);
    let want_y = (dy * 100.0) as i32 + RESIDUAL_Y.swap(0, Ordering::Relaxed);

    // 向零取整：正负余量都留在原地，下一帧继续用
    let move_x = want_x / 100;
    let move_y = want_y / 100;
    RESIDUAL_X.store(want_x - move_x * 100, Ordering::Relaxed);
    RESIDUAL_Y.store(want_y - move_y * 100, Ordering::Relaxed);

    if move_x == 0 && move_y == 0 {
        return;
    }

    // MOUSEEVENTF_MOVE 且不设 MOUSEEVENTF_ABSOLUTE = 相对移动（正右负左 / 正下负上）
    let mut input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: move_x,
                dy: move_y,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // 返回值是成功注入的事件数，失败（极少见）就丢掉这一帧，不影响下一帧
    let _ = unsafe { SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32) };
}
