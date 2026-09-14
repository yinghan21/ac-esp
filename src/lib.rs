//! 【本文件职责】本文件实现：**DllMain 入口 + 逐帧调度**，把下面所有模块串起来。
//!
//! AssaultCube ESP（简化学习版）—— 程序入口。
//!
//! # 一、整体是怎么跑起来的
//!
//! ```text
//!  游戏加载我们的 dll
//!    └─ DllMain(DLL_PROCESS_ATTACH)
//!         └─ 另开一个线程 → run()          （不能在 DllMain 里做重活，见文末注释）
//!              ├─ 1. 取游戏模块基址          （所有静态偏移都以它为基准）
//!              ├─ 2. 起「数据线程」          （读游戏内存 → 写进共享快照）
//!              └─ 3. hudhook 挂钩 wglSwapBuffers
//!                       └─ 每帧回调 render()
//!                            ├─ 处理热键
//!                            ├─ esp::draw()   画方框 / 引导线 / 血条
//!                            └─ menu::draw()  画设置菜单
//! ```
//!
//! # 二、三条必须记住的规则
//!
//! 1. **渲染回调跑在游戏线程上**。里面只做「画」，任何耗时操作（读内存、算距离）
//!    都放在数据线程，否则会拖慢游戏帧率。
//! 2. **读游戏内存前必须先检查这块内存可不可读**（见 `memory::read`）。
//!    直接解引用野指针会让游戏崩溃。
//! 3. **ESP 不用 imgui 窗口**，用的是 draw list（见 `esp.rs` 顶部注释）。
//!
//! # 三、建议的阅读顺序
//!
//! `offset.rs` → `memory.rs` → `data.rs` → `esp.rs` → `config.rs` → `menu.rs`
//! → `hotkeys.rs` → `fonts.rs` → 最后回到本文件看全局串起来。

mod aimbot;
mod config;
mod data;
mod esp;
mod fonts;
mod hotkeys;
mod memory;
mod menu;
mod offset;

use std::ffi::c_void;
use std::sync::{Arc, RwLock};

use hudhook::hooks::opengl3::ImguiOpenGl3Hooks;
use hudhook::{Hudhook, ImguiRenderLoop, MessageFilter, RenderContext};
use imgui::{Context, Io, Ui};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::Win32::UI::WindowsAndMessaging::ClipCursor;

use crate::config::Config;
use crate::data::Snapshot;

/// 渲染循环：hudhook 每帧会调用它的几个方法，我们在这里画东西。
struct EspRenderLoop {
    /// 界面上所有开关和参数
    config: Config,
    /// 和数据线程共享的数据快照（数据线程写，我们读）
    snapshot: Arc<RwLock<Snapshot>>,
    /// 两个热键「上一帧是否按下」，用来做边沿检测（见 poll_hotkeys）
    prev_menu_key_down: bool,
    prev_esp_key_down: bool,
    prev_aim_key_down: bool,
}

impl EspRenderLoop {
    fn new(snapshot: Arc<RwLock<Snapshot>>) -> Self {
        Self {
            config: Config::default(),
            snapshot,
            prev_menu_key_down: false,
            prev_esp_key_down: false,
            prev_aim_key_down: false,
        }
    }

    /// 处理热键。
    ///
    /// 两件事要注意：
    /// * 必须**每帧**都调用（哪怕菜单没开），否则会漏掉按键状态；
    /// * 不能只看「有没有按下」，否则按住键不动会每秒切换 60 次，菜单疯狂闪烁。
    ///   所以要靠 `hotkeys::pressed` 做「上升沿」检测：只在这一帧刚按下时返回 true。
    fn poll_hotkeys(&mut self) {
        if hotkeys::pressed(self.config.menu_key, &mut self.prev_menu_key_down) {
            self.config.menu_open = !self.config.menu_open;
        }
        if hotkeys::pressed(self.config.esp_key, &mut self.prev_esp_key_down) {
            // 不开菜单也能一把关掉 / 打开 ESP
            self.config.esp_enabled = !self.config.esp_enabled;
        }
        if hotkeys::pressed(self.config.aim_key, &mut self.prev_aim_key_down) {
            // 自瞄默认关着（注入即动鼠标太危险），用热键手动开关
            self.config.aim_enabled = !self.config.aim_enabled;
        }
    }
}

