//! Worktree/session checkpoint archives (P7 crash-resume).
//!
//! A crash-resume checkpoint is a `tar.gz` of one or more named directory roots — the
//! Coder's git **worktree** and the **`claude` session** dir — so a re-dispatch in a
//! fresh worker container can restore exactly what the crashed run had and
//! `claude --resume`. [`archive_dirs`] packs them; [`extract_dirs`] unpacks the chosen
//! roots into their destinations. Both are synchronous (CPU/FS-bound); async callers run
//! them on a blocking thread. The bytes are opaque to the resume orchestration — only the
//! worker that wrote them needs to understand the layout.

use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

/// Tar + gzip a set of named directory roots into one archive. Each `(name, dir)` stores
/// `dir`'s contents under `name/` in the archive. A root whose path does not exist is
/// SKIPPED (logged), not an error — e.g. the `claude` session dir may not exist yet on a
/// very early checkpoint. Returns the gzip bytes.
pub fn archive_dirs(roots: &[(&str, &Path)]) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut included = 0usize;
    for (name, dir) in roots {
        if !dir.exists() {
            tracing::info!(root = name, path = %dir.display(), "archive_dirs: root missing, skipped");
            continue;
        }
        // `append_dir_all` stores `dir`'s tree under the `name/` prefix.
        builder.append_dir_all(name, dir)?;
        included += 1;
    }
    let gz = builder.into_inner()?;
    let bytes = gz.finish()?;
    tracing::info!(
        roots = roots.len(),
        included,
        bytes = bytes.len(),
        "archive_dirs: packed checkpoint"
    );
    Ok(bytes)
}

/// Extract an archive made by [`archive_dirs`]: for each `(name, dest)`, the entries
/// stored under `name/` are written into `dest`. Entries whose top component is not a
/// requested `name` are ignored. An entry whose path contains `..` or a root component
/// is rejected, so a name-based traversal can't escape `dest`. The writer is trusted
/// (the worker tars its own worktree, and `append_dir_all` does not follow symlinks, so
/// a real worktree can't produce an out-of-tree entry); this guards the entry *paths*,
/// not a full sandbox against an adversarially crafted archive (e.g. symlink-then-write-
/// through is out of scope — the input is our own checkpoint).
pub fn extract_dirs(archive: &[u8], dests: &[(&str, &Path)]) -> anyhow::Result<()> {
    // Create every destination up front so an archive that simply lacks a root still
    // yields an (empty) dest rather than a later missing-dir surprise.
    for (_, dest) in dests {
        std::fs::create_dir_all(dest)?;
    }
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    let mut extracted = 0usize;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut comps = path.components();
        let Some(Component::Normal(top)) = comps.next() else {
            continue; // skip odd leading components (root/curdir/parent)
        };
        let rel: PathBuf = comps.as_path().to_path_buf();
        // Reject traversal: the remaining path must stay within the destination.
        if rel
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::RootDir))
        {
            anyhow::bail!(
                "checkpoint archive entry escapes its root: {}",
                path.display()
            );
        }
        let Some((_, dest)) = dests
            .iter()
            .find(|(name, _)| Path::new(name) == Path::new(top))
        else {
            continue; // an archived root the caller didn't ask to restore
        };
        if rel.as_os_str().is_empty() {
            continue; // the root dir entry itself; dest already created
        }
        entry.unpack(dest.join(&rel))?;
        extracted += 1;
    }
    tracing::info!(extracted, "extract_dirs: restored checkpoint");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, contents: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("factory-archive-{label}-{}", uuid_like()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // A cheap unique suffix without pulling in the uuid crate for a test.
    fn uuid_like() -> u128 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn archive_then_extract_round_trips_multiple_roots() {
        let worktree = tmp("wt-src");
        let session = tmp("ses-src");
        write(&worktree, "src/main.rs", "fn main() {}");
        write(&worktree, "README.md", "hi");
        write(&session, "projects/abc/state.json", "{\"k\":1}");

        let bytes = archive_dirs(&[("worktree", &worktree), ("session", &session)]).unwrap();
        assert!(!bytes.is_empty());

        let wt_dest = tmp("wt-dest");
        let ses_dest = tmp("ses-dest");
        extract_dirs(&bytes, &[("worktree", &wt_dest), ("session", &ses_dest)]).unwrap();

        assert_eq!(
            std::fs::read_to_string(wt_dest.join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
        assert_eq!(
            std::fs::read_to_string(wt_dest.join("README.md")).unwrap(),
            "hi"
        );
        assert_eq!(
            std::fs::read_to_string(ses_dest.join("projects/abc/state.json")).unwrap(),
            "{\"k\":1}"
        );
        for d in [worktree, session, wt_dest, ses_dest] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn missing_root_is_skipped_not_an_error() {
        let worktree = tmp("wt-only");
        write(&worktree, "a.txt", "a");
        // `session` path does not exist — must be skipped, not fail.
        let bytes = archive_dirs(&[
            ("worktree", &worktree),
            ("session", Path::new("/no/such/dir")),
        ])
        .unwrap();
        let dest = tmp("wt-only-dest");
        let ses_dest = tmp("ses-empty-dest");
        extract_dirs(&bytes, &[("worktree", &dest), ("session", &ses_dest)]).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "a");
        // The absent root extracts to an empty (created) directory.
        assert!(ses_dest.exists());
        assert_eq!(std::fs::read_dir(&ses_dest).unwrap().count(), 0);
        for d in [worktree, dest, ses_dest] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn extract_ignores_roots_the_caller_did_not_request() {
        let worktree = tmp("wt-sel");
        let session = tmp("ses-sel");
        write(&worktree, "code.rs", "x");
        write(&session, "s.json", "y");
        let bytes = archive_dirs(&[("worktree", &worktree), ("session", &session)]).unwrap();
        // Only ask for "worktree" back.
        let dest = tmp("wt-sel-dest");
        extract_dirs(&bytes, &[("worktree", &dest)]).unwrap();
        assert!(dest.join("code.rs").exists());
        assert!(!dest.join("s.json").exists(), "session entries are ignored");
        for d in [worktree, session, dest] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
