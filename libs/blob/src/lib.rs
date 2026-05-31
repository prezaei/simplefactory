//! Azure Blob access for run transcripts and session/worktree checkpoints
//! (Azurite locally).
//!
//! Transcript append lands in **P3**; checkpoint/restore in **P5**
//! (see `docs/factory-implementation-plan.md`).
#![forbid(unsafe_code)]