/// hudhook 每帧按固定顺序回调下面这几个方法，顺序记住就不会用错位置：
///
/// ```text
/// initialize()     只调一次，而且是在构建字体图集之前 → 加载字体必须放这里
/// before_render()  每帧，在 imgui「开始新一帧」之前   → 改 io.display_size 放这里
/// render(&mut ui)  每帧，真正画东西；&mut Ui 只能在这里用
/// message_filter() 每帧，决定哪些 Windows 消息不转发给游戏
/// ```
impl ImguiRenderLoop for EspRenderLoop {
    /// 只调用一次的初始化钩子。
    ///
    /// 在这里加载中文字体最合适：hudhook 会把 initialize 安排在构建字体图集之前，
    /// 我们加进去的字体才会被真正「烘焙」进图集。
    fn initialize<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        // 返回「是否加载成功」，这里不关心（失败时 fonts.rs 内部已经打印了警告），
        // 但显式写 let _ = 说明我们知道它有返回值，而不是不小心忽略了。
        let _ = fonts::setup(ctx);
    }

    /// 每帧渲染前：改 imgui 全局设置的正确位置。
    fn before_render<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        // 先做 Win32 查询，拿到结果再动 ctx：避免和 ctx 的可变借用缠在一起过不了借用检查
        let client_size = memory::game_client_size();

        let io = ctx.io_mut();

        // 让 imgui 自己画鼠标光标。
        // 第一人称射击游戏默认会隐藏并锁定系统光标，不打开这一项，
        // 菜单里的开关你根本点不到。只在菜单打开时画，关掉马上还给游戏。
        io.mouse_draw_cursor = self.config.menu_open;

        // UI 整体缩放（菜单里可调）
        io.font_global_scale = self.config.ui_scale;

        // 每帧校正分辨率：hudhook 只在建立管线时读一次窗口尺寸，
        // 之后你切分辨率 / 全屏 / 拉窗口它都不会更新，
        // 不校正的话菜单会被拉伸、ESP 坐标整体错位（越靠边缘偏得越多）。
        if let Some((width, height)) = client_size {
            if io.display_size[0] != width || io.display_size[1] != height {
                io.display_size = [width, height];
            }
        }

        // 【鼠标"点不动"的坑】
        // AssaultCube（SDL）会调用 ClipCursor 把光标锁在窗口里，甚至吸到屏幕中心，
        // 于是 imgui 画的光标和真实光标对不上，表现就是「菜单点不中」。
        // 菜单打开期间每帧解除一次剪裁（游戏下一帧可能又锁回去，所以要每帧做）。
        if self.config.menu_open && self.config.release_cursor {
            let _ = unsafe { ClipCursor(std::ptr::null()) };
        }
    }

    /// 每帧绘制。注意这里**不要**自己调 ctx.frame()，帧已经被 hudhook 开好了。
    fn render(&mut self, ui: &mut Ui) {
        // 1) 热键拦截的准备：
        //    必须等 hudhook 建好管线（它会给窗口装自己的过程）之后再包一层，
        //    而 render() 只在管线建好后才被调用，所以时机正好。
        if let Some(hwnd) = memory::game_hwnd() {
            hotkeys::install(hwnd);
        }
        hotkeys::set_swallow(
            self.config.swallow_hotkeys,
            self.config.menu_key,
            self.config.esp_key,
            self.config.aim_key,
        );
        // 把菜单里选的实体列表布局告诉数据线程（它在另一个线程里，只能这样传）
        data::set_layout(self.config.entity_layout);
        // 同理：调参用的状态字段偏移也要传过去
        data::set_state_offset(self.config.state_offset);

        // 2) 先处理热键，这样同一帧里开关的状态立刻生效
        self.poll_hotkeys();

        // 3) 拿一份数据快照（只读锁，数据线程那段临界区非常短）。
        //    如果数据线程 panic 过导致锁「中毒」，这里也照样能读，不要跟着一起炸。
        let snapshot = match self.snapshot.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        // 4) 先画 ESP，再画菜单 —— 后画的压在上面，
        //    这样菜单永远盖住方框，开关不会被方框糊住。
        esp::draw(ui, &self.config, &snapshot);
        // 5) 自瞄放在最后：它会发 SendInput，绝不能在菜单开着时抢鼠标（内部有判断）
        aimbot::run(ui, &self.config, &snapshot);
        menu::draw(ui, &mut self.config, &snapshot);
    }

    /// 输入过滤：决定哪些 Windows 消息**不**转发给游戏。
    ///
    /// 【关键点：不要「菜单开着就全拦」】
    /// 那样做的结果是菜单一开，键鼠全被吞掉，游戏完全操作不了。
    /// 正确做法是用 imgui 自己的「想不想要输入」来判断：
    /// * `io.want_capture_mouse`：鼠标正悬停在 imgui 窗口上（或在拖控件）→ 才拦鼠标；
    /// * `io.want_capture_keyboard`：正在编辑控件 → 才拦键盘。
    /// 其余时间输入照常发给游戏，这就是所谓的「鼠标穿透」。
    ///
    /// 另外这个过滤**管不到单个按键**，所以热键要用 `hotkeys` 里的窗口过程单独吞掉。
    ///
    /// 细节：`want_capture_*` 是 imgui 在上一帧算出来的结果，所以这里的判断天然滞后一帧，
    /// 实践中完全感知不到。
    fn message_filter(&self, io: &Io) -> MessageFilter {
        // 菜单没开：一个字都不拦，输入全部交给游戏
        if !self.config.menu_open {
            return MessageFilter::empty();
        }

        let mut filter = MessageFilter::empty();
        if io.want_capture_mouse {
            // InputRaw 一起拦：有些游戏用 WM_INPUT（Raw Input）读鼠标，
            // 只拦 WM_MOUSEMOVE 是拦不住的。
            filter |= MessageFilter::InputMouse | MessageFilter::InputRaw;
        }
        if io.want_capture_keyboard {
            filter |= MessageFilter::InputKeyboard;
        }
        filter
    }
}

