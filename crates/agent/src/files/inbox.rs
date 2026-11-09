//! Files sent to this machine (F40 user story 2, FR-004, FR-005). The bytes go to a private
//! partial file, `<inbox>/.partial/<device>-<transfer>-<size>.part`, so a send that stops
//! carries on later (with another size it starts over). Once all are there the device sends
//! the file's BLAKE3 hash, and only when the bytes here have it are they linked into the
//! Inbox under a name nothing else has (`photo (2).jpg`), never over another file. A `.done`
//! note in place of the partial keeps that name for a day, so a device that missed `Saved`
//! gets it again instead of sending the file twice. Room is checked by [`super::room`];
//! unfinished sends are removed after a day.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use iroh::PublicKey;
use iroh::endpoint::{RecvStream, SendStream};
use pocketgpu_proto::files::{CHUNK, Response, Upload, clean_name};
use pocketgpu_proto::{Error, ErrorCode, read_frame, write_frame};
use tokio::io::AsyncWriteExt;

use super::place::{mark_downloaded, place, read_note, write_note};
use super::room::{self, Reservation};
use super::still::Still;
use crate::audit::{self, Event};
use crate::daemon::state::Agent;

/// Unfinished sends and notes of finished ones older than this are removed.
pub const PARTIAL_AGE: Duration = Duration::from_secs(24 * 3600);
pub const PARTIAL_DIR: &str = ".partial";

/// What the device hears when the bytes here are not its file.
const CHANGED: &str = "the file changed while it was being sent, so it wasn't saved; send it again";

/// `<first 8 of the device key>-<transfer in hex>`: one send, whatever its size.
pub fn transfer_key(device: &str, transfer: &[u8; 16]) -> String {
    let hex: String = transfer.iter().map(|b| format!("{b:02x}")).collect();
    let prefix: String = device
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    format!("{prefix}-{hex}")
}

/// `<transfer key>-<size>.part`.
pub fn partial_name(device: &str, transfer: &[u8; 16], size: u64) -> String {
    format!("{}-{size}.part", transfer_key(device, transfer))
}

/// The transfer key in the name of a partial or a note.
fn key_of(file_name: &str) -> Option<&str> {
    let stem = file_name
        .strip_suffix(".part")
        .or_else(|| file_name.strip_suffix(".done"))?;
    Some(stem.rsplit_once('-')?.0)
}

/// Sends being written or placed now, by transfer key: one stream per send at a time.
static WRITING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

struct Writing(String);

