//! 【本文件职责】本文件实现：**热键轮询** + 把热键从游戏那里吞掉的窗口过程。
//!
//! 热键：检测按键 + 让热键**不泄漏给游戏**。
//!
//! # 一、检测热键
//! 用 `GetAsyncKeyState`：它读的是**全局**按键状态，和窗口消息无关，
//! 所以哪怕我们下面把按键从游戏那里吞掉了，这里照样能检测到。
//!
//! # 二、为什么还要"吞键"
//! hudhook 的输入过滤（`MessageFilter`）只能按**消息类别**拦，
//! 比如"所有键盘消息都吞掉"，它没法只拦某一个按键。
//! 结果就是：你按 Insert 开菜单，游戏同时也收到了 Insert，
//! 而游戏恰好给这个键绑了动作，于是莫名其妙触发了游戏功能。
//!
//! 解决办法：在 hudhook 装的窗口过程**外面再包一层**，形成调用链：
//!
//! ```text
//!   [我们的过程] ──(是热键? 返回 0，到此为止)──> ✗ 游戏收不到
//!          └────(其他消息)────> [hudhook 的过程] ──> [游戏原本的过程]
//! ```
//!
//! 只吃掉我们自己的两个热键，其他消息一律原样往下传。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_DELETE, VK_END, VK_F2, VK_F7, VK_F8, VK_HOME, VK_INSERT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, GWLP_WNDPROC, SetWindowLongPtrW, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// 窗口过程函数的类型签名（Windows 的消息处理函数都长这样）
type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// 可选热键。
///
/// 挑键原则：尽量避开游戏自己会响应的键。
/// AssaultCube 默认会用到 TAB（计分板）、ESC（菜单）、T/Y（聊天）、
/// F1（投票）、0-9（选武器）、WASD（移动），所以候选集中在这几个上。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hotkey {
    Insert,
    Delete,
    Home,
    End,
    F2,
    F7,
    F8,
}

impl Hotkey {
    pub const ALL: [Hotkey; 7] = [
        Hotkey::Insert,
        Hotkey::Delete,
        Hotkey::Home,
        Hotkey::End,
        Hotkey::F2,
        Hotkey::F7,
        Hotkey::F8,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Hotkey::Insert => "Insert",
            Hotkey::Delete => "Delete",
            Hotkey::Home => "Home",
            Hotkey::End => "End",
            Hotkey::F2 => "F2",
            Hotkey::F7 => "F7",
            Hotkey::F8 => "F8",
        }
    }

    /// 对应的虚拟键码（VK_*），Windows 用它来标识按键
    pub fn vk(self) -> i32 {
        match self {
            Hotkey::Insert => VK_INSERT as i32,
            Hotkey::Delete => VK_DELETE as i32,
            Hotkey::Home => VK_HOME as i32,
            Hotkey::End => VK_END as i32,
            Hotkey::F2 => VK_F2 as i32,
            Hotkey::F7 => VK_F7 as i32,
            Hotkey::F8 => VK_F8 as i32,
        }
    }

    /// 枚举 ↔ 菜单下拉框的序号转换
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Hotkey::Insert)
    }
}

// ---------------------------------------------------------------------------
// 全局状态。
// 窗口过程跑在游戏的 UI 线程上，这里只用原子变量：不加锁、不分配内存，
// 避免在窗口过程里死锁（在窗口过程里等锁 = 画面直接卡死）。
// ---------------------------------------------------------------------------

/// 是否要吞掉热键
static SWALLOW_ENABLED: AtomicBool = AtomicBool::new(true);
/// 要吞掉的第一个键（菜单键）
static SWALLOW_VK_A: AtomicI32 = AtomicI32::new(VK_INSERT as i32);
/// 要吞掉的第二个键（ESP 开关）
static SWALLOW_VK_B: AtomicI32 = AtomicI32::new(VK_F2 as i32);
/// 要吞掉的第三个键（自瞄开关）
static SWALLOW_VK_C: AtomicI32 = AtomicI32::new(VK_F7 as i32);
/// 上一层窗口过程，也就是 hudhook 装的那个
static PREV_WNDPROC: AtomicUsize = AtomicUsize::new(0);
/// 已经挂到哪个窗口上（用来判断游戏是否重建了窗口）
static INSTALLED_HWND: AtomicUsize = AtomicUsize::new(0);

