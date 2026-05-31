//! Record book, durable queue, leasing, capacity, and idempotency.
//!
//! Implemented in **P1** (see `docs/factory-implementation-plan.md`): Postgres
//! schema, `FOR UPDATE SKIP LOCKED` leasing, advisory-lock capacity enforcement,
//! the partial-unique idempotent-enqueue index, and the retention purge.
#![forbid(unsafe_code)]
