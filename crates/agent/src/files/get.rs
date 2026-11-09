//! Sending a file to a device (F40 user story 1): `Head`, the bytes from where the device
//! stopped, and the BLAKE3 hash of the whole file. The file is opened through the root's
//! rules, or for an offer at its exact path with the size and time it was offered with.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use iroh::endpoint::SendStream;
use pocketgpu_proto::files::{CHUNK, Response, Root};
use pocketgpu_proto::{Error, ErrorCode, write_frame};
use tokio::io::AsyncReadExt;

use super::browse::{error, is_job_log, rules};
use super::offers;
use super::resolve::{self, Refused};
use super::still::Still;
use crate::audit::{self, Event};
use crate::daemon::state::Agent;
use crate::store::db::now_ms;

/// An opened file and what the audit log calls it (`output/cat.png`).
fn open(agent: &Agent, device: &str, root: Root, path: &str) -> Result<Opened, Error> {
    let refused = |e: Refused| e.to_error();
    match root {
        Root::Offers => {
            let offer = offers::find(&agent.db(), path, device, now_ms())
                .map_err(|e| error(ErrorCode::Internal, e.to_string()))?
                .ok_or_else(|| {
                    error(
                        ErrorCode::NotFound,
                        "this offer has expired or isn't for this device",
                    )
                })?;
            let (file, meta) = resolve::open_exact(&offer.path).map_err(refused)?;
            if meta.len() != offer.size || super::modified_ms(&meta) != offer.modified_ms {
                return Err(error(
                    ErrorCode::NotFound,
                    "the file changed since it was offered; offer it again with pgpu send",
                ));
            }
            Ok(Opened {
                file,
                meta,
                label: format!("Offers/{}", offer.name),
                shared: None,
            })
        }
        Root::JobLogs if !is_job_log(path) => Err(error(ErrorCode::NotFound, "not a job log")),
        _ => {
            let rules = rules(agent, root)?;
            let (file, meta) = rules.open(path).map_err(refused)?;
            let label = match root {
                Root::Shared(_) => rules
                    .root
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                Root::Inbox => "Inbox".into(),
                Root::JobLogs => "Job logs".into(),
                Root::Home | Root::Offers => "~".into(),
            };
            Ok(Opened {
                file,
                meta,
                label: format!("{label}/{path}"),
                shared: matches!(root, Root::Shared(_)).then(|| rules.root.clone()),
            })
        }
    }
}

/// A file a device asked for, opened and checked.
pub struct Opened {
    file: std::fs::File,
    meta: std::fs::Metadata,
    /// What the audit log calls it.
    label: String,
    /// The shared folder it is in, for `Root::Shared`.
    pub shared: Option<PathBuf>,
}

impl Opened {
    pub fn size(&self) -> u64 {
        self.meta.len()
    }
}

/// Opens the file of a `Get`, or says why it can't be.
pub async fn open_file(
    agent: &Arc<Agent>,
    device: &str,
    root: Root,
    path: String,
) -> Result<Opened, Error> {
    let (agent, device) = (agent.clone(), device.to_string());
    match tokio::task::spawn_blocking(move || open(&agent, &device, root, &path)).await {
        Ok(opened) => opened,
        Err(e) => Err(error(ErrorCode::Internal, e.to_string())),
    }
}

/// The rest of a `Get`: `Head`, the bytes from `from`, then `Done` with the whole file's
/// hash. Returns the reply still to send (an error), or `None` when all was sent or the
/// device left. Once the bytes start to go the get is in the audit log, however it ends.
pub async fn send_file(
    agent: &Agent,
    device: &str,
    opened: Opened,
    from: u64,
    mut still: Still,
    send: &mut SendStream,
) -> Option<Response> {
    let Opened {
        file, meta, label, ..
    } = opened;
    let size = meta.len();
    let head = Response::Head {
        size,
        modified_ms: super::modified_ms(&meta),
    };
    write_frame(send, &head).await.ok()?;
    if from > size {
        // The device holds more than the file has now: it starts again.
        return None;
    }
    let mut log = GetLog::start(agent, device, &label, from, size);
    // The hash covers the whole file, so the part the device has is read too.
    let hashed = tokio::task::spawn_blocking(move || {
        let mut file = file;
        let mut hasher = blake3::Hasher::new();
        hasher.update_reader((&mut file).take(from))?;
        Ok::<_, std::io::Error>((file, hasher))
    })
    .await;
    let (file, mut hasher) = match hashed {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return Some(log.failed(error(ErrorCode::Internal, e.to_string()))),
        Err(e) => return Some(log.failed(error(ErrorCode::Internal, e.to_string()))),
    };
    let mut file = tokio::fs::File::from_std(file);
    let mut buf = vec![0u8; CHUNK];
    while log.sent < size {
        let want = (size - log.sent).min(CHUNK as u64) as usize;
        if file.read_exact(&mut buf[..want]).await.is_err() {
            return Some(log.failed(error(
                ErrorCode::Busy,
                "the file got shorter while it was being sent; save it again",
            )));
        }
        if let Some(e) = still.stopped(agent) {
            return Some(log.failed(e));
        }
        hasher.update(&buf[..want]);
        write_frame(send, &Response::Chunk(buf[..want].to_vec()))
            .await
            .ok()?;
        log.sent += want as u64;
    }
    let done = Response::Done {
        blake3: *hasher.finalize().as_bytes(),
    };
    write_frame(send, &done).await.ok()?;
    log.ended = Some("ok".into());
    None
}

/// The audit entry of one `Get`: written `started` as the bytes start to go (which shows
/// the notice), and set to how it ended when this is dropped: `ok`, or where it stopped
/// and why. Dropped on every way out, the device leaving and the connection closing (which
/// drops the whole task) included, so no get leaves the machine unrecorded.
struct GetLog<'a> {
    agent: &'a Agent,
    id: Option<i64>,
    /// Where the device is now: `from` plus the bytes sent since.
    sent: u64,
    size: u64,
    ended: Option<String>,
}

impl<'a> GetLog<'a> {
    fn start(agent: &'a Agent, device: &str, label: &str, from: u64, size: u64) -> Self {
        let id = audit::start(&agent.db(), device, Event::FileGet, label, audit::STARTED)
            .inspect_err(|e| tracing::warn!("could not write audit entry: {e}"))
            .ok();
        Self {
            agent,
            id,
            sent: from,
            size,
            ended: None,
        }
    }

    /// Ends the entry with where it stopped and why; returns the error to send.
    fn failed(&mut self, e: Error) -> Response {
        self.ended = Some(format!("{}: {}", self.stopped(), e.message));
        Response::Error(e)
    }

    fn stopped(&self) -> String {
        format!(
            "stopped at {} of {}",
            crate::cli::files::size(self.sent),
            crate::cli::files::size(self.size)
        )
    }
}

impl Drop for GetLog<'_> {
    fn drop(&mut self) {
        let Some(id) = self.id else { return };
        let result = self.ended.take().unwrap_or_else(|| self.stopped());
        if let Err(e) = audit::finish(&self.agent.db(), id, &result) {
            tracing::warn!("could not write audit entry: {e}");
        }
    }
}