impl Writing {
    fn take(key: &str) -> Option<Self> {
        let mut set = WRITING.lock().expect("partials lock poisoned");
        set.insert(key.to_string()).then(|| Self(key.to_string()))
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        WRITING
            .lock()
            .expect("partials lock poisoned")
            .remove(&self.0);
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Error(Error::new(code, message))
}

/// What a `Put` asked for.
#[derive(Debug, Clone)]
pub struct Sent {
    pub name: String,
    pub size: u64,
    pub transfer: [u8; 16],
}

/// Where a `Put` starts.
enum Prepared {
    /// The bytes go to this partial, which holds `have` already.
    Write {
        inbox: PathBuf,
        path: PathBuf,
        file: std::fs::File,
        have: u64,
        room: Reservation,
    },
    /// Saved before; the device missed the reply.
    Saved {
        name: String,
        blake3: [u8; 32],
        note: PathBuf,
    },
}

/// A `Put`: `Ready`, the device's chunks and hash, then `Saved` (returned), or `None` when
/// the device stopped sending (the partial stays for it to carry on).
pub async fn put(
    agent: &Arc<Agent>,
    key: PublicKey,
    sent: Sent,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Option<Response> {
    let device = key.to_string();
    let Sent {
        name,
        size,
        transfer,
    } = sent;
    let Some(writing) = Writing::take(&transfer_key(&device, &transfer)) else {
        return Some(error(
            ErrorCode::Busy,
            "this file is being sent already; wait for that to finish",
        ));
    };
    let partial = partial_name(&device, &transfer, size);
    let prepared = {
        let agent = agent.clone();
        tokio::task::spawn_blocking(move || prepare(&agent, &partial, size)).await
    };
    let (inbox, path, file, mut have, room) = match prepared {
        Ok(Ok(Prepared::Write {
            inbox,
            path,
            file,
            have,
            room,
        })) => (inbox, path, file, have, room),
        Ok(Ok(Prepared::Saved { name, blake3, note })) => {
            return saved_again(send, recv, size, (name, blake3), &note).await;
        }
        Ok(Err(e)) => return Some(Response::Error(e)),
        Err(e) => return Some(error(ErrorCode::Internal, e.to_string())),
    };
    let mut still = Still::put(key);
    let mut file = tokio::fs::File::from_std(file);
    write_frame(send, &Response::Ready { have }).await.ok()?;
    let mut unchecked = 0u64;
    let expected = loop {
        let chunk = match read_frame::<_, Upload>(recv).await {
            Ok(Some(Upload::Chunk(c))) => c,
            Ok(Some(Upload::Done { blake3 })) if have == size => break blake3,
            Ok(Some(Upload::Done { .. })) => {
                return Some(error(
                    ErrorCode::Invalid,
                    format!("the send ended at {have} of the {size} bytes it started with"),
                ));
            }
            Ok(None) => return None,
            Err(e) => return Some(error(ErrorCode::Invalid, e.to_string())),
        };
        if chunk.len() > CHUNK {
            return Some(error(ErrorCode::Invalid, "a chunk is bigger than 256 KB"));
        }
        if have + chunk.len() as u64 > size {
            return Some(error(
                ErrorCode::Invalid,
                format!("more bytes than the {size} the send started with"),
            ));
        }
        if let Some(e) = still.stopped(agent) {
            return Some(Response::Error(e));
        }
        if unchecked >= room::RECHECK {
            if let Err(e) = room::still_room(room::free_space(&inbox)) {
                return Some(Response::Error(e));
            }
            unchecked = 0;
        }
        if let Err(e) = file.write_all(&chunk).await {
            return Some(error(ErrorCode::Internal, format!("could not save: {e}")));
        }
        have += chunk.len() as u64;
        unchecked += chunk.len() as u64;
        room.wrote(chunk.len() as u64);
    };
    if let Err(e) = async {
        file.flush().await?;
        file.sync_all().await
    }
    .await
    {
        return Some(error(ErrorCode::Internal, format!("could not save: {e}")));
    }
    drop((file, room));
    let wanted = name.clone();
    let placed = tokio::task::spawn_blocking(move || {
        // Held until the file is in place, also when the device leaves meanwhile, so a
        // second try of this send waits for this one and then finds its note.
        let _writing = writing;
        finish(&path, &inbox, &wanted, size, &expected)
    })
    .await;
    let (saved, blake3) = match placed {
        Ok(Ok(Some(done))) => done,
        Ok(Ok(None)) => {
            let target = clean_name(&name);
            let result = "not saved: the bytes differ from the device's file";
            if let Err(e) = audit::record(&agent.db(), &device, Event::FilePut, &target, result) {
                tracing::warn!("could not write audit entry: {e}");
            }
            return Some(error(ErrorCode::Invalid, CHANGED));
        }
        Ok(Err(e)) => return Some(error(ErrorCode::Internal, format!("could not save: {e}"))),
        Err(e) => return Some(error(ErrorCode::Internal, e.to_string())),
    };
    if let Err(e) = audit::record(&agent.db(), &device, Event::FilePut, &saved, "ok") {
        tracing::warn!("could not write audit entry: {e}");
    }
    Some(Response::Saved {
        name: saved,
        blake3,
    })
}

/// A send saved before whose `Saved` the device missed: it hears it again once its hash
/// says it is the same file, and nothing is saved twice. Another file drops the note, so
/// the next try starts over.
async fn saved_again(
    send: &mut SendStream,
    recv: &mut RecvStream,
    size: u64,
    (name, blake3): (String, [u8; 32]),
    note: &Path,
) -> Option<Response> {
    write_frame(send, &Response::Ready { have: size })
        .await
        .ok()?;
    match read_frame::<_, Upload>(recv).await {
        Ok(Some(Upload::Done { blake3: theirs })) if theirs == blake3 => {
            Some(Response::Saved { name, blake3 })
        }
        Ok(Some(Upload::Done { .. })) => {
            let _ = std::fs::remove_file(note);
            Some(error(ErrorCode::Invalid, CHANGED))
        }
        Ok(Some(Upload::Chunk(_))) => Some(error(
            ErrorCode::Invalid,
            format!("more bytes than the {size} the send started with"),
        )),
        Ok(None) => None,
        Err(e) => Some(error(ErrorCode::Invalid, e.to_string())),
    }
}

/// Where this send starts, or why it can't.
fn prepare(agent: &Agent, partial: &str, size: u64) -> Result<Prepared, Error> {
    let io = |e: std::io::Error| Error::new(ErrorCode::Internal, format!("the Inbox: {e}"));
    let inbox = super::ensure_inbox(agent).map_err(io)?;
    let dir = inbox.join(PARTIAL_DIR);
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir).map_err(io)?;
        super::private_dir(&dir);
    }
    let path = dir.join(partial);
    let note = path.with_extension("done");
    if let Some((name, blake3)) = read_note(&note) {
        return Ok(Prepared::Saved { name, blake3, note });
    }
    // The same send with another size is another file now: it starts over.
    if let Some(key) = key_of(partial) {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name != partial && name.ends_with(".part") && key_of(&name) == Some(key) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut have = std::fs::metadata(&path).map_or(0, |m| m.len());
    if have > size {
        std::fs::remove_file(&path).map_err(io)?;
        have = 0;
    }
    let prefix = format!("{}-", partial.split('-').next().unwrap_or(""));
    let free = room::free_space(&inbox);
    let room = room::reserve(&dir, &prefix, partial, size, have, free)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&path).map_err(io)?;
    Ok(Prepared::Write {
        inbox,
        path,
        file,
        have,
        room,
    })
}

