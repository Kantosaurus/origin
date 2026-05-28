//! Open a session, take a turn, SIGKILL the daemon; next daemon's
//! `list_sessions` includes the same session at the same turn.
//!
//! Strategy: drive the IPC client from the supervisor's test process; assert
//! on the post-restart session list.
//!
//! The detailed end-to-end wiring relies on `session_store` + protocol changes
//! shipped in P12.12 and on the supervisor's `resume_token` writer. The full
//! fixture lands in P14 polish; for P12 the test exists with the correct
//! shape so Linux CI can flesh it out, and is `#[ignore]`d so the workspace
//! `cargo test` stays green on hosts that can't drive the daemon binary
//! end-to-end (notably Windows, where SIGKILL has no equivalent).

#[cfg(unix)]
mod unix_only {
    use origin_resume_token::ResumeToken;
    use origin_supervisor::ipc_resume;
    use origin_supervisor::resume_token;

    #[test]
    #[ignore = "P14 polish: needs a real origin-daemon binary + IPC fixture"]
    fn session_resumes_after_kill() {
        // 1. Spawn supervisor with a real daemon path.
        // 2. Connect IPC client, open session "S".
        // 3. Send one prompt; await assistant completion.
        // 4. SIGKILL the daemon.
        // 5. Wait for supervisor restart (< 2 s).
        // 6. Connect a fresh IPC client; call `list_sessions`.
        // 7. Assert "S" is present with `last_turn == 1`.
        //
        // The detailed wiring relies on session_store + protocol changes
        // shipped in P12.12 and on the supervisor's resume_token writer.
    }

    /// Smoke check that the supervisor's resume helpers compile and link
    /// against the shared `origin-resume-token` shape. Runs on every host
    /// so a refactor that breaks the leaf-crate dependency surfaces here.
    #[test]
    fn resume_token_round_trip_through_supervisor_aliases() {
        let tmp = tempfile::tempdir().expect("tmp");
        let token = resume_token::ResumeToken {
            session_id: "smoke".into(),
            last_turn: 0,
            cas_handle_root: [0u8; 32],
            pending_tool_calls: Vec::new(),
            plan_seq: 0,
            goal: None,
        };
        token.save(tmp.path()).expect("save");
        let loaded: Vec<ResumeToken> = ResumeToken::load_all(tmp.path()).expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session_id, "smoke");
        // Compile-time check: the replay surface exists and is async.
        let _ = std::ptr::addr_of!(ipc_resume::replay_all);
    }
}
