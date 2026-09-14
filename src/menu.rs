//! 【本文件职责】本文件实现：**imgui 菜单**（所有功能的操作入口）+ 自检面板。
//!
//! imgui 菜单：所有开关都在这里。
//!
//! 菜单本身就是一个普通的 imgui 窗口，可以拖动、可以关闭；
//! 它只负责**改配置**，真正画 ESP 的是 `esp.rs`。
//!
//! # 这个文件跟 imgui 学的东西
//! * `ui.window(标题).size(...).position(...).build(|| { ... })`
//!   闭包里就是窗口内容，闭包结束时窗口自动"结束"（不用手动 end）；
//! * `ui.checkbox` / `ui.slider` / `ui.combo_simple_string` / `ui.color_edit4`
//!   大部分控件都返回 `bool`：表示"这一帧用户有没有改它"；
//! * 控件的可变借用：我们把 `&mut Config` 传进来，直接改字段就行，
//!   下一帧 `esp.rs` 读到的就是新值（这也是"所见即所得"能实现的原因）。

use imgui::{Condition, TreeNodeFlags, Ui};

use crate::config::{Config, EntityLayout, SnapOrigin, SnapTarget};
use crate::data::{FieldRow, Snapshot};
use crate::hotkeys::Hotkey;

/// 窗口标题。`###` 后面是 imgui 用来标识窗口的 ID：
/// 这样即使前面的显示文字改了，imgui 也不会把窗口当成"新窗口"而重置位置大小。
const MENU_TITLE: &str = "AC ESP 学习版###ac_esp_menu";

