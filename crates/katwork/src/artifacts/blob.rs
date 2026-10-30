//! The blob store: file bytes, content-addressed under one directory per workspace.
//!
//! A local store keeps `<root>/<workspace_id>/tmp/<random>` while a file is staged and
//! `<root>/<workspace_id>/sha256/<aa>/<bb>/<hex>` once it is committed, where `aa` and `bb` are
//! the digest's first two byte pairs. Files are mode 0600 and directories 0700. Every path is
//! built from a workspace id and a digest that is checked to be 64 lowercase hex characters
//! first, so a path never leaves the workspace's directory.
use std::{
    fmt, io,
    io::{SeekFrom, Write as _},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    pin::Pin,
};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use super::media::{TextProbe, TextScanner};
use crate::{error::KatError, ids::WorkspaceId};

/// A blob's bytes, checked against its digest before the first chunk.
pub type BlobStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;

/// A committed blob: a version row's `sha256` and `bytes` in its workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    pub workspace_id: WorkspaceId,
    pub sha256: String,
    pub bytes: u64,
}

/// Where file bytes live. Every method works inside one workspace's directory.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// A temp file in the workspace's `tmp/`, created exclusively with mode 0600, that refuses
    /// more than `max_bytes`.
    async fn stage(&self, workspace: WorkspaceId, max_bytes: u64) -> Result<Staging, KatError>;

    /// `stage`, one `write` and `finish`, for bytes already in memory (writers).
    async fn stage_bytes(
        &self,
        workspace: WorkspaceId,
        bytes: &[u8],
    ) -> Result<StagedBlob, KatError> {
        let mut staging = self.stage(workspace, bytes.len() as u64).await?;
        staging.write(bytes).await?;
        staging.finish().await
    }

    /// Renames a staged file to its content address; an existing file there is replaced with
    /// the same bytes. Called only through `store::commit_blob_on`, under the blob lock.
    async fn commit(&self, staged: StagedBlob) -> Result<BlobRef, KatError>;

    /// The blob's bytes, re-hashed: a length or hash mismatch is `BlobCorrupt`, a missing file
    /// `NotFound`, and a blob larger than `max_bytes` `BodyTooLarge` before anything is read.
    async fn read(&self, blob: &BlobRef, max_bytes: u64) -> Result<Vec<u8>, KatError>;

    /// The blob's bytes as a stream, for a download: the whole file is hashed first, so a
    /// tampered blob is `BlobCorrupt` before any byte is sent, and memory stays one chunk.
    async fn open(&self, blob: &BlobRef) -> Result<BlobStream, KatError>;

    /// Unlinks the blob; a missing file is no error. Called only under the blob lock.
    async fn remove(&self, blob: &BlobRef) -> Result<(), KatError>;

    /// Bytes free to the server on the store's filesystem.
    async fn free_bytes(&self) -> Result<u64, KatError>;
}

/// A file being written. Dropped before `finish`, it deletes its temp file.
pub struct Staging {
    workspace_id: WorkspaceId,
    file: tokio::fs::File,
    temp: Option<PathBuf>,
    max_bytes: u64,
    bytes: u64,
    hasher: Sha256,
    head: Vec<u8>,
    text: TextScanner,
    /// A write started and did not finish (its future was dropped, or it failed): the file may
    /// hold part of a chunk the hash does not.
    torn: bool,
}

