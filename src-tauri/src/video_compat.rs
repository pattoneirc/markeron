//! Keep the annotation overlay classified as a floating tool window (#30).
//!
//! Chromium excludes tool windows from its native occlusion calculation. Keep
//! that classification when tao rewrites styles during drawing/click-through
//! transitions. No capture, polling, GPU flags, or input changes are introduced.

use std::io;

use tauri::WebviewWindow;
use tracing::warn;
use windows_sys::Win32::{
    Foundation::{GetLastError, SetLastError, BOOL, HWND, LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_EXSTYLE, STYLESTRUCT, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_NCDESTROY, WM_STYLECHANGING,
        WS_EX_TOOLWINDOW,
    },
};

const SUBCLASS_ID: usize = 0x4d4f_0030;
type SubclassProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM, usize, usize) -> LRESULT;

// Use the common-controls subclass chain; never replace tao/WebView2's WndProc.
#[link(name = "comctl32")]
extern "system" {
    fn SetWindowSubclass(
        hwnd: HWND,
        callback: Option<SubclassProc>,
        id: usize,
        data: usize,
    ) -> BOOL;
    fn RemoveWindowSubclass(hwnd: HWND, callback: Option<SubclassProc>, id: usize) -> BOOL;
    fn DefSubclassProc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
}

unsafe extern "system" fn tool_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(tool_window_proc), SUBCLASS_ID);
        return DefSubclassProc(hwnd, message, wparam, lparam);
    }

    // Let existing handlers process the requested styles first. Only add our
    // bit afterward; preserve WS_EX_TRANSPARENT/LAYERED and every other bit.
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if message == WM_STYLECHANGING && wparam as i32 == GWL_EXSTYLE && lparam != 0 {
        let styles = &mut *(lparam as *mut STYLESTRUCT);
        styles.styleNew |= WS_EX_TOOLWINDOW;
    }
    result
}

/// Must run on the window's owning UI thread, before the overlay is shown.
unsafe fn install_tool_window(hwnd: HWND) -> io::Result<()> {
    if SetWindowSubclass(hwnd, Some(tool_window_proc), SUBCLASS_ID, 0) == 0 {
        return Err(io::Error::other("SetWindowSubclass failed"));
    }
    let before = GetWindowLongW(hwnd, GWL_EXSTYLE);
    SetLastError(0);
    let previous = SetWindowLongW(hwnd, GWL_EXSTYLE, before | WS_EX_TOOLWINDOW as i32);
    let error = GetLastError();
    if previous == 0 && error != 0 {
        RemoveWindowSubclass(hwnd, Some(tool_window_proc), SUBCLASS_ID);
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    if SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        0,
        0,
        0,
        0,
        SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
    ) == 0
    {
        let error = io::Error::last_os_error();
        RemoveWindowSubclass(hwnd, Some(tool_window_proc), SUBCLASS_ID);
        SetWindowLongW(hwnd, GWL_EXSTYLE, before);
        return Err(error);
    }
    Ok(())
}

pub fn configure_overlay(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else {
        warn!("video-compat: cannot inspect overlay HWND");
        return;
    };
    unsafe {
        if let Err(error) = install_tool_window(hwnd.0) {
            warn!(%error, "video-compat: could not configure overlay as a tool window");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_EX_LAYERED, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };

    struct TestWindow(HWND);
    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn tool_window_survives_style_rewrites_without_changing_input_flags() {
        unsafe {
            // A hidden real HWND exercises Windows' WM_STYLECHANGING delivery.
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            // TOPMOST must be set at creation or with SetWindowPos; Windows
            // ignores attempts to add it through SetWindowLongW alone.
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                0,
                0,
                32,
                32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!hwnd.is_null(), "{}", io::Error::last_os_error());
            let _window = TestWindow(hwnd);
            let original = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_eq!(original & WS_EX_TOOLWINDOW, 0);
            install_tool_window(hwnd).unwrap();
            assert_eq!(
                GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
                original | WS_EX_TOOLWINDOW
            );

            // Mimic tao: replace the complete style in each input mode.
            for requested in [
                WS_EX_TOPMOST,
                WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_LAYERED,
                WS_EX_TOPMOST,
            ] {
                SetWindowLongW(hwnd, GWL_EXSTYLE, requested as i32);
                assert_eq!(
                    GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
                    requested | WS_EX_TOOLWINDOW
                );
            }
            assert_ne!(
                RemoveWindowSubclass(hwnd, Some(tool_window_proc), SUBCLASS_ID),
                0
            );
            SetWindowLongW(hwnd, GWL_EXSTYLE, original as i32);
            assert_eq!(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32, original);
        }
    }
}
