//! A picture to send: scaled to the width asked for and made a JPEG small enough for one
//! frame.

use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, Rgb, imageops};
use pocketgpu_proto::frame::MAX_FRAME;

use super::capture::Shot;

/// Under the frame limit with room for the rest of the message.
const MOST: usize = MAX_FRAME - 16 * 1024;

/// The JPEG of `shot` at most `width` wide, and its size.
pub fn jpeg(shot: &Shot, width: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_raw(shot.width, shot.height, shot.rgb())
            .ok_or("the picture has the wrong size")?;
    let img = if width < shot.width {
        let height = ((shot.height as u64 * width as u64) / shot.width as u64).max(1) as u32;
        imageops::thumbnail(&img, width, height)
    } else {
        img
    };
    let (w, h) = img.dimensions();
    for quality in [72, 55, 40, 25] {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&img)
            .map_err(|e| e.to_string())?;
        if out.len() <= MOST {
            return Ok((out, w, h));
        }
    }
    Err("the picture is too big to send".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_down_and_keeps_the_shape() {
        let shot = Shot {
            width: 400,
            height: 200,
            bgra: vec![90; 400 * 200 * 4],
        };
        let (bytes, w, h) = jpeg(&shot, 200).unwrap();
        assert_eq!((w, h), (200, 100));
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        let (_, w, _) = jpeg(&shot, 1000).unwrap();
        assert_eq!(w, 400);
    }
}