impl Staging {
    /// Writes, hashes and probes one chunk. Past `max_bytes` it is `BodyTooLarge`, with nothing
    /// of the chunk written.
    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), KatError> {
        if self.torn {
            return Err(torn());
        }
        let total = self
            .bytes
            .checked_add(chunk.len() as u64)
            .filter(|total| *total <= self.max_bytes)
            .ok_or(KatError::BodyTooLarge)?;
        self.torn = true;
        self.file.write_all(chunk).await.map_err(storage_error)?;
        self.torn = false;
        if self.head.len() < HEAD_BYTES {
            let take = (HEAD_BYTES - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
        }
        self.hasher.update(chunk);
        self.text.feed(chunk);
        self.bytes = total;
        Ok(())
    }

    /// Makes the file durable and checks that it holds every byte hashed.
    ///
    /// `tokio::fs::File` writes in the background: an error of the last write surfaces on the
    /// next write or flush, and `sync_all` does not report it. So: flush (any write error),
    /// fsync, then compare the length on disk with the count hashed, so a short file never
    /// passes under the whole file's digest.
    pub async fn finish(mut self) -> Result<StagedBlob, KatError> {
        if self.torn {
            return Err(torn());
        }
        self.file.flush().await.map_err(storage_error)?;
        self.file.sync_all().await.map_err(storage_error)?;
        let on_disk = self.file.metadata().await.map_err(storage_error)?.len();
        if on_disk != self.bytes {
            tracing::error!(
                workspace_id = %self.workspace_id,
                hashed = self.bytes,
                on_disk,
                "staged_length_mismatch"
            );
            return Err(KatError::Io(io::Error::other("staged_length_mismatch")));
        }
        let sha256 = format!("{:x}", std::mem::take(&mut self.hasher).finalize());
        let head = std::mem::take(&mut self.head);
        let text = self.text.finish(&head);
        Ok(StagedBlob {
            workspace_id: self.workspace_id,
            sealed: (self.workspace_id, sha256.clone(), self.bytes),
            sha256,
            bytes: self.bytes,
            head,
            text,
            temp: self.temp.take(),
        })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(temp) = self.temp.take() {
            let _ = std::fs::remove_file(temp);
        }
    }
}

