//! Run-transcript + checkpoint storage (P3/P5/P7). The worker never touches Blob
//! directly — the Tools service appends transcript chunks here on the worker's
//! behalf; the worker snapshots session/worktree checkpoints here for crash-resume.
//!
//! [`TranscriptStore`]/[`CheckpointStore`] are traits so callers depend on behavior,
//! not on a live object store. The **production** impls are Azure Blob–backed
//! ([`AzureBlobTranscripts`]/[`AzureBlobCheckpoints`], in `azure.rs`), exercised
//! against Azurite locally (P7) and real Azure in P8. [`InMemoryTranscripts`]/
//! [`InMemoryCheckpoints`] are the hermetic **test doubles** for caller unit tests.
#![forbid(unsafe_code)]

mod archive;
mod azure;
pub use archive::{archive_dirs, extract_dirs};
pub use azure::{AzureBlobCheckpoints, AzureBlobTranscripts};

/// Default Blob container for crash-resume checkpoints. A fixed convention (not an
/// env override) so the worker and a re-dispatch always agree on where checkpoints
/// live — the name has one home next to the store that uses it.
pub const DEFAULT_CHECKPOINT_CONTAINER: &str = "checkpoints";
/// Default Blob container for run transcripts.
pub const DEFAULT_TRANSCRIPT_CONTAINER: &str = "transcripts";

use std::collections::HashMap;
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

/// Crash-resume checkpoint storage (P5): the worker snapshots its Claude session +
/// worktree here on an interval/at tool boundaries, keyed by an opaque `uri`, so a
/// re-dispatch after a crash can restore it and `claude --resume`. Unlike the
/// append-only transcript, a checkpoint is the latest-wins whole-snapshot blob.
/// [`InMemoryCheckpoints`] is the test double; [`AzureBlobCheckpoints`] is production.
#[async_trait::async_trait]
pub trait CheckpointStore: Send + Sync {
    /// Write (overwrite) the checkpoint at `uri`.
    async fn write(&self, uri: &str, data: &[u8]) -> anyhow::Result<()>;
    /// Read the checkpoint back. `None` if nothing was written at `uri`.
    async fn read(&self, uri: &str) -> anyhow::Result<Option<Vec<u8>>>;
}

/// In-memory checkpoint store for tests.
#[derive(Default)]
pub struct InMemoryCheckpoints {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
}

impl InMemoryCheckpoints {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl CheckpointStore for InMemoryCheckpoints {
    async fn write(&self, uri: &str, data: &[u8]) -> anyhow::Result<()> {
        self.blobs
            .lock()
            .unwrap()
            .insert(uri.to_owned(), data.to_vec());
        Ok(())
    }

    async fn read(&self, uri: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.blobs.lock().unwrap().get(uri).cloned())
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
    async fn in_memory_checkpoint_roundtrip_and_overwrite() {
        let store = InMemoryCheckpoints::new();
        assert_eq!(store.read("cp/1").await.unwrap(), None);
        store.write("cp/1", b"first").await.unwrap();
        assert_eq!(store.read("cp/1").await.unwrap(), Some(b"first".to_vec()));
        // Latest-wins: a second write replaces, not appends.
        store.write("cp/1", b"second").await.unwrap();
        assert_eq!(store.read("cp/1").await.unwrap(), Some(b"second".to_vec()));
    }
}
