//! Windows: each monitor copied with GDI (`BitBlt` from the screen into a bitmap), in
//! physical pixels: the thread is made aware of each monitor's scaling while it works.

use std::ptr::null_mut;

use pocketgpu_proto::screen::Display;
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC,
    GetDIBits, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, ReleaseDC, SRCCOPY, SelectObject,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::core::BOOL;

use super::Shot;

/// `MONITORINFOF_PRIMARY`.
const PRIMARY: u32 = 1;

struct Monitor {
    rect: RECT,
    primary: bool,
}

/// Physical pixels on this thread while it lives; the old setting comes back after.
struct Aware(isize);

impl Aware {
    fn new() -> Self {
        // SAFETY: changes this thread's DPI setting only; the old one is given back on drop.
        Self(
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
                as isize,
        )
    }
}

impl Drop for Aware {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the value SetThreadDpiAwarenessContext gave back before.
            unsafe { SetThreadDpiAwarenessContext(self.0 as _) };
        }
    }
}

unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `Vec<Monitor>` that `monitors` passed, alive for this call.
    let list = unsafe { &mut *(data as *mut Vec<Monitor>) };
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    // SAFETY: `info` is a MONITORINFO with its size set.
    if unsafe { GetMonitorInfoW(m, &mut info) } != 0 {
        list.push(Monitor {
            rect: info.rcMonitor,
            primary: info.dwFlags & PRIMARY != 0,
        });
    }
    1
}

/// Monitors, the main one first.
fn monitors() -> Vec<Monitor> {
    let mut list: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only touches `list`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            std::ptr::null(),
            Some(each),
            &mut list as *mut Vec<Monitor> as LPARAM,
        )
    };
    list.sort_by_key(|m| !m.primary);
    list
}

pub fn displays() -> Result<Vec<Display>, String> {
    let _aware = Aware::new();
    let list = monitors();
    if list.is_empty() {
        return Err("This PC has no screen on now.".into());
    }
    Ok(list
        .iter()
        .enumerate()
        .map(|(i, m)| Display {
            name: format!("Display {}", i + 1),
            width: (m.rect.right - m.rect.left) as u32,
            height: (m.rect.bottom - m.rect.top) as u32,
            primary: m.primary,
        })
        .collect())
}

pub fn grab(n: u32) -> Result<Shot, String> {
    let _aware = Aware::new();
    let m = monitors()
        .into_iter()
        .nth(n as usize)
        .ok_or_else(|| format!("there is no display {}", n + 1))?;
    let (w, h) = (m.rect.right - m.rect.left, m.rect.bottom - m.rect.top);
    if w <= 0 || h <= 0 {
        return Err("this display has no size".into());
    }
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    // SAFETY: every handle made here is released below; the bitmap info describes `bgra`
    // exactly (32 bits, top-down, w × h), so GetDIBits writes inside it.
    let copied = unsafe {
        let screen = GetDC(null_mut());
        if screen.is_null() {
            return Err("the screen can't be read now (is the PC locked?)".into());
        }
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp);
        let ok = BitBlt(
            mem,
            0,
            0,
            w,
            h,
            screen,
            m.rect.left,
            m.rect.top,
            SRCCOPY | CAPTUREBLT,
        );
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        SelectObject(mem, old);
        let lines = if ok != 0 {
            GetDIBits(
                mem,
                bmp,
                0,
                h as u32,
                bgra.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(null_mut(), screen);
        lines
    };
    if copied != h {
        return Err("the screen can't be read now (is the PC locked?)".into());
    }
    Ok(Shot {
        width: w as u32,
        height: h as u32,
        bgra,
    })
}
