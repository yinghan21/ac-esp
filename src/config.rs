//! 【本文件职责】本文件定义：**所有功能开关和参数**（菜单里能改的东西都在这里）。
//!
//! 配置：所有「开关」和参数都集中在这里，方便菜单直接改。
//!
//! 为什么单独抽一个文件：
//! * 菜单（menu.rs）只需要「改字段」，绘制（esp.rs）只需要「读字段」，
//!   两边不用互相知道对方的存在；
//! * 想加一个新功能，改这一个文件和对应绘制代码就行。

use crate::data::FieldRow;
use crate::hotkeys::Hotkey;

/// 引导线（俗称"拉线"）的起点。
///
/// 为什么要拉线：敌人在屏幕边缘或者很远的时候，单看一个方框很难判断方向，
/// 一条从固定点连过去的线能立刻告诉你「人在那边」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapOrigin {
    /// 屏幕底部正中（最常用）
    BottomCenter,
    /// 屏幕正中
    Center,
    /// 屏幕顶部正中
    TopCenter,
}

impl SnapOrigin {
    /// 菜单下拉框里的顺序
    pub const ALL: [SnapOrigin; 3] = [
        SnapOrigin::BottomCenter,
        SnapOrigin::Center,
        SnapOrigin::TopCenter,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SnapOrigin::BottomCenter => "底部中间",
            SnapOrigin::Center => "屏幕中心",
            SnapOrigin::TopCenter => "顶部中间",
        }
    }

    /// 下面这对 index/from_index 是给 imgui 下拉框用的惯例写法：
    /// imgui 只认识「第几项」，我们自己负责在序号和枚举之间转换。
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|o| *o == self).unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL
            .get(index)
            .copied()
            .unwrap_or(SnapOrigin::BottomCenter)
    }

    /// 算出起点的屏幕像素坐标。`offset` 可以把起点左右平移，
    /// 比如不想让它压到准星或血条上。
    pub fn point(self, width: f32, height: f32, offset: f32) -> [f32; 2] {
        match self {
            SnapOrigin::BottomCenter => [width * 0.5 + offset, height],
            SnapOrigin::Center => [width * 0.5 + offset, height * 0.5],
            SnapOrigin::TopCenter => [width * 0.5 + offset, 0.0],
        }
    }
}

/// 引导线连到玩家身上的哪个点
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapTarget {
    Head,
    Feet,
}

impl SnapTarget {
    pub const ALL: [SnapTarget; 2] = [SnapTarget::Head, SnapTarget::Feet];

    pub fn label(self) -> &'static str {
        match self {
            SnapTarget::Head => "头部",
            SnapTarget::Feet => "脚部",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|o| *o == self).unwrap_or(1)
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(SnapTarget::Feet)
    }
}

/// 实体列表的内存布局（**识别不到敌人时第一个要试的开关**）。
///
/// 从 `基址 + ENTITY_LIST` 开始可能是两种东西，差别只在于"要不要多解一次引用"：
/// * 内联数组：这个地址本身就是 `entity*[]` 的起点；
/// * 指针数组：这个地址里存着一个指针，那个指针才指向真正的数组
///   （`std::vector<entity*>` 的 begin 就是这样）。
///
/// 默认「自动检测」：两种布局各抽样几个槽位，谁读出来的东西更像玩家就用谁。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntityLayout {
    Auto,
    /// 数组就在 ENTITY_LIST 处
    Inline,
    /// ENTITY_LIST 处存的是数组指针
    Pointer,
}

impl EntityLayout {
    pub const ALL: [EntityLayout; 3] =
        [EntityLayout::Auto, EntityLayout::Inline, EntityLayout::Pointer];

    pub fn label(self) -> &'static str {
        match self {
            EntityLayout::Auto => "自动检测",
            EntityLayout::Inline => "内联数组",
            EntityLayout::Pointer => "指针数组",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|o| *o == self).unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(EntityLayout::Auto)
    }
}

/// 全局配置。默认值 = 一进游戏就能看到 ESP，方便调试。
#[derive(Clone, Debug)]
pub struct Config {
    // -------- 总开关 --------
    /// 菜单窗口是否显示（Insert 键切换）
    pub menu_open: bool,
    /// ESP 总开关（F2 可以不开菜单直接切）。关掉后所有绘制都停。
    pub esp_enabled: bool,

    // -------- 热键 --------
    pub menu_key: Hotkey,
    pub esp_key: Hotkey,
    /// 把热键从游戏那里"吃掉"，否则按热键的同时游戏也会收到（强烈建议开启）
    pub swallow_hotkeys: bool,

    // -------- 画什么 --------
    pub draw_box: bool,
    pub draw_snapline: bool,
    pub draw_health_bar: bool,

    // -------- 引导线参数 --------
    pub snap_origin: SnapOrigin,
    pub snap_target: SnapTarget,
    /// 起点左右平移（像素）
    pub snap_offset: f32,
    pub snap_thickness: f32,

    // -------- 方框参数 --------
    pub box_thickness: f32,
    /// 方框宽度 = 人物屏幕高度 × 这个比例（正常人身高:肩宽 ≈ 2.2:1）
    pub box_width_ratio: f32,

