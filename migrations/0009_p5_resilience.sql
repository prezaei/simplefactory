-- P5 (Resilience): support the lease sweeper's wall-clock-cap branch and the
-- crash-resume checkpoint lookup.
--
-- The sweeper reclaims an attempt whose *lease* expired OR whose run has exceeded an
-- absolute wall-clock cap (a stuck-but-alive worker that keeps heart-beating). The
-- wall-clock branch filters running attempts by `started_at`; this partial index keeps
-- that scan cheap. (Lease-expiry already has `work_items_lease_expiry` from 0001.)
CREATE INDEX IF NOT EXISTS attempts_running_started_at
    ON attempts (started_at)
    WHERE status = 'running';

-- Crash-resume looks up the most recent RESUMABLE attempt for a work item — one with
-- BOTH session_id AND checkpoint_uri set (see `last_checkpoint_for_item`). The partial
-- predicate matches that query exactly (both columns non-NULL), keeping the index smaller
-- and more selective than filtering on session_id alone.
CREATE INDEX IF NOT EXISTS attempts_checkpointed
    ON attempts (work_item_id, started_at DESC)
    WHERE session_id IS NOT NULL AND checkpoint_uri IS NOT NULL;
