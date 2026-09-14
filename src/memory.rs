//! 【本文件职责】本文件提供：**安全读内存** + **世界坐标转屏幕** + 窗口尺寸，整个项目的地基。
//!
//! 底层工具：数学类型、**带安全检查的内存读取**、世界坐标转屏幕坐标、窗口尺寸。
//!
//! 这个文件是整个项目的"地基"，其他地方都要用：
//! * `read` / `is_readable`  —— 读游戏内存的唯一入口（保证不崩游戏）
//! * `world_to_screen`       —— 3D 世界坐标 → 2D 屏幕像素（ESP 的核心算法）
//! * `game_client_size`      —— 每帧校正 imgui 的画面尺寸

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowA, GetClientRect, IsWindow};
use windows_sys::core::s;

// ---------------------------------------------------------------------------
// 数学类型
// ---------------------------------------------------------------------------

/// 3D 世界坐标（游戏里的位置）。
///
/// `#[repr(C)]` 很关键：我们会用 `read::<Vec3>(addr)` 整块读内存，
/// 只有 repr(C) 才能保证内存里就是 x、y、z 依次紧挨着排（和游戏里的结构体一致）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    /// 两点距离。AC 里 1 单位不是 1 米，所以这里算出来的是「游戏单位」。
    pub fn distance(&self, other: &Vec3) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

/// 2D 屏幕坐标（像素），原点在屏幕左上角，x 向右、y 向下。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// 转成 imgui draw list 要的数组格式
    pub fn to_array(self) -> [f32; 2] {
        [self.x, self.y]
    }
}

// ---------------------------------------------------------------------------
// 内存读取
// ---------------------------------------------------------------------------

/// 取当前进程主模块（AssaultCube.exe）的基址。
///
/// 我们是被注入进游戏进程的，所以 `GetModuleHandleA(null)` 拿到的就是游戏自己，
/// 用它 + 偏移才能定位到玩家数组、视图矩阵这些东西。
pub fn module_base() -> Option<usize> {
    let handle = unsafe { GetModuleHandleA(std::ptr::null()) };
    if handle.is_null() {
        None
    } else {
        Some(handle as usize)
    }
}

/// 判断 `[addr, addr + size)` 这段内存是否真的可读。
///
/// # 为什么必须有这个函数
/// 直接 `*(0xdeadbeef as *const i32)` 会真的触发 ACCESS_VIOLATION，
/// 结果是**游戏直接崩溃**（外挂把游戏搞崩是最尴尬的 bug）。
/// 玩家退出、换地图的那一瞬间，实体指针就是野指针。
/// 所以每次读写前先用 VirtualQuery 问系统这块页的属性，不可读就当作读取失败。
///
/// # 注意它是"尽力而为"
/// 检查完到真正读取之间，那一页理论上仍可能被游戏释放（多线程的经典 TOCTOU 竞争），
/// 只是这个窗口只有几十纳秒、概率极低。它真正挡住的是常态问题：
/// 玩家退了、数组越界、偏移填错读到未映射区域 —— 这三类是 99% 的崩溃来源。
pub fn is_readable(addr: usize, size: usize) -> bool {
    if addr == 0 || size == 0 {
        return false;
    }

    let Some(end) = addr.checked_add(size) else {
        return false;
    };

    let mut current = addr;
    // 一次 VirtualQuery 只能问一段连续的、属性相同的区域，所以这里循环着问
    let mut mbi = unsafe { std::mem::zeroed::<MEMORY_BASIC_INFORMATION>() };
    let mbi_size = size_of::<MEMORY_BASIC_INFORMATION>();

    while current < end {
        let written = unsafe { VirtualQuery(current as *const c_void, &mut mbi, mbi_size) };
        if written == 0 {
            // 查询失败：这个地址根本不在当前进程的地址空间里
            return false;
        }
        if mbi.State != MEM_COMMIT {
            // 只是保留 / 已释放，没有真正映射物理页
            return false;
        }
        if mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 {
            // PAGE_GUARD 是栈保护页，踩上去一样会崩
            return false;
        }

        let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
        if region_end <= current {
            // 防御：区域不前进就退出，避免死循环
            return false;
        }
        current = region_end;
    }

    true
}

/// 通用读取：先检查可读性，再按类型 `T` 解释这段内存。读不到就返回 `None`。
///
/// 用 `read_unaligned` 而不是 `*ptr`：游戏里的字段不保证满足 Rust 的对齐要求，
/// 直接解引用属于 UB（现在没事，开了优化就可能出玄学 bug）。
pub fn read<T: Copy>(addr: usize) -> Option<T> {
    if !is_readable(addr, size_of::<T>()) {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(addr as *const T) })
}