impl fmt::Debug for Staging {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Staging")
            .field("workspace_id", &self.workspace_id)
            .field("bytes", &self.bytes)
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

/// A staged, fsynced file waiting for `commit`. Dropped first, it deletes its temp file.
#[derive(Debug)]
pub struct StagedBlob {
    pub workspace_id: WorkspaceId,
    pub sha256: String,
    pub bytes: u64,
    /// The file's first 8 bytes (fewer for a shorter file).
    pub head: Vec<u8>,
    pub text: TextProbe,
    temp: Option<PathBuf>,
    /// What staging computed; `commit` refuses a blob whose public fields no longer match it.
    sealed: (WorkspaceId, String, u64),
}

impl Drop for StagedBlob {
    fn drop(&mut self) {
        if let Some(temp) = self.temp.take() {
            let _ = std::fs::remove_file(temp);
        }
    }
}

const HEAD_BYTES: usize = 8;
/// One read or one chunk of a download.
const CHUNK_BYTES: usize = 65_536;

fn torn() -> KatError {
    KatError::Io(io::Error::other("staging_torn"))
}

/// A full disk or quota is `StorageFull`; any other error keeps its cause.
fn storage_error(error: io::Error) -> KatError {
    match error.kind() {
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => KatError::StorageFull,
        _ => KatError::Io(error),
    }
}

fn valid_digest(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The local filesystem store, rooted at `KATWORK_ARTIFACT_DIR`.
#[derive(Debug, Clone)]
pub struct LocalBlobStore {
    root: PathBuf,
}

impl LocalBlobStore {
    /// Creates the root (mode 0700) and proves it writable by writing, fsyncing and deleting a
    /// probe file, since creating a directory that exists succeeds even when it is read-only.
    /// Any failure is `ConfigInvalid { var: "KATWORK_ARTIFACT_DIR" }`, with the cause logged.
    pub fn new(root: PathBuf) -> Result<Self, KatError> {
        let refuse = |cause: &dyn fmt::Display| {
            tracing::error!(root = %root.display(), %cause, "artifact_dir_unusable");
            KatError::ConfigInvalid {
                var: "KATWORK_ARTIFACT_DIR",
            }
        };
        if !root.is_absolute() {
            return Err(refuse(&"the path is not absolute"));
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)
            .map_err(|error| refuse(&error))?;
        let probe = root.join(format!(".write-probe-{}", Uuid::now_v7().simple()));
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&probe)
            .and_then(|mut file| {
                let written = file.write_all(b"katwork").and_then(|()| file.sync_all());
                let removed = std::fs::remove_file(&probe);
                written.and(removed)
            });
        written.map_err(|error| refuse(&error))?;
        Ok(Self { root })
    }

    fn workspace_dir(&self, workspace: WorkspaceId) -> PathBuf {
        self.root.join(workspace.to_string())
    }

    /// `<root>/<workspace>/sha256/<aa>/<bb>/<hex>`, for a well-formed digest only.
    fn blob_path(&self, workspace: WorkspaceId, sha256: &str) -> Result<PathBuf, KatError> {
        if !valid_digest(sha256) {
            return Err(KatError::RequestInvalid);
        }
        Ok(self
            .workspace_dir(workspace)
            .join("sha256")
            .join(&sha256[0..2])
            .join(&sha256[2..4])
            .join(sha256))
    }

    /// Opens a blob to read it. A missing file is data loss, though the client sees a 404.
    async fn open_blob(&self, blob: &BlobRef) -> Result<tokio::fs::File, KatError> {
        let path = self.blob_path(blob.workspace_id, &blob.sha256)?;
        match tokio::fs::File::open(&path).await {
            Ok(file) => Ok(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tracing::error!(
                    workspace_id = %blob.workspace_id,
                    sha256 = %blob.sha256,
                    "blob_missing"
                );
                Err(KatError::NotFound)
            }
            Err(error) => Err(storage_error(error)),
        }
    }
}

/// `BlobCorrupt` unless `bytes` and `sha256` are the blob's.
fn check(blob: &BlobRef, bytes: u64, sha256: &str) -> Result<(), KatError> {
    if bytes == blob.bytes && sha256 == blob.sha256 {
        return Ok(());
    }
    tracing::error!(
        workspace_id = %blob.workspace_id,
        sha256 = %blob.sha256,
        expected_bytes = blob.bytes,
        bytes,
        "blob_corrupt"
    );
    Err(KatError::BlobCorrupt)
}

async fn create_dir(path: &Path) -> Result<(), KatError> {
    tokio::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .await
        .map_err(storage_error)
}

/// Makes a directory's entries durable: `fsync` of a file does not cover its name.
async fn sync_dir(path: &Path) -> Result<(), KatError> {
    tokio::fs::File::open(path)
        .await
        .map_err(storage_error)?
        .sync_all()
        .await
        .map_err(storage_error)
}

#[async_trait]
impl BlobStore for LocalBlobStore {
    async fn stage(&self, workspace: WorkspaceId, max_bytes: u64) -> Result<Staging, KatError> {
        let dir = self.workspace_dir(workspace).join("tmp");
        create_dir(&dir).await?;
        let temp = dir.join(Uuid::now_v7().simple().to_string());
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .await
            .map_err(storage_error)?;
        Ok(Staging {
            workspace_id: workspace,
            file,
            temp: Some(temp),
            max_bytes,
            bytes: 0,
            hasher: Sha256::new(),
            head: Vec::with_capacity(HEAD_BYTES),
            text: TextScanner::default(),
            torn: false,
        })
    }

