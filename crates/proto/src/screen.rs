//! `pgpu/screen/1` (F45, board 59): a live picture of the machine's screen, looking only.
//! One request per bidirectional stream. `Info` gets one reply. `Peek` gets one small
//! `Frame`. `Watch` gets a `Frame` each time the screen changes, at most `fps` a second,
//! with `Same` now and then while nothing changes, until the device closes the stream or
//! someone at the machine stops it (then `Stopped`). Off until `pgpu screen on` at the
//! machine. `Info` needs View; any picture needs Shell, because a screen can show anything.

use serde::{Deserialize, Serialize};

use crate::{Error, Permission};

/// The most pictures a second a device may ask for.
pub const MAX_FPS: u8 = 8;
/// The widest picture sent; wider screens are scaled down to this.
pub const MAX_WIDTH: u32 = 1920;
/// The narrowest a device may ask for.
pub const MIN_WIDTH: u32 = 160;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Whether the screen may be seen, and its displays. Replies `Info`.
    Info,
    /// One picture of a display, at most `max_width` wide. Replies `Frame`.
    Peek { display: u32, max_width: u32 },
    /// Pictures of a display as it changes. Replies `Frame`s and `Same`s.
    Watch {
        display: u32,
        max_width: u32,
        fps: u8,
    },
}

/// One of the machine's displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Display {
    /// "Display 1", or the name the system gives it.
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    /// `pgpu screen on` at the machine.
    pub enabled: bool,
    /// Empty while off, or when the machine has no screen to show.
    pub displays: Vec<Display>,
    /// Why there is nothing to show when it's on, such as a machine with no desktop.
    pub why: Option<String>,
}

/// One picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub display: u32,
    pub width: u32,
    pub height: u32,
    /// A JPEG picture.
    pub jpeg: Vec<u8>,
    /// When it was taken, in milliseconds since 1970.
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Info(Info),
    Frame(Frame),
    /// Nothing changed since the last picture.
    Same {
        at_ms: u64,
    },
    /// Someone at the machine stopped it, or turned the screen off.
    Stopped,
    Error(Error),
}

impl Request {
    pub fn min_permission(&self) -> Permission {
        match self {
            Self::Info => Permission::View,
            Self::Peek { .. } | Self::Watch { .. } => Permission::Shell,
        }
    }
}

/// The width actually sent for a request: never wider than the display or [`MAX_WIDTH`].
pub fn width_for(asked: u32, screen: u32) -> u32 {
    asked.clamp(MIN_WIDTH, MAX_WIDTH).min(screen.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_need_shell() {
        assert_eq!(Request::Info.min_permission(), Permission::View);
        let peek = Request::Peek {
            display: 0,
            max_width: 400,
        };
        assert_eq!(peek.min_permission(), Permission::Shell);
    }

    #[test]
    fn widths_stay_in_bounds() {
        assert_eq!(width_for(1280, 2560), 1280);
        assert_eq!(width_for(9000, 5120), MAX_WIDTH);
        assert_eq!(width_for(10, 2560), MIN_WIDTH);
        assert_eq!(width_for(1280, 800), 800);
    }
}
