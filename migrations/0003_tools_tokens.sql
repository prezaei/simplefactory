-- P3: the Tools/MCP channel — per-task scoped tokens + tool-write idempotency.
--
-- The worker's only channel to Factory is the Tools service, authenticated by a
-- short-lived per-task token. We store only the SHA-256 hash of the token (never
-- the raw secret), mirroring how passwords are stored; the raw token is handed to
-- the dispatcher once and injected into the job. Resolving a presented token
-- re-hashes and looks up the row, then authorizes every call against THAT task's
-- own work item / repo (no cross-task access).
CREATE TABLE task_tokens (
    token_hash   text PRIMARY KEY,
    work_item_id uuid NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    repo_id      uuid NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    role         text NOT NULL CHECK (role IN ('manager', 'coder', 'reviewer', 'tester')),
    attempt_id   uuid REFERENCES attempts(id) ON DELETE SET NULL,
    expires_at   timestamptz NOT NULL,
    revoked      boolean NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX task_tokens_by_item ON task_tokens (work_item_id);

-- Idempotency for Tools writes that carry a dedupe_key (post_comment,
-- submit_review, escalate): the first call records the external result keyed by
-- (work_item, scope, dedupe_key); a re-run (e.g. lease-expiry re-dispatch)
-- returns the recorded result and updates the same external object rather than
-- creating a duplicate.
-- A change request is idempotent BY WORK ITEM: a lease-expiry re-dispatch must
-- reuse the same CR row rather than open a second one. (The table already has
-- UNIQUE(repo_id, number); this adds the per-work-item guard used before a number
-- exists on GitHub.)
CREATE UNIQUE INDEX change_requests_by_item
    ON change_requests (work_item_id) WHERE work_item_id IS NOT NULL;

CREATE TABLE tool_idempotency (
    work_item_id uuid NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    scope        text NOT NULL CHECK (scope IN ('comment', 'review', 'escalate')),
    dedupe_key   text NOT NULL,
    result       jsonb NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (work_item_id, scope, dedupe_key)
);
