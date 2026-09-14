//! ESP 绘制：方框、引导线、血条。
//!
//! # 本文件实现的功能
//! * ✅ **引导线**：从屏幕上的固定点连到目标身上，敌人在屏幕边缘时能一眼看出方向；
//! * ✅ **方框**：由「头」「脚」两个投影点推出矩形，高度天然带透视（越远越矮）；
//! * ✅ **血条**：画在方框左侧，颜色随血量比例由绿变红；
//! * ✅ **过滤**：死人（血量 <= 0）不画 —— 见 `data.rs` 里算好的 `alive`。
//!
//! 刻意**没有**实现：名字、队伍颜色、距离数字。
//! 前两个需要还没验证过的偏移；距离要先反推相机位置，实测偏差不好控制，
//! 练手阶段不值得为它拖一堆代码 —— README 的「功能清单」里标了每项的状态。
//!
//! # 一、用的是什么"画布"
//! 全程用 imgui 的 **draw list**，不创建任何 imgui 窗口。层级关系是：
//!
//! ```text
//!   background draw list      ← 画在所有窗口的下面
//!     普通窗口（我们的菜单）    ← ui.window(...)
//!   foreground draw list      ← 画在所有窗口的上面   ★ ESP 在这里
//! ```
//!
//! 所以我们用的是 `ui.get_foreground_draw_list()`：
//! 这样方框既不会被菜单盖住，也不会被游戏自己的 UI 盖住。
//! 图省事用 background list 的话，菜单一打开方框就全被挡住了。
//!
//! # 二、坐标系
//! ```text
//!   世界坐标 Vec3 --memory::world_to_screen--> 屏幕像素 Vec2
//! ```
//! 屏幕原点在**左上角**，x 向右、y 向下（和 imgui 一致，和 OpenGL 的 NDC 相反）。
//!
//! # 三、绘制顺序 = 图层顺序
//! 「后面画的压在先画的上面」，所以顺序是：过滤 → 投影 → 算方框 → 拉线 → 方框 → 血条。

use imgui::Ui;

use crate::config::{Config, SnapTarget};
use crate::data::Snapshot;
use crate::memory;

