//! Length-prefixed postcard frames: a big-endian `u32` length followed by the encoded message.

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame either side accepts (1 MB).
pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the 1 MB limit")]
    TooLarge(usize),
    #[error("could not encode or decode message: {0}")]
    Codec(#[from] postcard::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Encodes `msg` into a complete frame, length prefix included.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let body = postcard::to_stdvec(msg)?;
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(body.len()));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decodes a frame body (without the length prefix).
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    Ok(postcard::from_bytes(body)?)
}

pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    w.write_all(&encode(msg)?).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one frame. Returns `Ok(None)` when the stream ends cleanly between frames.
pub async fn read_frame<R, T>(r: &mut R) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    // Grown as bytes arrive, so a peer that only announces a large frame holds no memory.
    let mut body = Vec::with_capacity(len.min(64 * 1024));
    (&mut *r).take(len as u64).read_to_end(&mut body).await?;
    if body.len() < len {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    decode(&body).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_announced_frame_that_never_arrives_is_an_error() {
        // Announces 1 MiB, sends 3 bytes and stops.
        let mut data = (MAX_FRAME as u32).to_be_bytes().to_vec();
        data.extend_from_slice(b"abc");
        let res = read_frame::<_, String>(&mut data.as_slice()).await;
        assert!(res.is_err(), "{res:?}");
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &"hello".to_string()).await.unwrap();
        let got: Option<String> = read_frame(&mut buf.as_slice()).await.unwrap();
        assert_eq!(got.as_deref(), Some("hello"));
    }
}
