//! `pgpu/docker/1`: a machine's Docker containers (F28).

use iroh::endpoint::{Connection, RecvStream};
use pocketgpu_proto::alpn;
use pocketgpu_proto::docker::{Action, Container, LogLine, Request, Response};
use pocketgpu_proto::{read_frame, write_frame};

use crate::{ClientError, Machine, request};

impl Machine {
    /// Every container, running ones first.
    pub async fn containers(&self) -> Result<Vec<Container>, ClientError> {
        match self.docker(&Request::List).await? {
            Response::Containers(list) => Ok(list),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Start, stop or restart a container (needs Control).
    pub async fn container_action(&self, id: &str, action: Action) -> Result<(), ClientError> {
        let req = Request::Act {
            id: id.into(),
            action,
        };
        self.docker(&req).await.map(|_| ())
    }

    /// The last `tail` lines of a container's log, then new ones until it stops.
    pub async fn container_logs(&self, id: &str, tail: u32) -> Result<ContainerLog, ClientError> {
        let conn = self.docker.get(self, alpn::DOCKER).await?;
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        let req = Request::Logs {
            id: id.into(),
            tail,
        };
        write_frame(&mut send, &req).await?;
        send.finish()
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        Ok(ContainerLog { _conn: conn, recv })
    }

    async fn docker(&self, req: &Request) -> Result<Response, ClientError> {
        let conn = self.docker.get(self, alpn::DOCKER).await?;
        match request(&conn, req).await? {
            Response::Error(e) => Err(ClientError::Remote(e)),
            other => Ok(other),
        }
    }
}

pub struct ContainerLog {
    _conn: Connection,
    recv: RecvStream,
}

impl ContainerLog {
    /// The next lines, or `None` when the log ends (the container stopped).
    pub async fn next(&mut self) -> Result<Option<Vec<LogLine>>, ClientError> {
        match read_frame::<_, Response>(&mut self.recv).await? {
            Some(Response::Log(lines)) => Ok(Some(lines)),
            Some(Response::Error(e)) => Err(ClientError::Remote(e)),
            Some(_) => Err(ClientError::NoReply),
            None => Ok(None),
        }
    }
}