    async fn commit(&self, mut staged: StagedBlob) -> Result<BlobRef, KatError> {
        let path = self.blob_path(staged.workspace_id, &staged.sha256)?;
        if (staged.workspace_id, &staged.sha256, staged.bytes)
            != (staged.sealed.0, &staged.sealed.1, staged.sealed.2)
        {
            return Err(KatError::BlobCorrupt);
        }
        let Some(temp) = staged.temp.clone() else {
            return Err(KatError::Io(io::Error::other("blob_already_committed")));
        };
        let dir = path.parent().expect("a blob path has a directory");
        // The first blob under a new `aa/bb` also makes every directory above it durable.
        let fresh = !tokio::fs::try_exists(dir).await.map_err(storage_error)?;
        create_dir(dir).await?;
        if let Err(error) = tokio::fs::rename(&temp, &path).await {
            if error.kind() == io::ErrorKind::CrossesDevices {
                tracing::error!(
                    root = %self.root.display(),
                    "tmp and sha256 are on different filesystems; mount the whole artifact directory as one volume"
                );
            }
            return Err(storage_error(error));
        }
        // The temp file is the blob now.
        staged.temp = None;
        sync_dir(dir).await?;
        if fresh {
            for ancestor in dir.ancestors().skip(1).take(4) {
                sync_dir(ancestor).await?;
            }
        }
        Ok(BlobRef {
            workspace_id: staged.workspace_id,
            sha256: staged.sha256.clone(),
            bytes: staged.bytes,
        })
    }

    async fn read(&self, blob: &BlobRef, max_bytes: u64) -> Result<Vec<u8>, KatError> {
        // The digest is checked before any other use of the blob.
        self.blob_path(blob.workspace_id, &blob.sha256)?;
        if blob.bytes > max_bytes {
            return Err(KatError::BodyTooLarge);
        }
        let file = self.open_blob(blob).await?;
        let capacity = usize::try_from(blob.bytes).map_err(|_| KatError::BodyTooLarge)?;
        let mut bytes = Vec::with_capacity(capacity);
        // One byte past the expected length shows a file that grew.
        file.take(blob.bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(storage_error)?;
        check(
            blob,
            bytes.len() as u64,
            &format!("{:x}", Sha256::digest(&bytes)),
        )?;
        Ok(bytes)
    }

    async fn open(&self, blob: &BlobRef) -> Result<BlobStream, KatError> {
        let mut file = self.open_blob(blob).await?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; CHUNK_BYTES];
        let mut total: u64 = 0;
        loop {
            let read = file.read(&mut buffer).await.map_err(storage_error)?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > blob.bytes {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        check(blob, total, &format!("{:x}", hasher.finalize()))?;
        file.seek(SeekFrom::Start(0)).await.map_err(storage_error)?;
        let expected = blob.clone();
        Ok(Box::pin(async_stream::try_stream! {
            // The second pass hashes again and holds back the last chunk until the digest
            // matches, so a file changed in place between the passes never arrives whole.
            let mut file = file.take(expected.bytes.saturating_add(1));
            let mut hasher = Sha256::new();
            let mut sent: u64 = 0;
            let mut pending: Option<Bytes> = None;
            loop {
                let mut chunk = vec![0u8; CHUNK_BYTES];
                let read = file.read(&mut chunk).await?;
                if read == 0 {
                    break;
                }
                chunk.truncate(read);
                hasher.update(&chunk);
                sent += read as u64;
                if let Some(ready) = pending.replace(Bytes::from(chunk)) {
                    yield ready;
                }
            }
            if sent != expected.bytes || format!("{:x}", hasher.finalize()) != expected.sha256 {
                tracing::error!(
                    workspace_id = %expected.workspace_id,
                    sha256 = %expected.sha256,
                    "blob_changed_during_download"
                );
                Err(io::Error::other("blob_corrupt"))?;
            }
            if let Some(last) = pending {
                yield last;
            }
        }))
    }

    async fn remove(&self, blob: &BlobRef) -> Result<(), KatError> {
        let path = self.blob_path(blob.workspace_id, &blob.sha256)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(storage_error(error)),
        }
        // The unlink is durable before the caller's transaction commits.
        sync_dir(path.parent().expect("a blob path has a directory")).await
    }

    async fn free_bytes(&self) -> Result<u64, KatError> {
        let root = self.root.clone();
        let stats = tokio::task::spawn_blocking(move || nix::sys::statvfs::statvfs(&root))
            .await
            .map_err(|error| KatError::Io(io::Error::other(error)))?
            .map_err(|errno| KatError::Io(io::Error::from(errno)))?;
        // POSIX counts available blocks in units of the fragment size, not the block size.
        #[allow(
            clippy::unnecessary_cast,
            reason = "statvfs field types differ between platforms"
        )]
        let (available, fragment) = (
            stats.blocks_available() as u64,
            stats.fragment_size() as u64,
        );
        Ok(available.saturating_mul(fragment))
    }
}

