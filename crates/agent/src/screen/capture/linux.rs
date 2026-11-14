//! Linux: the X server's root window, read over its socket (no system libraries). One
//! display: the whole desktop. Wayland desktops don't let other programs read the screen
//! this way, and a machine with no desktop has nothing to show.

use pocketgpu_proto::screen::Display;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};

use super::Shot;

fn why_not() -> String {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("DISPLAY").is_none() {
        "This desktop (Wayland) can't be shown yet.".into()
    } else {
        "There's no desktop on this machine to show.".into()
    }
}

fn root() -> Result<(x11rb::rust_connection::RustConnection, u32, u32, u32), String> {
    if std::env::var_os("DISPLAY").is_none() {
        return Err(why_not());
    }
    let (conn, n) = x11rb::connect(None).map_err(|_| why_not())?;
    let s = &conn.setup().roots[n];
    let (root, w, h) = (s.root, s.width_in_pixels as u32, s.height_in_pixels as u32);
    Ok((conn, root, w, h))
}

pub fn displays() -> Result<Vec<Display>, String> {
    let (_, _, width, height) = root()?;
    Ok(vec![Display {
        name: "Display 1".into(),
        width,
        height,
        primary: true,
    }])
}

pub fn grab(n: u32) -> Result<Shot, String> {
    if n != 0 {
        return Err(format!("there is no display {}", n + 1));
    }
    let (conn, root, width, height) = root()?;
    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            root,
            0,
            0,
            width as u16,
            height as u16,
            !0,
        )
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("the desktop didn't give a picture: {e}"))?;
    let want = (width * height * 4) as usize;
    if reply.data.len() < want {
        return Err("this desktop's colours can't be shown yet".into());
    }
    let mut bgra = reply.data;
    bgra.truncate(want);
    Ok(Shot {
        width,
        height,
        bgra,
    })
}