/// 由渲染线程每帧同步一次（这些值在菜单里可以改）
pub fn set_swallow(enabled: bool, menu_key: Hotkey, esp_key: Hotkey, aim_key: Hotkey) {
    SWALLOW_ENABLED.store(enabled, Ordering::Relaxed);
    SWALLOW_VK_A.store(menu_key.vk(), Ordering::Relaxed);
    SWALLOW_VK_B.store(esp_key.vk(), Ordering::Relaxed);
    SWALLOW_VK_C.store(aim_key.vk(), Ordering::Relaxed);
}

/// 按键边沿检测：只有「这一帧刚刚按下」才返回 true。
///
/// 没有这个的话，按住键不动会每帧翻转一次，开关会疯狂抖动（菜单闪烁）。
/// `was_down` 是调用方保存的"上一帧状态"，函数会顺便把它更新掉。
pub fn pressed(hotkey: Hotkey, was_down: &mut bool) -> bool {
    // GetAsyncKeyState 返回值的最高位（0x8000）表示"现在是否按下"
    let is_down = unsafe { (GetAsyncKeyState(hotkey.vk()) as u16 & 0x8000) != 0 };
    let pressed = is_down && !*was_down;
    *was_down = is_down;
    pressed
}

/// 安装"吞热键"的窗口过程。
///
/// 必须在 hudhook **之后**调用（hudhook 建立管线时才会给窗口装自己的过程），
/// 所以我们在第一次 `render()` 里调用它 —— 那时 hudhook 的过程已经就位。
/// 每帧调用一次的开销只有一个原子比较；游戏重建窗口时会自动重新安装。
pub fn install(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    if INSTALLED_HWND.load(Ordering::Relaxed) == hwnd as usize {
        return; // 已经装过了
    }

    // 32 位下 SetWindowLongPtrW 就是 SetWindowLongW 的别名，参数和返回值都是 i32；
    // 64 位下才是 isize。所以要按目标架构分开写，否则编不过（最终目标是 i686）。
    let new_proc: usize = swallow_wnd_proc as *const () as usize;

    #[cfg(target_pointer_width = "32")]
    let prev: usize = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, new_proc as i32) as usize };
    #[cfg(target_pointer_width = "64")]
    let prev: usize = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, new_proc as isize) as usize };

    if prev == 0 {
        return; // 安装失败（句柄无效），下次再来
    }

    PREV_WNDPROC.store(prev, Ordering::Relaxed);
    INSTALLED_HWND.store(hwnd as usize, Ordering::Relaxed);
}

/// 我们自己的窗口过程：只负责把热键吃掉，其他消息原样转给上一级。
///
/// 这个函数运行在游戏的 UI 线程上，**绝对不能 panic**
/// （跨 FFI 边界 panic 会直接把整个进程干掉），所以里面只有原子读取和整数比较。
unsafe extern "system" fn swallow_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let is_key_message = matches!(msg, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP);

    if is_key_message && SWALLOW_ENABLED.load(Ordering::Relaxed) {
        // wparam 低字节就是虚拟键码
        let vk = (wparam as u32 & 0xFF) as i32;
        if vk == SWALLOW_VK_A.load(Ordering::Relaxed)
            || vk == SWALLOW_VK_B.load(Ordering::Relaxed)
            || vk == SWALLOW_VK_C.load(Ordering::Relaxed)
        {
            // ★ 返回 0 表示"这条消息我处理完了"：
            //   既不传给 hudhook，也不传给游戏 —— 游戏根本不知道你按了 Insert
            return 0;
        }
    }

    // 不是热键 → 交给上一层（hudhook 的过程，它会再往下传给游戏）
    let prev = PREV_WNDPROC.load(Ordering::Relaxed);
    if prev == 0 {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    let prev: WndProc = unsafe { std::mem::transmute(prev) };
    unsafe { CallWindowProcW(Some(prev), hwnd, msg, wparam, lparam) }
}