/// Checks the finished partial against the device's hash, links it into the Inbox, marks
/// it as downloaded and leaves a note of its name. `None` when the bytes here are not the
/// device's file: the partial goes, so the next try starts over.
fn finish(
    partial: &Path,
    inbox: &Path,
    wanted: &str,
    size: u64,
    expected: &[u8; 32],
) -> std::io::Result<Option<(String, [u8; 32])>> {
    let file = std::fs::File::open(partial)?;
    if file.metadata()?.len() != size {
        return Err(std::io::Error::other("the partial file changed"));
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;
    let hash = *hasher.finalize().as_bytes();
    if hash != *expected {
        std::fs::remove_file(partial)?;
        return Ok(None);
    }
    let name = place(partial, inbox, wanted)?;
    write_note(&partial.with_extension("done"), &name, &hash);
    let path = inbox.join(&name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Never executable; best effort, as some disks have no modes.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        // The new name itself on disk too.
        let _ = std::fs::File::open(inbox).and_then(|d| d.sync_all());
    }
    mark_downloaded(&path);
    Ok(Some((name, hash)))
}

/// Removes unfinished sends and notes last written over [`PARTIAL_AGE`] ago; returns how
/// many.
pub fn remove_old_partials(inbox: &Path, now: SystemTime) -> usize {
    remove_partials_where(inbox, |meta| {
        meta.modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > PARTIAL_AGE)
    })
}

/// An Inbox moved elsewhere: its unfinished sends and notes go now (sends still running
/// there aside), and its `.partial` folder when that is empty; returns how many.
pub fn remove_partials(inbox: &Path) -> usize {
    let removed = remove_partials_where(inbox, |_| true);
    let _ = std::fs::remove_dir(inbox.join(PARTIAL_DIR));
    removed
}

fn remove_partials_where(inbox: &Path, old: impl Fn(&std::fs::Metadata) -> bool) -> usize {
    let writing = WRITING.lock().expect("partials lock poisoned").clone();
    let mut removed = 0;
    for e in std::fs::read_dir(inbox.join(PARTIAL_DIR))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(key) = key_of(&name) else { continue };
        let file = e.metadata().ok().filter(std::fs::Metadata::is_file);
        if !writing.contains(key)
            && file.is_some_and(|m| old(&m))
            && std::fs::remove_file(e.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
