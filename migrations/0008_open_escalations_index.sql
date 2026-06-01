-- P6 (dashboard-api): partial index for the summary's open-escalations count.
--
-- The dashboard summary (run on every SSE tick) counts open escalations:
--   SELECT count(*) FROM escalations e JOIN work_items w ON w.id = e.work_item_id
--    WHERE w.repo_id = $1 AND e.state = 'open'
-- The only existing escalations index is the UNIQUE (work_item_id, reason) from
-- migration 0004, which can't filter out resolved rows — so a repo with many
-- historical (resolved) escalations would scan/filter them once per client per tick.
-- A PARTIAL index keyed by work_item_id over only open rows makes the join probe
-- cheap and stays tiny (open escalations are few). Additive (expand-phase).

CREATE INDEX IF NOT EXISTS escalations_open_by_item
    ON escalations (work_item_id)
    WHERE state = 'open';
