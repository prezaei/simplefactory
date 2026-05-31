# Migrations

`sqlx` migrations for the Postgres record book. Applied with `sqlx migrate run`
and gated in CI (a single-runner step before service rollout, per the technical
spec).

The actual schema migrations land in **P1** (`work_items`, `attempts`, `repos`,
`issues`, `change_requests`, `escalations`, `events`, `webhook_deliveries`).
This directory exists from P0 so the tooling and CI gate are wired first.
