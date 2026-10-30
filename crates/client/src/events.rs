//! `pgpu/events/1`: alerts as they fire on a machine, while the app is open (F31).

use iroh::endpoint::{Connection, RecvStream, SendStream};
use pocketgpu_proto::alpn;
use pocketgpu_proto::msg::events::{Alert, Subscribe};
use pocketgpu_proto::{read_frame, write_frame};

use crate::{ClientError, Machine};

impl Machine {
    /// Alerts from this machine until the returned stream is dropped. Older agents don't serve
    /// the protocol and refuse the connection.
    pub async fn events(&self) -> Result<EventStream, ClientError> {
        let conn = self.client.dial(self.addr.clone(), alpn::EVENTS).await?;
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        write_frame(&mut send, &Subscribe).await?;
        Ok(EventStream {
            _conn: conn,
            _send: send,
            recv,
        })
    }
}

pub struct EventStream {
    _conn: Connection,
    _send: SendStream,
    recv: RecvStream,
}

impl EventStream {
    /// The next alert, or `None` when the machine ends the stream.
    pub async fn next(&mut self) -> Result<Option<Alert>, ClientError> {
        Ok(read_frame(&mut self.recv).await?)
    }
}
