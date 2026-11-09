//! `pgpu/files/1`: files on a machine (F40). Every request and every transfer has its own
//! stream on one shared connection, so a big download doesn't hold up browsing.

use std::io::Read;
use std::path::{Path, PathBuf};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use pocketgpu_proto::alpn;
use pocketgpu_proto::files::{CHUNK, Entry, Overview, Request, Response, Root, Upload};
use pocketgpu_proto::{read_frame, write_frame};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::{ClientError, Machine, request};

/// A finished download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// Bytes in the file now, from before a resume included.
    pub size: u64,
    pub blake3: [u8; 32],
}

/// A finished upload: the name the machine saved it under in its Inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub name: String,
    pub blake3: [u8; 32],
}

impl Machine {
    /// What this device may open on the machine.
    pub async fn files_overview(&self) -> Result<Overview, ClientError> {
        match self.files(&Request::Overview).await? {
            Response::Overview(o) => Ok(o),
            _ => Err(ClientError::NoReply),
        }
    }

    /// One page of a folder, and the offset of the next page if there is one. `path` is
    /// `/`-separated and relative to `root`; `""` is the root itself.
    pub async fn files_list(
        &self,
        root: Root,
        path: &str,
        offset: u32,
    ) -> Result<(Vec<Entry>, Option<u32>), ClientError> {
        let req = Request::List {
            root,
            path: path.into(),
            offset,
        };
        match self.files(&req).await? {
            Response::Entries { entries, next } => Ok((entries, next)),
            _ => Err(ClientError::NoReply),
        }
    }

    /// Saves a file to `dest`, carrying on from what `dest` already holds (start with a
    /// missing or empty file for a fresh download). `progress` gets the bytes in `dest` and
    /// the file's size. When the whole file is there it is checked against the machine's
    /// BLAKE3 hash; on [`ClientError::FileChanged`] or [`ClientError::HashMismatch`] delete
    /// `dest` and start again.
    pub async fn download(
        &self,
        root: Root,
        path: &str,
        dest: &Path,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<Downloaded, ClientError> {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dest)
            .await
            .map_err(ClientError::File)?;
        let have = file.metadata().await.map_err(ClientError::File)?.len();
        let (_conn, mut send, mut recv) = self.files_stream().await?;
        let req = Request::Get {
            root,
            path: path.into(),
            from: have,
        };
        write_frame(&mut send, &req).await?;
        send.finish()
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        let size = match read_frame::<_, Response>(&mut recv).await? {
            Some(Response::Head { size, .. }) => size,
            Some(Response::Error(e)) => return Err(ClientError::Remote(e)),
            Some(_) | None => return Err(ClientError::NoReply),
        };
        if size < have {
            return Err(ClientError::FileChanged);
        }
        let mut done = have;
        progress(done, size);
        let expected = loop {
            match read_frame::<_, Response>(&mut recv).await? {
                Some(Response::Chunk(bytes)) => {
                    done += bytes.len() as u64;
                    if done > size {
                        return Err(ClientError::FileChanged);
                    }
                    file.write_all(&bytes).await.map_err(ClientError::File)?;
                    progress(done, size);
                }
                Some(Response::Done { blake3 }) => break blake3,
                Some(Response::Error(e)) => return Err(ClientError::Remote(e)),
                Some(_) => return Err(ClientError::NoReply),
                None => {
                    return Err(ClientError::Connection(format!(
                        "the download stopped at {done} of {size} bytes; save again to carry on"
                    )));
                }
            }
        };
        if done != size {
            return Err(ClientError::FileChanged);
        }
        file.flush().await.map_err(ClientError::File)?;
        file.sync_all().await.map_err(ClientError::File)?;
        drop(file);
        let hash = hash_file(dest.to_path_buf(), None).await?;
        if hash != expected {
            return Err(ClientError::HashMismatch);
        }
        Ok(Downloaded { size, blake3: hash })
    }

