//! Types shared by several protocols.

use serde::{Deserialize, Serialize};

/// What a paired phone may do. Ordered: each level includes the ones below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// Stats, jobs and alerts.
    View,
    /// Also quick actions, kill and stopping jobs.
    Control,
    /// Also creating and editing actions (and, from 1.1, the terminal).
    Shell,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Control => "control",
            Self::Shell => "shell",
        }
    }
}

impl std::str::FromStr for Permission {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "view" => Ok(Self::View),
            "control" => Ok(Self::Control),
            "shell" => Ok(Self::Shell),
            _ => Err(format!(
                "unknown permission {s:?}; use view, control or shell"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    Ios,
    Android,
    Desktop,
    /// A platform added by a newer app version.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    Forbidden,
    NotFound,
    Invalid,
    Busy,
    Internal,
    Unknown,
}

/// Error reply for any request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn forbidden(needed: Permission) -> Self {
        Self::new(
            ErrorCode::Forbidden,
            format!("needs {} permission", needed.as_str()),
        )
    }
}

/// Static facts about one GPU.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub index: u32,
    pub name: String,
    pub vram_total_mb: u64,
    pub driver: Option<String>,
}

/// One reading of one GPU. Fields a GPU doesn't report are `None`, never zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuSample {
    pub index: u32,
    pub util_pct: Option<u8>,
    pub vram_used_mb: Option<u64>,
    pub temp_c: Option<u8>,
    pub power_w: Option<u32>,
    pub fan_pct: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskSample {
    pub mount: String,
    pub used_gb: f32,
    pub total_gb: f32,
}

/// Everything the phone shows live, sent once a second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Unix time in milliseconds on the machine.
    pub at_ms: u64,
    pub gpus: Vec<GpuSample>,
    pub cpu_pct: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub disks: Vec<DiskSample>,
    pub ollama_up: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineInfo {
    pub name: String,
    pub os: String,
    pub agent_version: String,
    pub gpus: Vec<GpuInfo>,
    /// The asking phone's own permission.
    pub permission: Permission,
}