/// The store `Core::new` installs for tests and tools: every call fails, so a test that needs
/// files passes its own store to `Core::with_blob_store`, and none touches the default root.
pub(crate) struct Unconfigured;

fn unconfigured() -> KatError {
    KatError::ConfigInvalid {
        var: "KATWORK_ARTIFACT_DIR",
    }
}

#[async_trait]
impl BlobStore for Unconfigured {
    async fn stage(&self, _: WorkspaceId, _: u64) -> Result<Staging, KatError> {
        Err(unconfigured())
    }

    async fn commit(&self, _: StagedBlob) -> Result<BlobRef, KatError> {
        Err(unconfigured())
    }

    async fn read(&self, _: &BlobRef, _: u64) -> Result<Vec<u8>, KatError> {
        Err(unconfigured())
    }

    async fn open(&self, _: &BlobRef) -> Result<BlobStream, KatError> {
        Err(unconfigured())
    }

    async fn remove(&self, _: &BlobRef) -> Result<(), KatError> {
        Err(unconfigured())
    }

    async fn free_bytes(&self) -> Result<u64, KatError> {
        Err(unconfigured())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Root(PathBuf);

    impl Root {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("katwork-blob-{}", Uuid::now_v7())))
        }

        fn store(&self) -> LocalBlobStore {
            LocalBlobStore::new(self.0.clone()).unwrap()
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A disk that fills up on the last write: `/dev/full` fails every write with ENOSPC.
    /// The write itself is buffered and returns at once; only `finish` can see the failure.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn finish_fails_when_the_last_write_failed() {
        let root = Root::new();
        let mut staging = root.store().stage(WorkspaceId::new(), 1024).await.unwrap();
        let temp = staging.temp.clone().unwrap();
        staging.file = tokio::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .await
            .unwrap();
        staging.write(b"the last chunk").await.unwrap();
        assert!(matches!(staging.finish().await, Err(KatError::StorageFull)));
        assert!(!temp.exists(), "a failed finish left its temp file");
    }

    #[tokio::test]
    async fn staged_file_length_equals_the_hashed_count() {
        let root = Root::new();
        let store = root.store();
        let mut staging = store.stage(WorkspaceId::new(), 1024).await.unwrap();
        staging.write(b"twelve bytes").await.unwrap();
        let temp = staging.temp.clone().unwrap();
        let staged = staging.finish().await.unwrap();
        assert_eq!(staged.bytes, 12);
        assert_eq!(std::fs::metadata(&temp).unwrap().len(), 12);
        // A file shorter on disk than what was hashed never passes.
        let mut staging = store.stage(WorkspaceId::new(), 1024).await.unwrap();
        staging.write(b"twelve bytes").await.unwrap();
        staging.file.flush().await.unwrap();
        let temp = staging.temp.clone().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&temp)
            .unwrap()
            .set_len(6)
            .unwrap();
        assert!(matches!(staging.finish().await, Err(KatError::Io(_))));
        assert!(!temp.exists());
    }

    #[tokio::test]
    async fn a_torn_write_is_never_finished() {
        let root = Root::new();
        let mut staging = root.store().stage(WorkspaceId::new(), 1024).await.unwrap();
        staging.write(b"whole").await.unwrap();
        // A write whose future was dropped part way.
        staging.torn = true;
        assert!(matches!(staging.write(b"more").await, Err(KatError::Io(_))));
        assert!(matches!(staging.finish().await, Err(KatError::Io(_))));
    }
}