pub fn draw(ui: &Ui, config: &mut Config, snapshot: &Snapshot) {
    if !config.menu_open {
        return;
    }

    ui.window(MENU_TITLE)
        .size([430.0, 620.0], Condition::FirstUseEver)
        .position([30.0, 30.0], Condition::FirstUseEver)
        .build(|| {
            ui.text(format!(
                "按 {} 显示/隐藏菜单，按 {} 开关 ESP",
                config.menu_key.label(),
                config.esp_key.label()
            ));
            ui.separator();

            // ---------------- 总开关 ----------------
            ui.checkbox("启用 ESP（总开关）", &mut config.esp_enabled);
            if !config.esp_enabled {
                ui.text_colored([1.0, 0.6, 0.2, 1.0], "ESP 已关闭");
            }
            ui.separator();

            // ---------------- 画什么 ----------------
            ui.text("绘制内容");
            ui.checkbox("方框", &mut config.draw_box);
            ui.checkbox("引导线（拉线）", &mut config.draw_snapline);
            ui.checkbox("血量条", &mut config.draw_health_bar);


            // 引导线参数：只有勾了引导线才展开，界面更清爽。
            // ui.indent() / ui.unindent() 就是往右缩进一级 / 退回一级。
            if config.draw_snapline {
                ui.indent();

                // 枚举 ↔ 下拉框：imgui 只认识"第几项"，
                // 所以用 index() 拿出当前序号、from_index() 把用户选择转回枚举。
                let mut origin_index = config.snap_origin.index();
                if ui.combo_simple_string(
                    "起点",
                    &mut origin_index,
                    &SnapOrigin::ALL.map(|o| o.label()),
                ) {
                    config.snap_origin = SnapOrigin::from_index(origin_index);
                }

                let mut target_index = config.snap_target.index();
                if ui.combo_simple_string(
                    "连到",
                    &mut target_index,
                    &SnapTarget::ALL.map(|o| o.label()),
                ) {
                    config.snap_target = SnapTarget::from_index(target_index);
                }

                ui.slider("起点左右偏移", -400.0, 400.0, &mut config.snap_offset);
                ui.slider("线宽", 0.5, 6.0, &mut config.snap_thickness);

                ui.unindent();
            }

            if config.draw_box {
                ui.indent();
                ui.slider("方框线宽", 0.5, 6.0, &mut config.box_thickness);
                ui.slider("方框宽高比", 0.1, 1.0, &mut config.box_width_ratio);
                ui.unindent();
            }

            if config.draw_health_bar {
                ui.indent();
                ui.slider("血条宽度", 1.0, 12.0, &mut config.health_bar_width);
                ui.slider("血量上限", 1.0, 200.0, &mut config.max_health);
                ui.unindent();
            }

            ui.separator();

            // ---------------- 过滤 ----------------
            ui.text("过滤");
            ui.checkbox("不画尸体（血量 <= 0）", &mut config.ignore_dead);
            ui.checkbox("不画自己（需要 LOCAL_PLAYER 偏移）", &mut config.ignore_self);

            ui.separator();

            // ---------------- 自瞄 ----------------
            // 和 ESP 共用同一套「投影」数据，只是把「画框」换成了「移鼠标」（aimbot.rs）。
            ui.text("自瞄");
            ui.checkbox("启用自瞄（默认关！）", &mut config.aim_enabled);
            if config.aim_enabled {
                ui.indent();
                ui.text_colored([1.0, 0.5, 0.3, 1.0], "准星会自己动，注意场合！");

                // 瞄哪个点：复用引导线的 Head/Feet 枚举
                let mut aim_target_index = config.aim_target.index();
                if ui.combo_simple_string(
                    "瞄向",
                    &mut aim_target_index,
                    &SnapTarget::ALL.map(|t| t.label()),
                ) {
                    config.aim_target = SnapTarget::from_index(aim_target_index);
                }

                ui.slider("FOV 半径（像素）", 20.0, 600.0, &mut config.aim_fov);
                ui.slider("平滑（1=瞬吸，越大越慢）", 1.0, 10.0, &mut config.aim_smooth);
                ui.slider("速度倍率（对灵敏度）", 0.2, 3.0, &mut config.aim_speed);
                ui.checkbox("画出 FOV 圈", &mut config.draw_fov_circle);
                ui.checkbox("长按右键才瞄（hold 模式）", &mut config.aim_hold_mode);
                ui.text_disabled("hold 模式：按住右键自瞄，松开即停（仍需先开总开关）");
                ui.text_disabled("调参顺序：先开 FOV 圈 → 调速度倍率到准星能对准头");
                ui.text_disabled("→ 再加大平滑去掉「吸附感」。");
                ui.unindent();
            }

            let mut aim_key_index = config.aim_key.index();
            if ui.combo_simple_string(
                "自瞄开关键",
                &mut aim_key_index,
                &Hotkey::ALL.map(|k| k.label()),
            ) {
                config.aim_key = Hotkey::from_index(aim_key_index);
            }

            ui.separator();

            // ---------------- 界面 ----------------
            ui.text("界面");
            ui.checkbox(
                "菜单打开时解除光标锁定 (ClipCursor)",
                &mut config.release_cursor,
            );
            ui.text_disabled("鼠标点不中菜单 / 不跟手时，留着勾上");
            ui.slider("UI 缩放", 0.7, 1.8, &mut config.ui_scale);

            ui.separator();

            // ---------------- 热键 ----------------
            ui.text("热键");
            let mut menu_key_index = config.menu_key.index();
            if ui.combo_simple_string(
                "菜单键",
                &mut menu_key_index,
                &Hotkey::ALL.map(|k| k.label()),
            ) {
                config.menu_key = Hotkey::from_index(menu_key_index);
            }

            let mut esp_key_index = config.esp_key.index();
            if ui.combo_simple_string("ESP 键", &mut esp_key_index, &Hotkey::ALL.map(|k| k.label()))
            {
                config.esp_key = Hotkey::from_index(esp_key_index);
            }

            // 三个热键里任意两个一样，按一下就互相抵消，这里提前提醒
            if config.menu_key == config.esp_key
                || config.menu_key == config.aim_key
                || config.esp_key == config.aim_key
            {
                ui.text_colored([1.0, 0.5, 0.0, 1.0], "热键不能重复！");
            }

            ui.checkbox("吞掉热键（不让游戏收到）", &mut config.swallow_hotkeys);
            ui.indent();
            ui.text_disabled("hudhook 只能按消息类别拦输入，拦不住单个按键，");
            ui.text_disabled("所以 hotkeys.rs 另外包了一层窗口过程把这两个键截走。");
            ui.text_disabled("如果某个键在游戏里还是有反应，换个键即可。");
            ui.unindent();

            ui.separator();

            // ---------------- 颜色 ----------------
            // collapsing_header 是可折叠的区域，默认收起，省地方
            if ui.collapsing_header("颜色", TreeNodeFlags::empty()) {
                ui.color_edit4("方框", &mut config.color_box);
                ui.color_edit4("引导线", &mut config.color_line);
            }

            ui.separator();

            // ---------------- 自检 ----------------
            // 识别不到敌人时，答案基本都在这里。按顺序看：
            //   1. 数据状态 = 无数据      → 还在主菜单/换图，或 VIEW_MATRIX 偏移错
            //   2. 原始玩家数 = 0         → PLAYER_COUNT 偏移错
            //   3. 通过校验 = 0（前几项正常）→ 布局错（换下面的下拉框），或坐标偏移错
            //   4. 通过校验 > 0 但没有方框 → 视图矩阵错
            if ui.collapsing_header("自检", TreeNodeFlags::empty()) {
                let d = &snapshot.debug;

                ui.text(format!(
                    "数据状态: {}",
                    if snapshot.valid {
                        "正常"
                    } else {
                        "无数据（主菜单 / 换图中 / 偏移错）"
                    }
                ));
                ui.text(format!("原始玩家数: {}", d.raw_count));
                ui.text(format!("列表槽位值: 0x{:X}", d.list_raw));
                ui.text(format!("使用布局: {}", d.layout));
                ui.text(format!("数组基址: 0x{:X}", d.array_base));
                ui.text(format!("扫描槽位: {}   通过校验: {}", d.scanned, d.valid));
                ui.text(format!(
                    "视图矩阵首行: {:.3}  {:.3}  {:.3}  {:.3}",
                    snapshot.view_matrix[0],
                    snapshot.view_matrix[1],
                    snapshot.view_matrix[2],
                    snapshot.view_matrix[3],
                ));

                // 直接把「下一步该改什么」写在这里，省得对着文档查表
                if d.raw_count == 0 {
                    ui.text_colored(
                        [1.0, 0.4, 0.4, 1.0],
                        "玩家数为 0 → PLAYER_COUNT 偏移不对",
                    );
                } else if snapshot.valid && d.valid == 0 {
                    ui.text_colored(
                        [1.0, 0.6, 0.0, 1.0],
                        "有玩家数但 0 个通过校验 → 换布局试试，都不行就是坐标偏移错",
                    );
                }

                // 手动切换布局（自动检测不准时的救命开关）
                let mut layout_index = config.entity_layout.index();
                if ui.combo_simple_string(
                    "实体列表布局",
                    &mut layout_index,
                    &EntityLayout::ALL.map(|l| l.label()),
                ) {
                    config.entity_layout = EntityLayout::from_index(layout_index);
                }

                // 实体样本 = 原始读数，能直接看出偏移对不对：
                // 头脚全是 0 或天文数字 ⇒ 坐标偏移错；一行 OK 都没有也一样。
                // hp 后面带 (超出范围) 表示读到的数字完全不像血量（比如 1065353216）。
                //
                // 先看一眼槽位：人数偏移说“有 2 个”，也可能只有 1 个非空槽位。
                if !d.slots.is_empty() {
                    let text = d
                        .slots
                        .iter()
                        .map(|s| {
                            if *s == 0 {
                                "空".to_string()
                            } else {
                                format!("0x{s:X}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    ui.text(format!("槽位指针：{text}"));
                }

                if !d.samples.is_empty() {
                    ui.text("实体样本（原始读数）:");
                    ui.indent();
                    for s in &d.samples {
                        ui.text(format!(
                            "0x{:X} {} head({:.0},{:.0},{:.0}) feet({:.0},{:.0},{:.0}) hp {} state {}",
                            s.addr,
                            if s.ok { "OK" } else { "NG" },
                            s.head.x,
                            s.head.y,
                            s.head.z,
                            s.feet.x,
                            s.feet.y,
                            s.feet.z,
                            match s.health {
                                // (超出范围) = 读到了数字，但根本不像血量
                                Some(hp) => format!("{hp}{}", if s.health_valid { "" } else { " (超出范围)" }),
                                None => "读不到".to_string(),
                            },
                            match s.state {
                                Some(st) => st.to_string(),
                                None => "-".to_string(),
                            }
                        ));
                    }
                    ui.unindent();
                }

                ui.separator();

                // ---------------- 尸体滤不掉的排查工具 ----------------
                // 用法：填一个候选偏移 → 去把人打死 → 看上面样本的 state 列
                //       （死亡瞬间变成 1 的那个偏移就是它）→ 写进 offset.rs::STATE
                //       → 这里改回 0。
                ui.text("状态字段偏移（找尸体用的）");
                ui.slider("偏移", 0, 1024, &mut config.state_offset);
                ui.text_disabled("Ctrl + 点滑块 可直接输入数值（输入十进制）");
                ui.text_disabled("0 = 不用（那就退回看血量）");
                if config.state_offset > 0 {
                    ui.text_colored(
                        [0.6, 0.9, 1.0, 1.0],
                        format!("正在用偏移 0x{:X} 判断生死", config.state_offset),
                    );
                }
            }

            ui.separator();

            // ---------------- 字段扫描（找血量 / 状态偏移） ----------------
            // 血量、状态这两个字段的偏移都不要靠猜，直接对比出来：
            //   ① 站在敌人旁边，点「扫描」+「把实体1存为基准」；
            //   ② 把他打死；
            //   ③ 勾上「和基准对比」再扫描 —— 哪一行变了，那个偏移就是要找的字段。
            if ui.collapsing_header("字段扫描（找血量 / 状态）", TreeNodeFlags::empty()) {
                if ui.button("扫描实体字段") {
                    // 用自检样本里的实体地址（跳过空槽位），最多 4 个，不然排版太宽
                    let addrs: Vec<usize> = snapshot
                        .debug
                        .samples
                        .iter()
                        .map(|s| s.addr)
                        .filter(|a| *a != 0)
                        .take(4)
                        .collect();
                    config.field_scan_addrs = addrs.clone();
                    config.field_rows =
                        crate::data::scan_fields(&addrs, crate::data::FIELD_SCAN_RANGE);
                }
                ui.same_line();
                ui.text_disabled(format!("实体数：{}", config.field_scan_addrs.len()));

                ui.same_line();
                if ui.button("把实体1存为基准") {
                    config.field_baseline = baseline_from(config);
                }

                ui.checkbox("和基准对比（只显示变了的行）", &mut config.field_scan_use_baseline);
                ui.checkbox("只看不同的行", &mut config.field_scan_diff_only);
                ui.checkbox("只看小整数（0~8）", &mut config.field_scan_small_int_only);

                if let Some((addr, _)) = &config.field_baseline {
                    ui.text_disabled(format!("基准实体：0x{addr:X}"));
                    // 地址变了就说明实体被重新分配过，当时的基准已经不能用了
                    let current = config.field_scan_addrs.first().copied().unwrap_or(0);
                    if current != 0 && current != *addr {
                        ui.text_colored(
                            [1.0, 0.8, 0.3, 1.0],
                            "(!) 实体1 的地址变了，对比结果不可信，请重新存基准",
                        );
                    }
                }

                // ---- 重点提示 1：和基准对比，直接把变化了的字段列出来 ----
                if config.field_scan_use_baseline {
                    if let Some((_, base)) = &config.field_baseline {
                        let changed: Vec<String> = config
                            .field_rows
                            .iter()
                            .filter(|row| row.values.first().copied().flatten() != value_in(base, row.offset))
                            .take(12)
                            .map(|r| format!("0x{:X}", r.offset))
                            .collect();
                        if changed.is_empty() {
                            ui.text_disabled("没发现变化的字段（基准存的是这个实体吗？）");
                        } else {
                            ui.text_colored(
                                [1.0, 0.6, 0.4, 1.0],
                                format!("变化的字段：{}", changed.join("  ")),
                            );
                        }
                    }
                }

                // ---- 重点提示 2：各实体取值不同、且都是 0~8 的小整数 → 状态码就是这种 ----
                // 不截断：列全所有候选，靠后的偏移也不会被吃掉。
                // 实在太多的话下方的数据表可以配合「只看小整数」勾缩小范围。
                let suspects: Vec<String> = config
                    .field_rows
                    .iter()
                    .filter(|r| row_varies(r) && row_is_small_int(r))
                    .map(|r| format!("0x{:X}", r.offset))
                    .collect();
                if !suspects.is_empty() {
                    ui.text_colored(
                        [0.4, 1.0, 0.4, 1.0],
                        format!("可疑字段（共 {} 个）：{}", suspects.len(), suspects.join("  ")),
                    );
                    ui.text_disabled("（一行里一个是 0、另一个是 1 的，基本就是 STATE）");
                }

                // ---- 数据表：偏移 + 每个实体在该偏移处的值 ----
                let mut shown = 0;
                for row in &config.field_rows {
                    // 勾了「和基准对比」就只显示变化了的行（这一步比什么都管用）
                    if config.field_scan_use_baseline {
                        if let Some((_, base)) = &config.field_baseline {
                            if row.values.first().copied().flatten() == value_in(base, row.offset) {
                                continue;
                            }
                        }
                    }
                    if config.field_scan_diff_only && !row_varies(row) {
                        continue;
                    }
                    if config.field_scan_small_int_only && !row_is_small_int(row) {
                        continue;
                    }
                    if shown >= 60 {
                        ui.text_disabled("...（只显示前 60 行，用上面几个勾缩小范围）");
                        break;
                    }

                    let mut line = format!("0x{:03X}   ", row.offset);
                    for value in &row.values {
                        line.push_str(&format!("{:<10} ", fmt_value(*value)));
                    }

                    // 一行里同时出现 0 和 1 → 很可能就是想要的“生死字段”
                    let looks_like_flag = row.values.contains(&Some(1)) && row.values.contains(&Some(0));
                    if looks_like_flag {
                        ui.text_colored([0.4, 1.0, 0.4, 1.0], line);
                    } else {
                        ui.text(line);
                    }
                    shown += 1;
                }
            }
        });
}

/// 取基准里某个偏移对应的值（基准是按 `偏移 / 4` 当下标存的）。
fn value_in(base: &[Option<i32>], offset: usize) -> Option<i32> {
    base.get(offset / 4).copied().flatten()
}

/// 把「实体1」当前的全部字段值存成基准。
/// 实体1 = 本次扫描的第一列（通常是列表里第一个非空槽位）。
fn baseline_from(config: &Config) -> Option<(usize, Vec<Option<i32>>)> {
    let addr = *config.field_scan_addrs.first()?;
    if config.field_rows.is_empty() {
        return None;
    }
    let values = config
        .field_rows
        .iter()
        .map(|row| row.values.first().copied().flatten())
        .collect();
    Some((addr, values))
}

/// 把读到的值格式化成表格里的一格。`None` 表示那块内存根本没读到。
fn fmt_value(value: Option<i32>) -> String {
    match value {
        Some(n) => n.to_string(),
        None => "?".to_string(),
    }
}

/// 各实体在这个偏移上的取值是不是并不完全一样（不一样才值得看）。
fn row_varies(row: &FieldRow) -> bool {
    match row.values.first() {
        Some(first) => row.values.iter().any(|v| v != first),
        None => false,
    }
}

/// 是不是「每个实体读到的都是 0~8 的小整数」。
/// 状态码（0 活 / 1 死 / 2 出生中…）正好符合这个特征，
/// 而指针、浮点数一读就是大数，会被这个条件直接筛掉。
fn row_is_small_int(row: &FieldRow) -> bool {
    row.values
        .iter()
        .all(|v| matches!(v, Some(n) if (0..=8).contains(n)))
}
