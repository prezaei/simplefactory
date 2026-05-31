//! Run-transcript storage (P3). The worker never touches Blob directly — the
//! Tools service appends transcript chunks here on the worker's behalf.
//!
//! [`TranscriptStore`] is a trait so callers depend on behavior, not on a live
//! object store: [`InMemoryTranscripts`] is the test double, [`FsTranscripts`]
//! gives local-parity persistence, and an Azure Blob / Azurite implementation is
//! the P7 swap (see `docs/factory-implementation-plan.md`). Session/worktree
//! checkpoints (also Blob) land in P5.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Append-only storage for run transcripts, keyed by an opaque `uri`.
#[async_trait::async_trait]
pub trait TranscriptStore: Send + Sync {
    /// Append a chunk to the transcript at `uri`, creating it if absent.
    async fn append(&self, uri: &str, chunk: &str) -> anyhow::Result<()>;
    /// Read the full transcript back. Empty string if nothing was appended.
    async fn read(&self, uri: &str) -> anyhow::Result<String>;
}

/// In-memory transcript store for tests.
#[derive(Default)]
pub struct InMemoryTranscripts {
    blobs: Mutex<HashMap<String, String>>,
}

impl InMemoryTranscripts {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl TranscriptStore for InMemoryTranscripts {
    async fn append(&self, uri: &str, chunk: &str) -> anyhow::Result<()> {
        self.blobs
            .lock()
            .unwrap()
            .entry(uri.to_owned())
            .or_default()
            .push_str(chunk);
        Ok(())
    }

    async fn read(&self, uri: &str) -> anyhow::Result<String> {
        Ok(self
            .blobs
            .lock()
            .unwrap()
            .get(uri)
            .cloned()
            .unwrap_or_default())
    }
}

/// Filesystem-backed transcript store (local parity). `uri` is treated as a path
/// relative to `root`; path traversal outside `root` is rejected.
pub struct FsTranscripts {
    root: PathBuf,
}

impl FsTranscripts {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn resolve(&self, uri: &str) -> anyhow::Result<PathBuf> {
        // Reject absolute paths and `..` traversal — a uri must stay under root.
        let rel = Path::new(uri);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            anyhow::bail!("invalid transcript uri: {uri}");
        }
        Ok(self.root.join(rel))
    }
}

#[async_trait::async_trait]
impl TranscriptStore for FsTranscripts {
    async fn append(&self, uri: &str, chunk: &str) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;
        let path = self.resolve(uri)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        f.write_all(chunk.as_bytes()).await?;
        // `tokio::fs::File` buffers writes and does NOT guarantee a flush on drop, so
        // dropping `f` here (without this) can lose the just-written chunk — a later
        // append/read then sees a truncated transcript (a lost append). Flush explicitly
        // so the bytes reach the file before the handle is dropped.
        f.flush().await?;
        Ok(())
    }

    async fn read(&self, uri: &str) -> anyhow::Result<String> {
        let path = self.resolve(uri)?;
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_appends_accumulate() {
        let store = InMemoryTranscripts::new();
        store.append("run/1", "hello ").await.unwrap();
        store.append("run/1", "world").await.unwrap();
        assert_eq!(store.read("run/1").await.unwrap(), "hello world");
        assert_eq!(store.read("run/other").await.unwrap(), "");
    }

    #[tokio::test]
    async fn fs_appends_accumulate_and_reject_traversal() {
        let dir = std::env::temp_dir().join(format!("factory-blob-test-{}", std::process::id()));
        // Start from a clean dir so a stale `a/b.log` left by a prior run (e.g. a reused
        // PID) can't make the appends accumulate onto old content.
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let store = FsTranscripts::new(&dir);
        store.append("a/b.log", "one\n").await.unwrap();
        store.append("a/b.log", "two\n").await.unwrap();
        // Two sequential appends must accumulate; this is the regression guard for the
        // missing-flush bug (a dropped buffered write would yield only "one\n").
        assert_eq!(store.read("a/b.log").await.unwrap(), "one\ntwo\n");
        assert!(store.append("../escape", "x").await.is_err());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
