//! 【本文件职责】本文件实现：**中文字体加载**（不做的话菜单里的中文全是 ?）。
//!
//! 中文字体加载。
//!
//! # 为什么中文会变成「?」或方块
//! imgui 自带的默认字体是 ProggyClean，它**只包含拉丁字符**，一个汉字字形都没有。
//! 要显示中文必须做两件事：
//! 1. 从系统字体目录加载一个含中文的字体文件（微软雅黑等）；
//! 2. **显式指定 glyph_ranges**（字形范围）—— 这一步最容易被漏掉，
//!    不写的话默认只烘焙 Latin 字符集，中文照样显示不出来。
//!
//! # 什么时候做
//! 字体图集（atlas）只在初始化时构建一次，之后每帧只是贴图采样。
//! 所以这件事放在 `ImguiRenderLoop::initialize` 里做（见 lib.rs）。

use imgui::{Context, FontConfig, FontGlyphRanges, FontSource};

/// UI 字号（像素）。汉字笔画密，13px 会糊成一团，18 起步比较舒服。
/// 运行时还可以用菜单里的「UI 缩放」整体放大 / 缩小。
pub const UI_FONT_SIZE: f32 = 18.0;

/// 按优先级排列的候选字体文件名（都在 `%SystemRoot%\Fonts` 下）
const FONT_FILES: &[&str] = &[
    "msyh.ttc",   // 微软雅黑（Win7 以上基本都有，首选）
    "simhei.ttf", // 黑体
    "Deng.ttf",   // 等线
    "simsun.ttc", // 宋体
    "msjh.ttc",   // 微软正黑（繁体系统）
];

/// 拼出候选字体的完整路径。
///
/// 用环境变量 `%SystemRoot%` 而不是写死 `C:\Windows` —— 系统装在 D 盘的人也有。
fn candidate_paths() -> Vec<String> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    FONT_FILES
        .iter()
        .map(|name| format!("{root}\\Fonts\\{name}"))
        .collect()
}

/// 加载中文字体。返回值表示是否成功（失败时 imgui 继续用内置字体，中文会显示异常）。
pub fn setup(ctx: &mut Context) -> bool {
    // 逐个尝试候选字体，找到第一个能读出来的就用它
    let mut loaded = None;
    for path in candidate_paths() {
        if let Ok(data) = std::fs::read(&path) {
            if !data.is_empty() {
                loaded = Some((path, data));
                break;
            }
        }
    }

    let Some((path, data)) = loaded else {
        eprintln!("[ac-esp] 警告：系统里找不到任何中文字体，UI 中文会显示成「?」");
        eprintln!("[ac-esp] 临时方案：把 menu.rs 里的中文改成英文");
        return false;
    };

    let config = FontConfig {
        size_pixels: UI_FONT_SIZE,
        // ★ 就是这一行决定中文能不能显示：
        //   chinese_simplified_common 覆盖 ASCII + 常用标点 + 约 2500 个常用汉字，
        //   足够菜单里的所有文案，图集也不会太大
        //   （chinese_full 会多出上万个字形，文件体积大很多，没必要）。
        glyph_ranges: FontGlyphRanges::chinese_simplified_common(),
        ..Default::default()
    };

    // add_font 内部会把字体数据复制一份交给 atlas 管理，
    // 所以 `data` 出了函数作用域被释放也没问题，不需要 'static。
    ctx.fonts().add_font(&[FontSource::TtfData {
        data: &data,
        size_pixels: UI_FONT_SIZE,
        config: Some(config),
    }]);

    eprintln!("[ac-esp] 已加载中文字体: {path}");
    true
}
