//! Lending a machine for a while (F44): invites, the guests list and more time. All on the
//! control connection; making invites and giving time need Shell, and guests can't.

use pocketgpu_proto::Permission;
use pocketgpu_proto::msg::control::{Guest, Request, Response};

use crate::{ClientError, Machine};

/// A new invite: the `pocketgpu://pair?…` link (show it as a QR code or send it) and when
/// the guest's access would end (Unix ms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub uri: String,
    pub ends_ms: i64,
}

impl Machine {
    pub async fn create_invite(
        &self,
        permission: Permission,
        minutes: u32,
    ) -> Result<Invite, ClientError> {
        match self
            .control(Request::CreateInvite {
                permission,
                minutes,
            })
            .await?
        {
            Response::Invite { uri, ends_ms } => Ok(Invite { uri, ends_ms }),
            _ => Err(ClientError::NoReply),
        }
    }

    pub async fn guests(&self) -> Result<Vec<Guest>, ClientError> {
        match self.control(Request::ListGuests).await? {
            Response::Guests(g) => Ok(g),
            _ => Err(ClientError::NoReply),
        }
    }

    /// More time for a guest, from its current end; the guests after.
    pub async fn extend_guest(
        &self,
        device: &str,
        minutes: u32,
    ) -> Result<Vec<Guest>, ClientError> {
        let req = Request::ExtendGuest {
            device: device.into(),
            minutes,
        };
        match self.control(req).await? {
            Response::Guests(g) => Ok(g),
            _ => Err(ClientError::NoReply),
        }
    }
}
