-- P6 (dashboard-api): indexes for the read-only projection / SSE "Fresh view" path.
--
-- The dashboard's work-items list (and every SSE poll tick) runs
--   WHERE repo_id = $1 ORDER BY updated_at DESC, id DESC LIMIT $2
-- and the repo events feed runs
--   WHERE repo_id = $1 ORDER BY at DESC, id DESC LIMIT $2.
-- Neither was covered by an existing index (those target pickup / in-flight /
-- terminal-purge / per-item / per-correlation paths), so at scale each request would
-- scan + sort the repo's rows. These composite indexes make both bounded queries an
-- index range scan. Additive (expand-phase), safe for a rolling deploy.

CREATE INDEX IF NOT EXISTS work_items_repo_recent
    ON work_items (repo_id, updated_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS events_by_repo_recent
    ON events (repo_id, at DESC, id DESC);
