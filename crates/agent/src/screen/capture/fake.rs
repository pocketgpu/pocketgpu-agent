//! Made-up displays (`PGPU_FAKE_SCREEN=1`): a dark desktop with a bar that moves once a
//! second, so watching shows change and a still second shows none.

use pocketgpu_proto::screen::Display;

use super::Shot;

const SIZES: [(u32, u32); 2] = [(1280, 720), (1024, 768)];

pub fn displays() -> Vec<Display> {
    SIZES
        .iter()
        .enumerate()
        .map(|(i, (w, h))| Display {
            name: format!("Display {}", i + 1),
            width: *w,
            height: *h,
            primary: i == 0,
        })
        .collect()
}

pub fn grab(n: u32) -> Result<Shot, String> {
    let (width, height) = *SIZES
        .get(n as usize)
        .ok_or_else(|| format!("there is no display {}", n + 1))?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let bar = (secs % 10) as u32 * width / 10;
    let mut bgra = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let on_bar = x >= bar && x < bar + width / 10 && y > height / 3 && y < height / 2;
            let px = if on_bar {
                [0x60, 0xD7, 0x1E, 0]
            } else {
                [
                    (40 + y * 60 / height) as u8,
                    30,
                    (30 + x * 50 / width) as u8,
                    0,
                ]
            };
            bgra.extend_from_slice(&px);
        }
    }
    Ok(Shot {
        width,
        height,
        bgra,
    })
}
