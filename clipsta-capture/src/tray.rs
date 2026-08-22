//! Win32 system tray icon for Clipsta Capture.
//!
//! Provides a notification area icon with context menu for save operations.
//! Runs a hidden message-only window with a message pump on the calling thread.
//!
//! The tray icon handles:
//! - Context menu: Save 30s / 1min / 5min, Open Clipsta, Quit
//! - Tooltip: shows current recording state
//! - Double-click: launch the Tauri editor/library UI

use std::sync::mpsc::SyncSender;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Custom window message for tray icon callbacks.
const WM_TRAY_ICON: u32 = WM_USER + 1;

/// Menu item IDs.
const IDM_SAVE_30S: u32 = 1001;
const IDM_SAVE_1MIN: u32 = 1002;
const IDM_SAVE_5MIN: u32 = 1003;
const IDM_OPEN_CLIPSTA: u32 = 1010;
const IDM_QUIT: u32 = 1099;

/// Quit flag — set when user selects Quit from context menu.
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Thread-safe sender for save requests (seconds to save).
static SAVE_TX: OnceLock<SyncSender<u32>> = OnceLock::new();
/// Thread-safe quit sender.
static QUIT_TX: OnceLock<SyncSender<()>> = OnceLock::new();

/// System tray icon handle.
pub struct TrayIcon {
    hwnd: HWND,
    nid: NOTIFYICONDATAW,
}

impl TrayIcon {
    /// Create a new tray icon. Must be called from the thread that will pump messages.
    ///
    /// - `save_tx`: channel to send save durations (30, 60, 300 seconds)
    /// - `quit_tx`: channel to signal quit
    ///
    /// Only one TrayIcon should exist per process.
    pub fn new(save_tx: SyncSender<u32>, quit_tx: SyncSender<()>) -> Self {
        // Store senders in statics for the wndproc
        let _ = SAVE_TX.set(save_tx);
        let _ = QUIT_TX.set(quit_tx);

        let hinstance = unsafe { GetModuleHandleW(None).unwrap_or_default() };

        // Register window class
        let class_name = w!("ClipstaCaptureHiddenWnd");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        unsafe { RegisterClassExW(&wc) };

        // Create message-only window (HWND_MESSAGE parent)
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("Clipsta Capture"),
                WINDOW_STYLE::default(),
                0, 0, 0, 0,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance.into()),
                None,
            )
        }.unwrap_or_default();

        // Load icon — try exe resource first, fall back to system icon
        let icon = unsafe {
            let h = LoadImageW(
                Some(hinstance.into()),
                PCWSTR(1 as *const u16), // Resource ID 1
                IMAGE_ICON,
                16,
                16,
                LR_DEFAULTCOLOR,
            );
            match h {
                Ok(handle) => HICON(handle.0),
                Err(_) => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            }
        };

        // Create notification icon data
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY_ICON,
            hIcon: icon,
            ..Default::default()
        };

        // Set tooltip
        set_tooltip_text(&mut nid, "Clipsta \u{2014} Starting...");

        // Add tray icon
        unsafe { let _ = Shell_NotifyIconW(NIM_ADD, &nid); }

        TrayIcon { hwnd, nid }
    }

    /// Update the tooltip text (e.g., "Clipsta — Recording" or "Clipsta — Idle").
    pub fn set_tooltip(&mut self, text: &str) {
        set_tooltip_text(&mut self.nid, text);
        self.nid.uFlags = NIF_TIP;
        unsafe { let _ = Shell_NotifyIconW(NIM_MODIFY, &self.nid); }
    }

    /// Run the Win32 message pump. Blocks until WM_QUIT is received.
    /// This handles tray icon events, hotkey messages (WM_HOTKEY), and timer messages.
    pub fn run_message_loop(&self) {
        unsafe {
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                // WM_HOTKEY is posted to the thread, not a specific window
                if msg.message == WM_HOTKEY {
                    handle_hotkey(msg.wParam);
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Post WM_QUIT to break out of the message loop.
    pub fn quit(&self) {
        unsafe { let _ = PostMessageW(Some(self.hwnd), WM_QUIT, WPARAM(0), LPARAM(0)); }
    }

    /// Returns true if quit was requested via the tray menu.
    pub fn is_quit_requested() -> bool {
        QUIT_REQUESTED.load(Ordering::Relaxed)
    }

    /// Get the HWND (needed for hotkey registration on the same thread).
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn set_tooltip_text(nid: &mut NOTIFYICONDATAW, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let tip = &mut nid.szTip;
    let len = wide.len().min(tip.len() - 1);
    tip[..len].copy_from_slice(&wide[..len]);
    tip[len] = 0;
}

fn handle_hotkey(wparam: WPARAM) {
    let id = wparam.0 as u32;
    let seconds = match id {
        1 => 30,
        2 => 60,
        3 => 300,
        _ => return,
    };
    if let Some(tx) = SAVE_TX.get() {
        let _ = tx.try_send(seconds);
    }
}

fn show_context_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        let _ = AppendMenuW(menu, MF_STRING, IDM_SAVE_30S as usize, w!("Save Last 30s"));
        let _ = AppendMenuW(menu, MF_STRING, IDM_SAVE_1MIN as usize, w!("Save Last 1 Min"));
        let _ = AppendMenuW(menu, MF_STRING, IDM_SAVE_5MIN as usize, w!("Save Last 5 Min"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, IDM_OPEN_CLIPSTA as usize, w!("Open Clipsta"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, IDM_QUIT as usize, w!("Quit"));

        // Required: SetForegroundWindow before TrackPopupMenu (Win32 quirk)
        let _ = SetForegroundWindow(hwnd);

        let mut pt = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut pt);

        let _ = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );

        let _ = DestroyMenu(menu);
    }
}

fn handle_menu_command(id: u32) {
    match id {
        IDM_SAVE_30S => {
            if let Some(tx) = SAVE_TX.get() { let _ = tx.try_send(30); }
        }
        IDM_SAVE_1MIN => {
            if let Some(tx) = SAVE_TX.get() { let _ = tx.try_send(60); }
        }
        IDM_SAVE_5MIN => {
            if let Some(tx) = SAVE_TX.get() { let _ = tx.try_send(300); }
        }
        IDM_OPEN_CLIPSTA => {
            launch_tauri_app();
        }
        IDM_QUIT => {
            QUIT_REQUESTED.store(true, Ordering::Relaxed);
            if let Some(tx) = QUIT_TX.get() {
                let _ = tx.try_send(());
            }
            unsafe { PostQuitMessage(0); }
        }
        _ => {}
    }
}

/// Launch the Tauri app (clipsta.exe) if it exists next to us.
fn launch_tauri_app() {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    let Some(dir) = exe_dir else { return; };

    // Try common names for the Tauri executable
    for name in &["Clipsta.exe", "clipsta.exe", "gg.clipsta.desktop.exe"] {
        let path = dir.join(name);
        if path.exists() {
            let _ = std::process::Command::new(&path)
                .spawn();
            return;
        }
    }
}

// ── Window Procedure ─────────────────────────────────────────────────────────

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY_ICON => {
            let event = (lparam.0 & 0xFFFF) as u32;
            match event {
                // Right-click → context menu
                WM_RBUTTONUP => {
                    show_context_menu(hwnd);
                }
                // Double-click → open Clipsta
                WM_LBUTTONDBLCLK => {
                    launch_tauri_app();
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u32;
            handle_menu_command(id);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
