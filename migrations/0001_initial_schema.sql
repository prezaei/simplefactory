-- Factory record book — initial schema (P1).
-- UUIDv7 PKs (minted app-side), text + CHECK enums, partial indexes for the
-- idempotent-enqueue guard and the SKIP LOCKED pickup. See docs/factory-technical-spec.md.

CREATE TABLE repos (
    id               uuid PRIMARY KEY,
    installation_id  bigint NOT NULL,
    owner            text   NOT NULL,
    name             text   NOT NULL,
    config           jsonb,
    config_sha       text,
    status           text   NOT NULL DEFAULT 'active'
                       CHECK (status IN ('active', 'suspended', 'removed')),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner, name)
);

CREATE TABLE work_items (
    id               uuid PRIMARY KEY,
    repo_id          uuid NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    kind             text NOT NULL CHECK (kind IN ('manager', 'coder', 'reviewer', 'tester')),
    worker_name      text NOT NULL,
    config_sha       text NOT NULL,
    trigger          text NOT NULL CHECK (trigger IN
                       ('issue_labeled', 'cr_opened', 'cr_updated', 'cr_changes_requested',
                        'review_requested', 'issue_comment', 'schedule', 'manager')),
    subject_ref      text NOT NULL,
    correlation_id   uuid NOT NULL,
    state            text NOT NULL DEFAULT 'queued'
                       CHECK (state IN ('queued', 'leased', 'running', 'blocked',
                                        'succeeded', 'failed', 'escalated')),
    priority         int  NOT NULL DEFAULT 0,
    available_at     timestamptz NOT NULL DEFAULT now(),
    lease_owner      text,
    lease_expires_at timestamptz,
    attempt_count    int  NOT NULL DEFAULT 0,
    loop_count       int  NOT NULL DEFAULT 0,
    payload          jsonb NOT NULL DEFAULT '{}',
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- Idempotent enqueue: at most one ACTIVE item per (repo, subject, kind);
-- terminal rows do not block a later re-run.
CREATE UNIQUE INDEX work_items_active_uniq ON work_items (repo_id, subject_ref, kind)
    WHERE state IN ('queued', 'leased', 'running', 'blocked');

-- SKIP LOCKED pickup: ready, unleased items per (repo, worker_name),
-- highest-priority first then oldest (matches lease ORDER BY priority DESC, available_at).
CREATE INDEX work_items_pickup ON work_items (repo_id, worker_name, priority DESC, available_at)
    WHERE state = 'queued';

-- Capacity counts: lease_next counts in-flight items by repo_id (global cap) and by
-- (repo_id, worker_name) (per-worker cap). This partial index serves both predicates
-- (repo_id is a leading column), keeping the cap checks off a sequential scan.
CREATE INDEX work_items_inflight ON work_items (repo_id, worker_name)
    WHERE state IN ('leased', 'running');

-- Sweeper: find expired leases fast (reclaim_expired filters lease_expires_at + leased/running).
CREATE INDEX work_items_lease_expiry ON work_items (lease_expires_at)
    WHERE state IN ('leased', 'running');

-- Retention: find old terminal rows fast (purge_terminal filters updated_at + terminal state).
CREATE INDEX work_items_terminal_purge ON work_items (updated_at)
    WHERE state IN ('succeeded', 'failed', 'escalated');

CREATE TABLE attempts (
    id               uuid PRIMARY KEY,
    work_item_id     uuid NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    job_execution_id text,
    session_id       text,
    checkpoint_uri   text,
    transcript_uri   text,
    status           text NOT NULL DEFAULT 'running'
                       CHECK (status IN ('running', 'succeeded', 'failed', 'blocked', 'reclaimed')),
    summary          text,
    started_at       timestamptz NOT NULL DEFAULT now(),
    ended_at         timestamptz
);
CREATE INDEX attempts_by_item ON attempts (work_item_id);

CREATE TABLE issues (
    id            uuid PRIMARY KEY,
    repo_id       uuid NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    number        int  NOT NULL,
    factory_state text NOT NULL DEFAULT 'open'
                    CHECK (factory_state IN ('open', 'in_progress', 'blocked', 'done')),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, number)
);

CREATE TABLE change_requests (
    id            uuid PRIMARY KEY,
    repo_id       uuid NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    number        int,
    head_branch   text NOT NULL,
    linked_issue  int,
    work_item_id  uuid REFERENCES work_items(id) ON DELETE SET NULL,
    factory_state text NOT NULL DEFAULT 'draft'
                    CHECK (factory_state IN ('draft', 'open', 'changes_requested',
                                             'approved', 'ready', 'merged', 'closed')),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, number)
);

CREATE TABLE escalations (
    id           uuid PRIMARY KEY,
    work_item_id uuid NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    reason       text NOT NULL CHECK (reason IN
                   ('retries_exhausted', 'loop_cap', 'needs_decision', 'blocked_input', 'conflict')),
    context      text NOT NULL,
    delivered_to text,
    state        text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'resolved')),
    created_at   timestamptz NOT NULL DEFAULT now(),
    resolved_at  timestamptz
);

CREATE TABLE events (
    id             uuid PRIMARY KEY,
    repo_id        uuid REFERENCES repos(id) ON DELETE CASCADE,
    work_item_id   uuid REFERENCES work_items(id) ON DELETE CASCADE,
    correlation_id uuid,
    actor          text NOT NULL,
    action         text NOT NULL,
    detail         jsonb NOT NULL DEFAULT '{}',
    at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX events_by_item ON events (work_item_id, at);
CREATE INDEX events_by_corr ON events (correlation_id, at);

CREATE TABLE webhook_deliveries (
    delivery_id text PRIMARY KEY,
    received_at timestamptz NOT NULL DEFAULT now()
);