    // -------- 颜色（imgui 用的是 0..1 的 RGBA） --------
    pub color_box: [f32; 4],
    pub color_line: [f32; 4],

    // -------- 过滤 --------
    /// 血量 <= 0 的不画（需要血量偏移正确）
    pub ignore_dead: bool,
    /// 不画自己（需要 LOCAL_PLAYER 偏移；没配置时这项自动失效，不会误判）
    pub ignore_self: bool,

    // -------- 自瞄（aimbot.rs） --------
    /// 自瞄总开关（热键直接切）。默认关着：注入即动鼠标太危险。
    pub aim_enabled: bool,
    /// 开关自瞄的热键
    pub aim_key: Hotkey,
    /// FOV 半径（屏幕像素）：只瞄「投影点落在准星周围这个圈内」的敌人。
    /// 圈太小没目标，圈太大像锁头 —— 调出你想要的"手感"即可。
    pub aim_fov: f32,
    /// 平滑：每帧移动量除以它，1 = 瞬吸，越大越像人手
    pub aim_smooth: f32,
    /// 速度倍率：像素差 × 它 = 实际鼠标移动量（对应游戏灵敏度，偏了就调它）
    pub aim_speed: f32,
    /// 瞄身上哪个点（复用引导线的 Head/Feet 枚举）
    pub aim_target: SnapTarget,
    /// 把 FOV 圈画出来（调参时非常有用，平时可以关）
    pub draw_fov_circle: bool,
    pub color_fov: [f32; 4],
    /// 长按右键才瞄（hold 模式）：开启后自瞄只在**按住鼠标右键**时生效，
    /// 松开立刻停。比热键切换更不容易误触，也更符合 FPS 的「开镜瞄准」习惯。
    pub aim_hold_mode: bool,

    // -------- 血条 --------
    /// 血量上限，用来算血条比例（AC 默认 100）
    pub max_health: f32,
    pub health_bar_width: f32,

    // -------- 界面 --------
    /// 菜单打开时解除游戏对光标的锁定（鼠标点不中菜单时留着勾上）
    pub release_cursor: bool,
    /// UI 整体缩放
    pub ui_scale: f32,

    // -------- 数据读取 --------
    /// 实体列表布局（自动检测不准时在菜单里手动切）
    pub entity_layout: EntityLayout,
    /// 【调参用】临时覆盖 `offset.rs` 里的 STATE 偏移，0 表示不用。
    /// 用途：用菜单滑块现场找出“死亡时会变成 1”的那个字段，
    /// 找出来之后把它写进 `offset.rs`，这里再改回 0。
    pub state_offset: i32,

    // -------- 调试：字段扫描 --------
    /// 扫描结果。平时是空的，只有你在菜单里点一次「扫描实体字段」才填。
    /// 放在 config 里是因为它本质上是「界面的临时状态」，跟游戏逻辑无关。
    pub field_rows: Vec<FieldRow>,
    /// 只显示各实体取值不同的行（不然 256 行看不过来）
    pub field_scan_diff_only: bool,
    /// 只显示「所有实体取值都是 0~8 的小整数」的行。
    /// 状态码就是这种小整数，这个开关能一下子把噪声（指针、浮点）滤掉。
    pub field_scan_small_int_only: bool,
    /// 本次扫描用到的实体地址（表格每一列对应的实体），用来显示表头
    pub field_scan_addrs: Vec<usize>,
    /// 【基准】某个实体在某个时刻的全部字段值，用来做「死后对比」。
    /// 存的是 `(实体地址, 每个偏移的值)`。
    pub field_baseline: Option<(usize, Vec<Option<i32>>)>,
    /// 只显示「和基准不一样」的行（找“死亡时变化的字段”最有效的办法）
    pub field_scan_use_baseline: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 注入后先弹出菜单，方便确认钩子是否挂上了
            menu_open: true,
            esp_enabled: true,

            menu_key: Hotkey::Insert,
            esp_key: Hotkey::F2,
            swallow_hotkeys: true,

            draw_box: true,
            draw_snapline: true,
            draw_health_bar: true,

            snap_origin: SnapOrigin::BottomCenter,
            snap_target: SnapTarget::Feet,
            snap_offset: 0.0,
            snap_thickness: 1.6,

            box_thickness: 1.6,
            box_width_ratio: 0.45,

            color_box: [1.0, 0.25, 0.25, 1.0],
            color_line: [1.0, 0.4, 0.4, 0.85],

            ignore_dead: true,
            ignore_self: true,

            // 自瞄默认全关：注入后不按键就不会动鼠标
            aim_enabled: false,
            aim_key: Hotkey::F7,
            aim_fov: 150.0,
            aim_smooth: 2.0,
            aim_speed: 1.0,
            aim_target: SnapTarget::Head,
            draw_fov_circle: true,
            color_fov: [0.3, 0.9, 1.0, 0.5],
            aim_hold_mode: false,

            max_health: 100.0,
            health_bar_width: 4.0,

            release_cursor: true,
            ui_scale: 1.0,

            entity_layout: EntityLayout::Auto,
            state_offset: 0,

            field_rows: Vec::new(),
            field_scan_diff_only: true,
            field_scan_small_int_only: false,
            field_scan_addrs: Vec::new(),
            field_baseline: None,
            field_scan_use_baseline: false,
        }
    }
}
