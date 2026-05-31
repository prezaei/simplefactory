-- P4: the Coordinator (Manager) loop — react-once tracking, per-CR rework loop
-- count, and exactly-once escalation.

-- The coordinator reacts to each TERMINAL work item exactly once (assign the next
-- step, close the loop, or escalate). `reacted` is the claim flag; the partial
-- index keeps the coordinator's scan for un-reacted terminal items cheap.
ALTER TABLE work_items ADD COLUMN reacted boolean NOT NULL DEFAULT false;
CREATE INDEX work_items_unreacted ON work_items (updated_at)
    WHERE state IN ('succeeded', 'failed', 'escalated') AND NOT reacted;

-- Per-CR review-rework loop count (Coder<->Reviewer cycles). The coordinator
-- increments it on a changes-requested verdict and escalates at the cap.
ALTER TABLE change_requests ADD COLUMN loop_count int NOT NULL DEFAULT 0;

-- Exactly-once escalation: at most one escalation per (work item, reason), so a
-- lease-expiry re-dispatch (or a racing coordinator replica) can never create a
-- second escalation row for the same cause.
--
-- The old `create_escalation` inserted unconditionally, so an upgraded DB may already
-- hold duplicate (work_item_id, reason) rows that would make the unique index below
-- fail. Dedup first, keeping the most recent row per (work_item_id, reason) (latest
-- created_at, with ctid as a deterministic tiebreaker). On a fresh DB this affects 0
-- rows.
DELETE FROM escalations a
 USING escalations b
 WHERE a.work_item_id = b.work_item_id
   AND a.reason = b.reason
   AND (a.created_at, a.ctid) < (b.created_at, b.ctid);

CREATE UNIQUE INDEX escalations_by_item_reason ON escalations (work_item_id, reason);
