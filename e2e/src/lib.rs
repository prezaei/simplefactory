//! Factory end-to-end acceptance suite.
//!
//! Every behavior the specs promise has exactly one named test here, authored
//! up front (P0) as a compiling `#[ignore]`d stub. A later phase **un-ignores**
//! and implements the tests it satisfies; a phase is not done while a test it
//! owns is still ignored. Run `cargo test -p factory-e2e -- --list` to see the
//! burndown. These drive the public entry points against the local stack
//! (docker-compose + Azurite), with Claude Code faked at the seam for
//! determinism.
#![forbid(unsafe_code)]

#[cfg(test)]
mod acceptance {
    // P1 — record book / queue
    #[test]
    #[ignore = "P1: implement record book + SKIP LOCKED leasing"]
    fn no_double_starts_under_concurrent_coordinators() {
        todo!("P1: N coordinators lease M items → each item leased exactly once")
    }

    #[test]
    #[ignore = "P1: implement advisory-lock capacity enforcement"]
    fn capacity_caps_never_exceeded() {
        todo!("P1: concurrent leasers vs cap K → running count never exceeds K (per-worker AND global)")
    }

    // P2 — intake / opt-in / config application
    #[test]
    #[ignore = "P2: implement opt-in label gating in intake"]
    fn opt_in_gating_unlabelled_issue_is_ignored() {
        todo!("P2: an issue WITHOUT the opt-in label produces no work item")
    }

    #[test]
    #[ignore = "P2: implement config apply-on-push with fail-safe validation"]
    fn invalid_config_is_rejected_and_last_good_kept() {
        todo!("P2: pushing an invalid .factory/config.yml is rejected; the previous good config stays in effect")
    }

    // P4 — coordinator + reviewer (full loop)
    #[test]
    #[ignore = "P4: implement manager + coder + reviewer loop"]
    fn worked_example_happy_path_issue_to_merge_handoff() {
        todo!("P4: labelled issue → Manager → Coder (tests+code) → CR → Reviewer approve → ready-for-human-merge → issue closed")
    }

    #[test]
    #[ignore = "P4: implement bounded review-rework loop + cap"]
    fn changes_requested_rework_loop_increments_and_caps() {
        todo!("P4: repeated changes-requested increments loop_count; at the cap the work escalates instead of looping forever")
    }

    #[test]
    #[ignore = "P4: implement exactly-once escalation"]
    fn exactly_once_escalation_under_redispatch() {
        todo!("P4: a lease-expiry re-dispatch during/after escalation yields exactly ONE escalation (one comment + one assignment)")
    }

    #[test]
    #[ignore = "P4: implement the can't-weaken-own-checks guard"]
    fn cannot_weaken_own_gating_checks_is_escalated() {
        todo!("P4: a change that weakens/disables the checks gating its own work is rejected → escalated, never marked ready")
    }

    // P5 — resilience
    #[test]
    #[ignore = "P5: implement needs-human-input blocked→resume"]
    fn needs_human_input_blocks_then_resumes_same_session_on_reply() {
        todo!("P5: worker hits high-stakes ambiguity → blocked + question on issue; a human reply resumes the SAME session")
    }

    #[test]
    #[ignore = "P5: implement checkpoint + claude --resume"]
    fn crash_resume_continues_the_same_session() {
        todo!("P5: kill a worker mid-run → the next dispatch resumes its same Claude Code session from checkpoint")
    }

    #[test]
    #[ignore = "P5/P1: implement retention purge + Blob lifecycle"]
    fn retention_aging_removes_only_terminal_records() {
        todo!("P5/P1: terminal records past the retention window are purged; active work is untouched")
    }
}
