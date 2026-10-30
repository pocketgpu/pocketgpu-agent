//! `pgpu/shell/1` from the phone: open or reattach a terminal and list sessions (F16).

use iroh::endpoint::{Connection, RecvStream, SendStream};
use pocketgpu_proto::shell::{SessionInfo, ToAgent, ToPhone};
use pocketgpu_proto::{alpn, read_frame, write_frame};

use crate::{ClientError, Machine};

impl Machine {
    /// Opens a new session (`session: None`) or reattaches to one. Waits for `Opened`.
    pub async fn shell(
        &self,
        cols: u16,
        rows: u16,
        session: Option<u64>,
    ) -> Result<(ShellOpened, ShellSender, ShellReceiver), ClientError> {
        let conn = self.client.dial(self.addr.clone(), alpn::SHELL).await?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        write_frame(
            &mut send,
            &ToAgent::Open {
                cols,
                rows,
                session,
            },
        )
        .await?;
        let opened = match read_frame::<_, ToPhone>(&mut recv).await? {
            Some(ToPhone::Opened {
                session,
                title,
                reattached,
            }) => ShellOpened {
                session,
                title,
                reattached,
            },
            Some(ToPhone::Error(e)) => return Err(ClientError::Remote(e)),
            _ => return Err(ClientError::NoReply),
        };
        Ok((
            opened,
            ShellSender {
                send,
                _conn: conn.clone(),
            },
            ShellReceiver { recv, _conn: conn },
        ))
    }

    /// This phone's terminal sessions on the machine.
    pub async fn shells(&self) -> Result<Vec<SessionInfo>, ClientError> {
        let conn = self.client.dial(self.addr.clone(), alpn::SHELL).await?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        write_frame(&mut send, &ToAgent::List).await?;
        match read_frame::<_, ToPhone>(&mut recv).await? {
            Some(ToPhone::Sessions(list)) => Ok(list),
            Some(ToPhone::Error(e)) => Err(ClientError::Remote(e)),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Which of `names` the machine's shell finds. Agents before 1.1.x don't know the
    /// request and close the stream: [`ClientError::NoReply`].
    pub async fn shell_tools(&self, names: Vec<String>) -> Result<Vec<String>, ClientError> {
        let conn = self.client.dial(self.addr.clone(), alpn::SHELL).await?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        write_frame(&mut send, &ToAgent::Tools(names)).await?;
        match read_frame::<_, ToPhone>(&mut recv).await? {
            Some(ToPhone::Tools(found)) => Ok(found),
            Some(ToPhone::Error(e)) => Err(ClientError::Remote(e)),
            _ => Err(ClientError::NoReply),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShellOpened {
    pub session: u64,
    pub title: String,
    pub reattached: bool,
}

/// Keystrokes and size changes to the machine. Dropping it without `close` detaches.
pub struct ShellSender {
    send: SendStream,
    _conn: Connection,
}

impl ShellSender {
    pub async fn input(&mut self, bytes: Vec<u8>) -> Result<(), ClientError> {
        Ok(write_frame(&mut self.send, &ToAgent::Input(bytes)).await?)
    }

    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<(), ClientError> {
        Ok(write_frame(&mut self.send, &ToAgent::Resize { cols, rows }).await?)
    }

    /// Ends the session: the shell is killed.
    pub async fn close(mut self) -> Result<(), ClientError> {
        write_frame(&mut self.send, &ToAgent::Close).await?;
        let _ = self.send.finish();
        Ok(())
    }
}

/// What the machine sends: output, title changes, and how the session ended.
pub struct ShellReceiver {
    recv: RecvStream,
    _conn: Connection,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShellEvent {
    Output(Vec<u8>),
    Title(String),
    Exited(Option<i32>),
}

impl ShellReceiver {
    /// The next event, or `None` when the machine ends the stream (another stream took over,
    /// or the session closed). A refusal mid-session comes back as `Remote`.
    pub async fn next(&mut self) -> Result<Option<ShellEvent>, ClientError> {
        match read_frame::<_, ToPhone>(&mut self.recv).await? {
            Some(ToPhone::Output(b)) => Ok(Some(ShellEvent::Output(b))),
            Some(ToPhone::Title(t)) => Ok(Some(ShellEvent::Title(t))),
            Some(ToPhone::Exited { code }) => Ok(Some(ShellEvent::Exited(code))),
            Some(ToPhone::Error(e)) => Err(ClientError::Remote(e)),
            Some(_) | None => Ok(None),
        }
    }
}