    /// Sends `src` to the machine's Inbox as `name` (the machine cleans the name and picks
    /// a free one). `transfer` is chosen once per file and kept to resume: the machine then
    /// says how much it already has, and only the rest is sent. `progress` gets the bytes
    /// the machine has and the file's size. The BLAKE3 hash of `src` goes last, and the
    /// machine saves the file only when what it got has that hash; when `src` changed
    /// between tries it says so and drops what it had, so trying again starts over. Trying
    /// again after a lost reply gets the same `Saved`, without a second copy.
    pub async fn upload(
        &self,
        src: &Path,
        name: &str,
        transfer: [u8; 16],
        mut progress: impl FnMut(u64, u64),
    ) -> Result<Saved, ClientError> {
        let mut file = tokio::fs::File::open(src)
            .await
            .map_err(ClientError::File)?;
        let size = file.metadata().await.map_err(ClientError::File)?.len();
        let (_conn, mut send, mut recv) = self.files_stream().await?;
        let req = Request::Put {
            name: name.into(),
            size,
            transfer,
        };
        write_frame(&mut send, &req).await?;
        let have = match read_frame::<_, Response>(&mut recv).await? {
            Some(Response::Ready { have }) => have,
            Some(Response::Error(e)) => return Err(ClientError::Remote(e)),
            Some(_) | None => return Err(ClientError::NoReply),
        };
        if have > size {
            return Err(ClientError::FileChanged);
        }
        // The hash covers the whole file, so the part the machine has is read too.
        let mut hasher = hasher_of(src.to_path_buf(), have).await?;
        file.seek(std::io::SeekFrom::Start(have))
            .await
            .map_err(ClientError::File)?;
        let mut done = have;
        progress(done, size);
        let mut buf = vec![0u8; CHUNK];
        while done < size {
            let want = (size - done).min(CHUNK as u64) as usize;
            let n = file
                .read(&mut buf[..want])
                .await
                .map_err(ClientError::File)?;
            if n == 0 {
                // Shorter than when it started.
                return Err(ClientError::FileChanged);
            }
            hasher.update(&buf[..n]);
            if let Err(e) = write_frame(&mut send, &Upload::Chunk(buf[..n].to_vec())).await {
                return Err(why_stopped(&mut recv, e).await);
            }
            done += n as u64;
            progress(done, size);
        }
        drop(file);
        let ours = *hasher.finalize().as_bytes();
        if let Err(e) = write_frame(&mut send, &Upload::Done { blake3: ours }).await {
            return Err(why_stopped(&mut recv, e).await);
        }
        send.finish()
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        let (saved, theirs) = match read_frame::<_, Response>(&mut recv).await? {
            Some(Response::Saved { name, blake3 }) => (name, blake3),
            Some(Response::Error(e)) => return Err(ClientError::Remote(e)),
            Some(_) | None => return Err(ClientError::NoReply),
        };
        if theirs != ours {
            return Err(ClientError::HashMismatch);
        }
        Ok(Saved {
            name: saved,
            blake3: theirs,
        })
    }

    async fn files(&self, req: &Request) -> Result<Response, ClientError> {
        let conn = self.files.get(self, alpn::FILES).await?;
        match request(&conn, req).await? {
            Response::Error(e) => Err(ClientError::Remote(e)),
            other => Ok(other),
        }
    }

    /// A new stream on the shared files connection, with the connection kept alive by the
    /// caller for as long as the transfer runs.
    async fn files_stream(&self) -> Result<(Connection, SendStream, RecvStream), ClientError> {
        let conn = self.files.get(self, alpn::FILES).await?;
        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        Ok((conn, send, recv))
    }
}

/// The machine stopped reading an upload: its reason, if it sent one in time.
async fn why_stopped(recv: &mut RecvStream, e: pocketgpu_proto::FrameError) -> ClientError {
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_frame::<_, Response>(recv),
    )
    .await;
    match reply {
        Ok(Ok(Some(Response::Error(r)))) => ClientError::Remote(r),
        _ => e.into(),
    }
}

/// A BLAKE3 hasher fed the first `len` bytes of a file.
async fn hasher_of(path: PathBuf, len: u64) -> Result<blake3::Hasher, ClientError> {
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)?;
        let mut hasher = blake3::Hasher::new();
        let read = std::io::copy(&mut file.take(len), &mut hasher)?;
        if read != len {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        Ok(hasher)
    })
    .await
    .map_err(|e| ClientError::File(std::io::Error::other(e)))?
    .map_err(ClientError::File)
}

/// The BLAKE3 hash of a file, or of its first `len` bytes.
async fn hash_file(path: PathBuf, len: Option<u64>) -> Result<[u8; 32], ClientError> {
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)?;
        let mut hasher = blake3::Hasher::new();
        match len {
            Some(len) => hasher.update_reader(file.take(len))?,
            None => hasher.update_reader(file)?,
        };
        Ok::<_, std::io::Error>(*hasher.finalize().as_bytes())
    })
    .await
    .map_err(|e| ClientError::File(std::io::Error::other(e)))?
    .map_err(ClientError::File)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashes_whole_files_and_their_start() {
        let dir = std::env::temp_dir().join(format!("pgpu-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.bin");
        let data: Vec<u8> = (0..(3 * CHUNK + 17)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let whole = hash_file(path.clone(), None).await.unwrap();
        assert_eq!(whole, *blake3::hash(&data).as_bytes());
        let start = hash_file(path.clone(), Some(100)).await.unwrap();
        assert_eq!(start, *blake3::hash(&data[..100]).as_bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
