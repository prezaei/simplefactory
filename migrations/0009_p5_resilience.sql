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

-- Crash-resume looks up the most recent checkpointed attempt for a work item
-- (session_id + checkpoint_uri set). This partial index serves both that lookup and
-- the resume-count tally (attempts carrying a session_id).
CREATE INDEX IF NOT EXISTS attempts_checkpointed
    ON attempts (work_item_id, started_at DESC)
    WHERE session_id IS NOT NULL;
