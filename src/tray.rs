//! System tray icon. Closing the window (X) hides Spoty to the tray; the tray icon reopens it,
//! and its menu can quit for real. Done directly with Win32: the window layer exits on close
//! and can't cancel it, so the window's close message is intercepted before it gets there.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, SetWindowSubclass, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Message the tray icon sends to the window.
const WM_TRAY: u32 = WM_APP + 1;
const MENU_OPEN: usize = 1;
const MENU_QUIT: usize = 2;

static QUITTING: AtomicBool = AtomicBool::new(false);
/// Follows the "close button hides to tray" preference.
pub static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(true);
static HINT_SHOWN: AtomicBool = AtomicBool::new(false);

/// Adds the tray icon and takes over the close button. Call from the UI thread once the window exists.
pub fn install() -> bool {
    let hwnd = find_own_window();
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        SetWindowSubclass(hwnd, Some(window_proc), 1, 0);
    }
    add_icon(hwnd);
    true
}

/// Hides Spoty's window to the tray (used when starting minimized).
pub fn hide_own_window() {
    let hwnd = find_own_window();
    if !hwnd.is_null() {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
}

/// Shows and focuses a window, un-minimizing it if needed.
pub fn bring_to_front(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

/// The top-level window created by this (UI) thread.
fn find_own_window() -> HWND {
    unsafe extern "system" fn pick(hwnd: HWND, out: LPARAM) -> i32 {
        unsafe {
            if GetParent(hwnd).is_null() && GetWindowTextLengthW(hwnd) > 0 {
                *(out as *mut HWND) = hwnd;
                return 0;
            }
        }
        1
    }
    let mut found: HWND = std::ptr::null_mut();
    unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(pick), &mut found as *mut HWND as LPARAM) };
    found
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Sent to every window when Explorer restarts and rebuilds the tray.
fn taskbar_created() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) })
}

fn copy_wide(dst: &mut [u16], text: &str) {
    for (d, c) in dst.iter_mut().zip(text.encode_utf16().chain(Some(0))) {
        *d = c;
    }
}

fn add_icon(hwnd: HWND) {
    let mut nid = icon_data(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = make_icon();
    copy_wide(&mut nid.szTip, "Spoty");
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
}

fn remove_icon(hwnd: HWND) {
    let nid = icon_data(hwnd);
    unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

/// The first time Spoty hides, say where it went.
fn show_hint(hwnd: HWND) {
    if HINT_SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut nid = icon_data(hwnd);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    copy_wide(&mut nid.szInfoTitle, "Spoty is still running");
    copy_wide(&mut nid.szInfo, "It keeps playing in the tray. Right-click the icon to quit.");
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

fn quit(hwnd: HWND) {
    QUITTING.store(true, Ordering::SeqCst);
    remove_icon(hwnd);
    unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu();
        let (open, exit) = (wide("Open Spoty"), wide("Quit"));
        AppendMenuW(menu, MF_STRING, MENU_OPEN, open.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_QUIT, exit.as_ptr());
        SetMenuDefaultItem(menu, MENU_OPEN as u32, 0);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
        DestroyMenu(menu);
        match picked as usize {
            MENU_OPEN => bring_to_front(hwnd),
            MENU_QUIT => quit(hwnd),
            _ => {}
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_CLOSE if !QUITTING.load(Ordering::SeqCst) && CLOSE_TO_TRAY.load(Ordering::SeqCst) => {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            show_hint(hwnd);
            return 0;
        }
        WM_TRAY => {
            match (lparam & 0xffff) as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => bring_to_front(hwnd),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            return 0;
        }
        WM_DESTROY => remove_icon(hwnd),
        // Explorer restarted: the tray was rebuilt without our icon.
        m if m != 0 && m == taskbar_created() => add_icon(hwnd),
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Spoty's icon as RGBA pixels, scaled to `size`.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    static PNG: &[u8] = include_bytes!("../assets/icon.png");
    match image::load_from_memory(PNG) {
        Ok(img) => img.resize_exact(size, size, image::imageops::FilterType::Lanczos3).to_rgba8().into_raw(),
        Err(_) => vec![0; (size * size * 4) as usize],
    }
}

fn make_icon() -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
    // Windows wants premultiplied BGRA here.
    let bgra: Vec<u8> = icon_rgba(size)
        .chunks(4)
        .flat_map(|p| {
            let a = p[3] as u32;
            let pm = |c: u8| (c as u32 * a / 255) as u8;
            [pm(p[2]), pm(p[1]), pm(p[0]), p[3]]
        })
        .collect();
    let mask = vec![0u8; (size * size / 8) as usize];
    unsafe {
        let color = CreateBitmap(size as i32, size as i32, 1, 32, bgra.as_ptr().cast());
        let mask_bmp = CreateBitmap(size as i32, size as i32, 1, 1, mask.as_ptr().cast());
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask_bmp, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask_bmp);
        icon
    }
}