/// 真正的初始化逻辑，跑在独立线程里。
fn run(hmodule: *mut c_void) -> anyhow::Result<()> {
    // 1) 模块基址：所有静态偏移的基准
    let module_base = memory::module_base().ok_or_else(|| anyhow::anyhow!("GetModuleHandleA 失败"))?;

    // 2) 共享快照 + 数据线程（读内存这种慢活全交给它）
    let snapshot = Arc::new(RwLock::new(Snapshot::default()));
    data::spawn(module_base, Arc::clone(&snapshot));

    // 3) 安装钩子。AssaultCube 是 OpenGL 渲染（Cube 引擎 + SDL），
    //    所以挂的是 opengl32.dll 的 wglSwapBuffers，而不是 D3D 的 Present。
    //    挂错渲染后端是「dll 注入成功但屏幕上什么都没有」最常见的原因。
    let render_loop = EspRenderLoop::new(snapshot);

    Hudhook::builder()
        .with::<ImguiOpenGl3Hooks>(render_loop)
        .with_hmodule(hudhook::windows::Win32::Foundation::HINSTANCE(hmodule))
        .build()
        .apply()
        .map_err(|e| anyhow::anyhow!("hudhook 挂钩失败: {e:?}"))?;

    Ok(())
}

/// dll 入口。Windows 加载我们的 dll 时会调用它。
///
/// 注意判的是 `DLL_PROCESS_ATTACH`，不是 `DLL_THREAD_ATTACH`
/// （后者每创建一个线程都会触发一次，会把钩子装很多遍）。
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
extern "system" fn DllMain(module: *mut c_void, call_reason: u32, _: *mut c_void) -> bool {
    if call_reason == DLL_PROCESS_ATTACH {
        // 在 DllMain 里做重活会踩 Windows 的 loader lock，可能直接死锁，
        // 所以真正的初始化丢到新线程里。
        // 裸指针不是 Send，先转成 usize 再 move 进线程，进去再转回指针。
        let module_raw = module as usize;
        std::thread::spawn(move || {
            if let Err(err) = run(module_raw as *mut c_void) {
                // 失败原因留在调试输出里，方便用 DebugView / 调试器查看
                eprintln!("[ac-esp] 初始化失败: {err:?}");
            }
        });
    }

    true
}
