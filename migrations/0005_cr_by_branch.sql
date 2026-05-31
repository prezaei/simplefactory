-- P4 FIX: key a change request to its head branch so the rework loop reuses the
-- SAME CR row across coder work items.
--
-- On `changes_requested` the coordinator re-enqueues a NEW coder work item for the
-- issue. The CR was previously idempotent BY WORK ITEM
-- (`change_requests_by_item`), so the rework coder's `open_or_update_change_request`
-- could not reuse the original CR row: it either tried to INSERT a second row that
-- collided on `UNIQUE(repo_id, number)` (GitHub returns the same PR number for the
-- same branch), or left the new work item with no CR row so `decide_coder_succeeded`
-- no-opped and never woke the reviewer. Either way the rework loop wedged.
--
-- The CR identity is the head branch (deterministic per issue: `factory/issue-N`),
-- so a rework coder pushing the same branch re-points the existing CR row to itself
-- rather than creating a colliding one.

-- Drop the per-work-item identity: a CR row's `work_item_id` must be re-pointable to
-- the CURRENT owning (rework) coder, so it can no longer be the unique key.
DROP INDEX change_requests_by_item;

-- The CR identity is now the head branch (one CR per branch per repo). `head_branch`
-- is NOT NULL, so every CR participates.
CREATE UNIQUE INDEX change_requests_by_branch
    ON change_requests (repo_id, head_branch);