/// 画出所有玩家。由 `lib.rs` 的 `render()` 每帧调用。
pub fn draw(ui: &Ui, config: &Config, snapshot: &Snapshot) {
    // 总开关关了，或者这一帧数据无效（主菜单 / 换图中）—— 什么都不画
    if !config.esp_enabled || !snapshot.valid {
        return;
    }

    // 屏幕尺寸取 imgui 的 display_size，这样算出来的坐标和实际画面完全一致。
    // （lib.rs 的 before_render 每帧都会把它校正成真实客户区尺寸。）
    let [screen_w, screen_h] = ui.io().display_size;
    if screen_w <= 0.0 || screen_h <= 0.0 {
        return;
    }

    let draw_list = ui.get_foreground_draw_list();

    // ---------------- 0) 自瞄的 FOV 圈 ----------------
    // 画在屏幕中心、半径 = aim_fov：调自瞄时一眼能看出「圈多大、谁会被瞄」。
    // 属于自瞄的可视化辅助，不算 ESP 内容，所以跟着 aim_enabled 走。
    if config.aim_enabled && config.draw_fov_circle && config.aim_fov > 0.0 {
        draw_list
            .add_circle(
                [screen_w * 0.5, screen_h * 0.5],
                config.aim_fov,
                config.color_fov,
            )
            .thickness(1.2)
            .build();
    }

    // 拉线起点：所有玩家共用，所以只在循环外算一次
    let line_origin = config
        .snap_origin
        .point(screen_w, screen_h, config.snap_offset);

    for player in &snapshot.players {
        // ---------------- 1) 过滤 ----------------
        // is_local 依赖 LOCAL_PLAYER 偏移，没配置时它永远是 false，
        // 于是"不画自己"这项会自动失效（而不是误判）。
        if config.ignore_self && player.is_local {
            continue;
        }
        // 尸体过滤：尸体是「坐标不动、还留在实体列表里」的实体，会一直存在到本人复活，
        // 所以不能只靠坐标区分，要看血量（实测：活人 100 / 尸体 -10）。
        if config.ignore_dead && !player.alive {
            continue;
        }

        // ---------------- 2) 3D 世界坐标 → 2D 屏幕坐标 ----------------
        // 头和脚分别投影。返回 None 的常见原因：这个点在摄像机背后，
        // 或者视图矩阵是错的。直接跳过，绝不留下鬼线。
        let Some(head) =
            memory::world_to_screen(player.head, &snapshot.view_matrix, screen_w, screen_h)
        else {
            continue;
        };
        let Some(feet) =
            memory::world_to_screen(player.feet, &snapshot.view_matrix, screen_w, screen_h)
        else {
            continue;
        };

        // ---------------- 3) 由两个 2D 点推出方框 ----------------
        // 高度直接用两个投影点的纵向距离 —— 这天然包含了「离得越远越矮」的透视。
        // 宽度按人体比例推：身高 : 肩宽 ≈ 2.2 : 1，所以比例取 0.45 左右。
        let height = (feet.y - head.y).abs();
        if height < 2.0 {
            // 太小（极远、或数据在抖）时画出来只是一条糊线，不如不画
            continue;
        }
        let width = height * config.box_width_ratio;
        let box_min = [head.x - width * 0.5, head.y.min(feet.y)];
        let box_max = [head.x + width * 0.5, head.y.max(feet.y)];

        // ---------------- 4a) 引导线 ----------------
        // 从屏幕固定点连到目标身上，敌人在屏幕边缘时能一眼看出方向。
        if config.draw_snapline {
            let target = match config.snap_target {
                SnapTarget::Head => head,
                SnapTarget::Feet => feet,
            };
            // add_line 返回一个"构造器"，可以链式设置粗细，最后 .build() 才真正写入。
            draw_list
                .add_line(line_origin, target.to_array(), config.color_line)
                .thickness(config.snap_thickness)
                .build();
        }

        // ---------------- 4b) 方框 ----------------
        if config.draw_box {
            // 同样：add_rect 要链式设置完再 .build()。
            // .thickness() 是线宽，.filled() 是实心还是空心。
            draw_list
                .add_rect(box_min, box_max, config.color_box)
                .thickness(config.box_thickness)
                .build();
        }

        // ---------------- 4c) 血条（画在方框左边） ----------------
        // health 为 None 说明读不到，那就干脆不画，
        // 免得画一条永远满的血条误导自己。
        if config.draw_health_bar {
            if let Some(hp) = player.health {
                if hp > 0 {
                    let bar_x = box_min[0] - config.health_bar_width - 3.0;
                    let bar_top = box_min[1];
                    let bar_bottom = box_max[1];
                    let bar_h = bar_bottom - bar_top;

                    // 血量比例（0..1），clamp 防止读到超出范围的垃圾值把条画飞
                    let ratio = (hp as f32 / config.max_health).clamp(0.0, 1.0);
                    let filled_h = bar_h * ratio;

                    // 先铺一层半透明黑底：背景是亮色天空时血条也看得清
                    draw_list
                        .add_rect(
                            [bar_x, bar_top],
                            [bar_x + config.health_bar_width, bar_bottom],
                            [0.0, 0.0, 0.0, 0.6],
                        )
                        .filled(true)
                        .build();

                    // 血量从下往上长（底部对齐）
                    draw_list
                        .add_rect(
                            [bar_x, bar_bottom - filled_h],
                            [bar_x + config.health_bar_width, bar_bottom],
                            health_color(ratio),
                        )
                        .filled(true)
                        .build();
                }
            }
        }
    }
}

/// 血量比例 → 颜色：1.0 是绿的，0.5 偏黄，0.0 是红的。
/// 做法就是把红和绿做反向线性插值，中间自然经过黄色。
fn health_color(ratio: f32) -> [f32; 4] {
    [1.0 - ratio, ratio, 0.0, 1.0]
}
