//! `pgpu/screen/1` (F45): a live picture of the machine's screen, looking only. Pictures
//! need Full control (Shell) and the screen turned on at the machine.

use iroh::endpoint::{RecvStream, SendStream};
use pocketgpu_proto::screen::{Frame, Info, Request, Response};
use pocketgpu_proto::{alpn, read_frame, write_frame};

use crate::{ClientError, Machine, request};

/// What a look sends.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    Frame(Frame),
    /// Nothing changed.
    Same {
        at_ms: u64,
    },
    /// Someone at the machine stopped it, or turned the screen off. The look is over.
    Stopped,
}

/// A look at a display. Dropping it ends the look.
pub struct Watch {
    _send: SendStream,
    recv: RecvStream,
}

impl Watch {
    /// The next picture or note; `None` when the look is over.
    pub async fn next(&mut self) -> Result<Option<Seen>, ClientError> {
        match read_frame::<_, Response>(&mut self.recv).await? {
            None => Ok(None),
            Some(Response::Frame(f)) => Ok(Some(Seen::Frame(f))),
            Some(Response::Same { at_ms }) => Ok(Some(Seen::Same { at_ms })),
            Some(Response::Stopped) => Ok(Some(Seen::Stopped)),
            Some(Response::Error(e)) => Err(ClientError::Remote(e)),
            Some(Response::Info(_)) => Err(ClientError::NoReply),
        }
    }
}

impl Machine {
    /// Whether the screen may be seen, and its displays.
    pub async fn screen_info(&self) -> Result<Info, ClientError> {
        let conn = self.screen.get(self, alpn::SCREEN).await?;
        match request(&conn, &Request::Info).await? {
            Response::Info(i) => Ok(i),
            Response::Error(e) => Err(ClientError::Remote(e)),
            _ => Err(ClientError::NoReply),
        }
    }

    /// One picture of a display, at most `max_width` wide.
    pub async fn screen_peek(&self, display: u32, max_width: u32) -> Result<Frame, ClientError> {
        let conn = self.screen.get(self, alpn::SCREEN).await?;
        match request(&conn, &Request::Peek { display, max_width }).await? {
            Response::Frame(f) => Ok(f),
            Response::Error(e) => Err(ClientError::Remote(e)),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Starts looking at a display: pictures as it changes, at most `fps` a second.
    pub async fn screen_watch(
        &self,
        display: u32,
        max_width: u32,
        fps: u8,
    ) -> Result<Watch, ClientError> {
        let conn = self.screen.get(self, alpn::SCREEN).await?;
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        let req = Request::Watch {
            display,
            max_width,
            fps,
        };
        write_frame(&mut send, &req).await?;
        Ok(Watch { _send: send, recv })
    }
}
