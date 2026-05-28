//! Bug #9 integration: GoalSnapshot must preserve `last_status_tag` so
//! a `Verifying` resume after a crash carries forward the `Met` claim
//! and the driver can re-run the verifier on the next tick.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_daemon::goal_checkpoint::make_goal_checkpoint_token;
use origin_daemon::session_store::SessionStore;
use origin_goal::{GoalState, GoalStatus, TagOutcome, TagOutcomeWire};

#[test]
fn snapshot_round_trip_preserves_last_status_tag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("sessions.db");

    let mut g = GoalState::new("x".into(), None, None);
    g.iter = 2;
    g.tokens_spent = 500;
    g.status = GoalStatus::Verifying;
    g.last_status_tag = Some(TagOutcome::Met);

    let token = make_goal_checkpoint_token("sess-last-tag", 4, &Some(g));

    {
        let store = SessionStore::open(&db_path).expect("open");
        store.save_resume_token(&token).expect("save");
    }
    let store2 = SessionStore::open(&db_path).expect("reopen");
    let loaded = store2
        .load_resume_token("sess-last-tag")
        .expect("load")
        .expect("token present");

    let snap = loaded.goal.expect("goal present");
    // Bug #9: this assertion fails before the fix (the field doesn't
    // exist on the wire). After the fix it must round-trip the Met tag.
    assert_eq!(
        snap.last_status_tag,
        Some(TagOutcomeWire::Met),
        "last_status_tag must round-trip through GoalSnapshot \
         (bug #9: pre-fix the field doesn't exist on the wire)"
    );
}

#[test]
fn snapshot_with_no_last_status_tag_round_trips_as_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("sessions.db");

    let g = GoalState::new("x".into(), None, None);
    // Fresh GoalState has last_status_tag = None.
    let token = make_goal_checkpoint_token("sess-no-tag", 0, &Some(g));

    {
        let store = SessionStore::open(&db_path).expect("open");
        store.save_resume_token(&token).expect("save");
    }
    let store2 = SessionStore::open(&db_path).expect("reopen");
    let loaded = store2
        .load_resume_token("sess-no-tag")
        .expect("load")
        .expect("token present");

    let snap = loaded.goal.expect("goal present");
    assert_eq!(snap.last_status_tag, None);
}