// ---------------------------------------------------------------------------
// 世界坐标 → 屏幕坐标
// ---------------------------------------------------------------------------

/// 把游戏里的 3D 坐标投影成屏幕上的 2D 像素。
///
/// # 原理
/// 视图矩阵是列主序的 4x4，等价于 `clip = matrix * vec4(world, 1.0)`：
/// ```text
/// clip.x = x*m[0] + y*m[4] + z*m[8]  + m[12]
/// clip.y = x*m[1] + y*m[5] + z*m[9]  + m[13]
/// clip.w = x*m[3] + y*m[7] + z*m[11] + m[15]
/// ```
/// 然后做**透视除法**（除以 w）得到 NDC（范围 -1..1），
/// 最后把 -1..1 映射到 0..宽 / 0..高 的像素坐标。
///
/// # 返回 None 的三种情况
/// 点在摄像机背后（w <= 0）、w 太小（会除爆）、结果不是有限数。
/// 调用方拿到 None 就跳过这个玩家，避免画出「穿屏」的鬼线。
pub fn world_to_screen(world: Vec3, matrix: &[f32; 16], width: f32, height: f32) -> Option<Vec2> {
    let m = matrix;

    let w = world.x * m[3] + world.y * m[7] + world.z * m[11] + m[15];
    if w < 0.001 || !w.is_finite() {
        return None;
    }

    let x = world.x * m[0] + world.y * m[4] + world.z * m[8] + m[12];
    let y = world.x * m[1] + world.y * m[5] + world.z * m[9] + m[13];

    let ndc_x = x / w;
    let ndc_y = y / w;
    if !ndc_x.is_finite() || !ndc_y.is_finite() {
        return None;
    }

    let center_x = width * 0.5;
    let center_y = height * 0.5;

    Some(Vec2::new(
        center_x + ndc_x * center_x,
        // 屏幕 y 轴向下，NDC 的 y 轴向上，所以这里是减
        center_y - ndc_y * center_y,
    ))
}

// ---------------------------------------------------------------------------
// 窗口
// ---------------------------------------------------------------------------

/// 找到游戏主窗口。
///
/// 优先用 hudhook 提供的「枚举本进程的顶层窗口」：不依赖窗口标题，
/// 比 `FindWindowA(null, "AssaultCube")` 可靠（标题一改就找不到，
/// 注入太早、窗口还没建好时也会找不到）。
fn find_game_window() -> Option<HWND> {
    if let Some(hwnd) = hudhook::hooks::find_process_hwnd() {
        return Some(hwnd.0 as HWND);
    }

    let hwnd = unsafe { FindWindowA(std::ptr::null(), s!("AssaultCube")) };
    if hwnd.is_null() { None } else { Some(hwnd) }
}

/// 缓存下来的游戏窗口句柄（枚举窗口比较慢，不该每帧都做）
static GAME_HWND: OnceLock<isize> = OnceLock::new();

/// 取（并缓存）游戏窗口句柄。热键拦截那边也用它。
pub fn game_hwnd() -> Option<HWND> {
    if let Some(&raw) = GAME_HWND.get() {
        let hwnd = raw as HWND;
        if unsafe { IsWindow(hwnd) } != 0 {
            return Some(hwnd);
        }
        // 句柄失效（游戏重建了窗口）→ 继续往下重新找一次
    }

    let hwnd = find_game_window()?;
    let _ = GAME_HWND.set(hwnd as isize);
    Some(hwnd)
}

/// 取窗口**客户区**尺寸，用来每帧校正 imgui 的 `display_size`。
///
/// 注意用 `GetClientRect` 而不是 `GetWindowRect`：后者包含标题栏和边框，
/// 宽度会多出十几个像素，算出来的坐标整体偏移，
/// 表现就是「方框总是比人物偏一点」，窗口不是全屏时尤其明显。
///
/// 【为什么需要它】hudhook 只在建立管线时读一次窗口尺寸，之后你在游戏里
/// 切分辨率 / 全屏 / 拉窗口它都不知道，imgui 的画面就会拉伸变形。
pub fn game_client_size() -> Option<(f32, f32)> {
    let hwnd = game_hwnd()?;
    let mut rect = RECT::default();
    let ok = unsafe { GetClientRect(hwnd, &mut rect) };
    if ok == 0 {
        return None;
    }

    let w = (rect.right - rect.left) as f32;
    let h = (rect.bottom - rect.top) as f32;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some((w, h))
}
