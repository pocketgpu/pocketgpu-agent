//! Sharing a model (F48): make a share, list them and stop one. All on the control
//! connection and all Shell; the link is built here from the machine's key.

use pocketgpu_proto::msg::control::{Request, Response};
use pocketgpu_proto::share::{NewShare, ShareInfo};

use crate::{ClientError, Machine};

impl Machine {
    /// A new share; its `link` holds what [`ShareInfo::link`] needs for the URL.
    pub async fn create_share(&self, new: NewShare) -> Result<ShareInfo, ClientError> {
        match self.control(Request::CreateShare(new)).await? {
            Response::ShareMade(s) => Ok(s),
            _ => Err(ClientError::NoReply),
        }
    }

    pub async fn shares(&self) -> Result<Vec<ShareInfo>, ClientError> {
        match self.control(Request::ListShares).await? {
            Response::Shares(s) => Ok(s),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Ends a share at once; the shares after.
    pub async fn stop_share(&self, share: u64) -> Result<Vec<ShareInfo>, ClientError> {
        match self.control(Request::StopShare { share }).await? {
            Response::Shares(s) => Ok(s),
            _ => Err(ClientError::NoReply),
        }
    }

    /// The guest page's URL for a share that waits to be opened.
    pub fn share_url(&self, share: &ShareInfo) -> Option<String> {
        let parts = share.link.as_ref()?;
        Some(parts.link(*self.addr.id.as_bytes()).url())
    }
}
