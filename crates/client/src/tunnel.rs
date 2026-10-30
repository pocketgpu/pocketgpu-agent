//! `pgpu/tunnel/1` from the phone: a request to the machine's Ollama (F17, F18).

use iroh::endpoint::{Connection, RecvStream};
use pocketgpu_proto::tunnel::{Method, ToAgent, ToPhone};
use pocketgpu_proto::{alpn, read_frame, write_frame};

use crate::{ClientError, Machine};

/// Largest reply `body()` collects; streams are read with `chunk()`.
const MAX_BODY: usize = 16 * 1024 * 1024;

impl Machine {
    /// Sends one request to Ollama on the machine and waits for its status. Dropping the
    /// reply cancels the request, which stops a pull.
    pub async fn ollama(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
    ) -> Result<OllamaReply, ClientError> {
        let conn = self.client.dial(self.addr.clone(), alpn::TUNNEL).await?;
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        let req = ToAgent::Request {
            method,
            path: path.into(),
            body,
        };
        write_frame(&mut send, &req).await?;
        let _ = send.finish();
        match read_frame::<_, ToPhone>(&mut recv).await? {
            Some(ToPhone::Head { status }) => Ok(OllamaReply {
                status,
                recv,
                _conn: conn,
            }),
            Some(ToPhone::Error(e)) => Err(ClientError::Remote(e)),
            _ => Err(ClientError::NoReply),
        }
    }
}

pub struct OllamaReply {
    /// Ollama's HTTP status.
    pub status: u16,
    recv: RecvStream,
    _conn: Connection,
}

impl OllamaReply {
    /// The next part of the body, or `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, ClientError> {
        match read_frame::<_, ToPhone>(&mut self.recv).await? {
            Some(ToPhone::Chunk(b)) => Ok(Some(b)),
            Some(ToPhone::End) | None => Ok(None),
            Some(ToPhone::Error(e)) => Err(ClientError::Remote(e)),
            Some(ToPhone::Head { .. }) => Err(ClientError::NoReply),
        }
    }

    /// The whole body, for short replies such as the model list.
    pub async fn body(mut self) -> Result<Vec<u8>, ClientError> {
        let mut out = Vec::new();
        while let Some(part) = self.chunk().await? {
            out.extend(part);
            if out.len() > MAX_BODY {
                return Err(ClientError::Connection("reply too large".into()));
            }
        }
        Ok(out)
    }
}
