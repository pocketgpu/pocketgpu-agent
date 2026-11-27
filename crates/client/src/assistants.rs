//! AI assistants (F32): held actions answered from the phone, the connected assistants and
//! their settings, and what happened. All on the control connection; everything but
//! `assistants` needs Shell, and the agent checks it again on every request.

use pocketgpu_proto::assistants::{ApprovalView, AssistantSettings, AssistantView};
use pocketgpu_proto::local::AuditEntry;
use pocketgpu_proto::msg::control::{Request, Response};

use crate::{ClientError, Machine};

/// The AI assistants screen's top: who is connected, and the settings for a Shell device
/// (`None` for View and Control).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assistants {
    pub connected: Vec<AssistantView>,
    pub settings: Option<AssistantSettings>,
}

impl Machine {
    /// Open and recently ended requests.
    pub async fn list_approvals(&self) -> Result<Vec<ApprovalView>, ClientError> {
        match self.control(Request::ListApprovals).await? {
            Response::Approvals(list) => Ok(list),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Allows or denies one held action; `opened_ms` must be the one the request showed.
    /// The request after, or `Remote` when it was already answered or is no longer open.
    pub async fn answer_approval(
        &self,
        id: u64,
        opened_ms: i64,
        allow: bool,
    ) -> Result<ApprovalView, ClientError> {
        let req = Request::AnswerApproval {
            id,
            opened_ms,
            allow,
        };
        match self.control(req).await? {
            Response::ApprovalAnswered(view) => Ok(*view),
            _ => Err(ClientError::NoReply),
        }
    }

    pub async fn assistants(&self) -> Result<Assistants, ClientError> {
        into_assistants(self.control(Request::GetAssistants).await?)
    }

    /// Saves the `[assistants]` settings; what the agent holds after.
    pub async fn set_assistant_settings(
        &self,
        settings: AssistantSettings,
    ) -> Result<Assistants, ClientError> {
        into_assistants(
            self.control(Request::SetAssistantSettings(settings))
                .await?,
        )
    }

    /// The `assistant_*` audit entries since then, newest first, at most 200.
    pub async fn assistant_activity(&self, since_ms: i64) -> Result<Vec<AuditEntry>, ClientError> {
        match self
            .control(Request::AssistantActivity { since_ms })
            .await?
        {
            Response::Activity(list) => Ok(list),
            _ => Err(ClientError::NoReply),
        }
    }
}

fn into_assistants(resp: Response) -> Result<Assistants, ClientError> {
    match resp {
        Response::Assistants {
            connected,
            settings,
        } => Ok(Assistants {
            connected,
            settings,
        }),
        _ => Err(ClientError::NoReply),
    }
}
