-- Scheduler state + dedupe-table retention (P2).
--
-- schedule_state records the last fired tick per (repo, worker) so the in-app
-- scheduler fires a due cron slot exactly once even across replicas: the fire is
-- taken under a per-(repo,worker) advisory lock that re-checks last_fired, and the
-- work_items idempotency index is a second backstop.
CREATE TABLE schedule_state (
    repo_id     uuid NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    worker_name text NOT NULL,
    last_fired  timestamptz NOT NULL,
    PRIMARY KEY (repo_id, worker_name)
);

-- The webhook dedupe table is load-bearing (a delivery id is only recorded after
-- a delivery is fully processed, so its presence means "already handled"). It
-- grows forever without pruning; this index lets retention purge old rows fast.
-- Safe because GitHub only redelivers within a short window.
CREATE INDEX webhook_deliveries_received ON webhook_deliveries (received_at);
