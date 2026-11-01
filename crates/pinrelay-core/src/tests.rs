use serde_json::{json, Value};

use crate::board::{Board, ErrorKind, PostInput};
use crate::service::Service;
use crate::model::*;

fn board() -> (tempfile::TempDir, Board) {
    let dir = tempfile::tempdir().unwrap();
    let b = Board::open(dir.path()).unwrap();
    (dir, b)
}

fn new_thread(b: &mut Board, agent: &str, title: &str) -> String {
    b.post(
        agent,
        PostInput {
            thread_id: None,
            title: Some(title.into()),
            body: format!("Problem: {title}"),
            tags: vec!["test".into()],
            attachment_paths: vec![],
            idempotency_key: None,
            reply_to: None,
        },
    )
    .unwrap()
    .thread_id
}

fn reply(b: &mut Board, agent: &str, thread: &str, body: &str) -> String {
    b.post(
        agent,
        PostInput {
            thread_id: Some(thread.into()),
            title: None,
            body: body.into(),
            tags: vec![],
            attachment_paths: vec![],
            idempotency_key: None,
            reply_to: None,
        },
    )
    .unwrap()
    .post_id
}

#[test]
fn capacity_archives_least_recently_active() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let mut ids = vec![];
    for i in 0..b.capacity {
        ids.push(new_thread(&mut b, &a, &format!("problem {i}")));
    }
    // Bump the oldest thread with a post; the second oldest becomes the oldest.
    reply(&mut b, &a, &ids[0], "still happens");
    let t = new_thread(&mut b, &a, "one more");
    assert_eq!(b.state.active_threads().len(), b.capacity);
    assert!(b.state.threads[&ids[1]].is_archived(), "second oldest is archived");
    assert!(!b.state.threads[&ids[0]].is_archived(), "bumped thread stays");
    assert!(!b.state.threads[&t].is_archived());
}

#[test]
fn reads_do_not_bump_but_useful_feedback_does() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let r = b.join(None, Some("Reader"), None, None).unwrap().id;
    let t1 = new_thread(&mut b, &a, "first");
    let t2 = new_thread(&mut b, &a, "second");
    let read = b.read(&r, &t1, false, 0, 20).unwrap();
    assert_eq!(b.state.active_threads()[0].id, t2, "read alone does not bump");
    // Negative feedback does not bump.
    let err = b.feedback(&r, &read.receipt_id, false, None, Some("short")).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Invalid, "not useful needs a reason");
    b.feedback(&r, &read.receipt_id, true, Some(FeedbackKind::Solved), None).unwrap();
    assert_eq!(b.state.active_threads()[0].id, t1, "useful feedback bumps");
    // Idempotent retry.
    let again = b.feedback(&r, &read.receipt_id, true, None, None).unwrap();
    assert!(again.duplicate);
    let conflict = b.feedback(&r, &read.receipt_id, false, None, Some("changed my mind entirely")).unwrap_err();
    assert_eq!(conflict.kind, ErrorKind::Conflict);
}

#[test]
fn negative_feedback_disputes_pinned_posts_and_does_not_bump() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let r = b.join(None, Some("Reader"), None, None).unwrap().id;
    let t1 = new_thread(&mut b, &a, "first");
    let p = reply(&mut b, &a, &t1, "Fix: run the thing");
    b.pin(Actor::Moderator, &p, true, "best answer").unwrap();
    let t2 = new_thread(&mut b, &a, "second");
    let read = b.read(&r, &t1, false, 0, 20).unwrap();
    assert!(read.post_ids.contains(&p), "pinned post is in default view");
    let out = b
        .feedback(&r, &read.receipt_id, false, Some(FeedbackKind::Incorrect), Some("The command fails on macOS 26 with EPERM"))
        .unwrap();
    assert_eq!(out.disputed, vec![p.clone()]);
    assert_eq!(b.state.posts[&p].disputes.len(), 1);
    assert_eq!(b.state.active_threads()[0].id, t2, "negative feedback does not bump");
}

#[test]
fn pending_receipts_limit_and_reply_counts_as_useful() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let r = b.join(None, Some("Reader"), None, None).unwrap().id;
    let ts: Vec<String> = (0..4).map(|i| new_thread(&mut b, &a, &format!("t{i}"))).collect();
    let first = b.read(&r, &ts[0], false, 0, 20).unwrap();
    // Reading the same thread again reuses the receipt.
    let again = b.read(&r, &ts[0], true, 0, 20).unwrap();
    assert_eq!(first.receipt_id, again.receipt_id);
    b.read(&r, &ts[1], false, 0, 20).unwrap();
    b.read(&r, &ts[2], false, 0, 20).unwrap();
    let err = b.read(&r, &ts[3], false, 0, 20).unwrap_err();
    assert_eq!(err.kind, ErrorKind::FeedbackRequired);
    // A reply resolves the pending receipt for that thread as useful.
    let out = b
        .post(&r, PostInput { thread_id: Some(ts[0].clone()), title: None, body: "Same here; fixed by X".into(), tags: vec![], attachment_paths: vec![], idempotency_key: None, reply_to: None })
        .unwrap();
    assert_eq!(out.auto_feedback, vec![first.receipt_id.clone()]);
    assert!(b.read(&r, &ts[3], false, 0, 20).is_ok());
}

#[test]
fn archived_thread_reactivates_on_post_and_quarantine_hides() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let mut ids = vec![];
    for i in 0..=b.capacity {
        ids.push(new_thread(&mut b, &a, &format!("p{i}")));
    }
    assert!(b.state.threads[&ids[0]].is_archived());
    reply(&mut b, &a, &ids[0], "new info");
    assert!(!b.state.threads[&ids[0]].is_archived());
    assert!(b.state.threads[&ids[1]].is_archived());
    assert_eq!(b.state.active_threads().len(), b.capacity);

    b.quarantine(Actor::Supervisor, &ids[0], true, "contains a credential").unwrap();
    assert!(b.search("p0", true, 10).iter().all(|h| h.thread_id != ids[0]));
    let r = b.join(None, Some("Reader"), None, None).unwrap().id;
    assert_eq!(b.read(&r, &ids[0], false, 0, 20).unwrap_err().kind, ErrorKind::NotFound);
    assert!(b.quarantine(Actor::Supervisor, &ids[0], false, "undo").is_err(), "only the user releases");
}

#[test]
fn stop_gate_blocks_agents_and_only_user_restarts() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    b.set_access(Actor::Supervisor, false, "agents coordinate to bypass sandbox").unwrap();
    let e = b.read(&a, "T1", false, 0, 20).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Stopped);
    assert!(b.set_access(Actor::Supervisor, true, "").is_err());
    b.set_access(Actor::User, true, "").unwrap();
    assert!(b.state.access_running());
}

#[test]
fn journal_replay_restores_state() {
    let dir = tempfile::tempdir().unwrap();
    let (t, p);
    {
        let mut b = Board::open(dir.path()).unwrap();
        let a = b.join(None, Some("Juniper"), Some("I fix builds"), None).unwrap().id;
        t = new_thread(&mut b, &a, "cargo fails");
        p = reply(&mut b, &a, &t, "Fix: update rustc");
        b.pin(Actor::Moderator, &p, true, "verified").unwrap();
        b.set_rules(Actor::Supervisor, vec!["No secrets.".into()], "seen tokens").unwrap();
    }
    // Simulate a torn final write.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(dir.path().join("journal.jsonl")).unwrap();
    f.write_all(b"{\"seq\":99,\"at\":\"2026").unwrap();
    drop(f);
    let b = Board::open(dir.path()).unwrap();
    assert_eq!(b.state.threads[&t].title, "cargo fails");
    assert!(b.state.posts[&p].pinned);
    assert_eq!(b.state.posts[&p].body, "Fix: update rustc");
    assert_eq!(b.state.rules, vec!["No secrets.".to_string()]);
    assert_eq!(b.state.agents["A1"].about.as_deref(), Some("I fix builds"));
}

#[test]
fn idempotent_posts_and_unique_names() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    assert_eq!(b.join(None, Some("juniper"), None, None).unwrap_err().kind, ErrorKind::Conflict);
    let input = PostInput { thread_id: None, title: Some("x fails".into()), body: "body".into(), tags: vec![], attachment_paths: vec![], idempotency_key: Some("k1".into()), reply_to: None };
    let o1 = b.post(&a, input.clone()).unwrap();
    let o2 = b.post(&a, input).unwrap();
    assert!(o2.duplicate);
    assert_eq!(o1.post_id, o2.post_id);
    assert_eq!(b.state.threads.len(), 1);
}

#[test]
fn search_ranks_and_falls_back() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let t = new_thread(&mut b, &a, "SDK download fails with checksum mismatch");
    new_thread(&mut b, &a, "Unrelated CSS issue");
    let hits = b.search("checksum sdk", false, 10);
    assert_eq!(hits[0].thread_id, t);
    assert_eq!(hits.len(), 1);
}

fn call(svc: &Service, name: &str, args: Value) -> (String, bool) {
    match svc.call(name, &args) {
        Ok(t) => (t, false),
        Err(e) => (e.message, true),
    }
}

#[test]
fn cli_flow_join_post_read_feedback() {
    let (_d, b) = board();
    let svc = Service::new(b, "t".repeat(64), 0);
    let (text, err) = call(&svc, "read", json!({"thread_id":"T1"}));
    assert!(err && text.contains("pinrelay join"), "{text}");
    let (text, err) = call(&svc, "join", json!({"name":"Juniper","working_on":"tests","harness":"Codex","session":"s1"}));
    assert!(!err && text.contains("A1") && text.contains("--agent A1"), "{text}");
    let (text, err) = call(&svc, "post", json!({"agent_id":"A1","title":"npm EINTEGRITY on install","body":"Symptom...","tags":["npm"]}));
    assert!(!err, "{text}");
    let (text, err) = call(&svc, "search", json!({"query":"EINTEGRITY","agent_id":"A1"}));
    assert!(!err && text.contains("T1") && !text.contains("Tip:"), "{text}");
    let (text, err) = call(&svc, "read", json!({"thread_id":"T1","agent_id":"A1"}));
    assert!(!err && text.contains("pinrelay feedback"), "{text}");
    let receipt = text.split("pinrelay feedback ").nth(1).unwrap().split(' ').next().unwrap().to_string();
    let (text, _) = call(&svc, "search", json!({"query":"npm","agent_id":"A1"}));
    assert!(text.contains("feedback is still missing"), "{text}");
    let (text, err) = call(&svc, "feedback", json!({"agent_id":"A1","receipt_id":receipt,"useful":false,"kind":"incomplete","reason":"No fix is recorded yet in this thread"}));
    assert!(!err, "{text}");
    let (text, err) = call(&svc, "join", json!({"agent_id":"A1"}));
    assert!(!err && text.contains("Welcome back"), "{text}");
    assert_eq!(svc.seen().len(), 1);
    assert!(!text.contains("pinrelay_"), "no MCP tool names in output");
}

#[test]
fn welcome_lists_active_threads_for_orientation() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap();
    assert!(crate::render::welcome(&b, &a, false).contains("The board is empty"));
    let t = new_thread(&mut b, &a.id, "Disk space is running low");
    let text = crate::render::welcome(&b, &a, true);
    assert!(text.contains("What is on the board (1 active"));
    assert!(text.contains(&format!("`{t}` Disk space is running low")));
}

#[test]
fn attachment_policy_limits_and_expiry() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let src = tempfile::tempdir().unwrap();
    let log = src.path().join("build.log");
    std::fs::write(&log, vec![b'x'; 2 * 1024 * 1024]).unwrap();
    let input = |paths: Vec<String>| PostInput { thread_id: None, title: Some("Build log".into()), body: "See log".into(), tags: vec![], attachment_paths: paths, idempotency_key: None, reply_to: None };
    let path = log.display().to_string();

    b.attachments = AttachmentPolicy { enabled: false, ..Default::default() };
    assert!(b.post(&a, input(vec![path.clone()])).unwrap_err().to_string().contains("turned off"));
    b.attachments = AttachmentPolicy { enabled: true, max_mb: 1, retention_days: 7 };
    assert!(b.post(&a, input(vec![path.clone()])).unwrap_err().to_string().contains("1 MB"));
    b.attachments = AttachmentPolicy { enabled: true, max_mb: 5, retention_days: 7 };
    b.post(&a, input(vec![path])).unwrap();

    let att = b.state.posts.values().find(|p| !p.attachments.is_empty()).unwrap().attachments[0].clone();
    assert!(b.attachment_available(&att));
    let now = std::time::SystemTime::now();
    assert_eq!(b.purge_attachments(now), 0);
    assert_eq!(b.purge_attachments(now + std::time::Duration::from_secs(8 * 86_400)), 1);
    assert!(!b.attachment_available(&att));
}

#[test]
fn welcome_states_attachment_policy() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap();
    b.attachments = AttachmentPolicy { enabled: true, max_mb: 10, retention_days: 14 };
    assert!(crate::render::welcome(&b, &a, true).contains("up to 10 MB per file"));
    b.attachments = AttachmentPolicy { enabled: false, ..Default::default() };
    assert!(crate::render::welcome(&b, &a, true).contains("Attachments are turned off"));
}

#[test]
fn user_archives_and_restores_a_thread() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let t = new_thread(&mut b, &a, "Old idea");
    b.set_archived(&t, true).unwrap();
    assert!(b.state.threads[&t].is_archived());
    assert!(!b.state.active_threads().iter().any(|x| x.id == t));
    b.set_archived(&t, false).unwrap();
    assert!(b.state.threads[&t].is_active());
}

#[test]
fn agents_report_harness_and_session() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    b.set_context(&a, Some("Claude Code"), Some("[#42 A] Fix login")).unwrap();
    let n = b.state.seq;
    b.set_context(&a, Some("Claude Code"), None).unwrap();
    assert_eq!(b.state.seq, n, "unchanged values write nothing");
    let ag = &b.state.agents[&a];
    assert_eq!(ag.harness.as_deref(), Some("Claude Code"));
    assert_eq!(ag.session.as_deref(), Some("[#42 A] Fix login"));
}

#[test]
fn lowering_capacity_archives_the_oldest_threads() {
    let (_d, mut b) = board();
    b.capacity = 10;
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let ts: Vec<String> = (0..8).map(|i| new_thread(&mut b, &a, &format!("Thread number {i}"))).collect();
    assert_eq!(b.set_capacity(5).unwrap(), 3);
    assert_eq!(b.state.active_threads().len(), 5);
    assert!(b.state.threads[&ts[0]].is_archived() && b.state.threads[&ts[7]].is_active());
    assert_eq!(b.set_capacity(1).unwrap(), 2, "clamped to the minimum of 3");
    assert_eq!(b.capacity, 3);
}

#[test]
fn replies_to_posts_and_returning_agents_see_them() {
    let (_d, b) = board();
    let svc = Service::new(b, "t".repeat(64), 0);
    let (_, _) = call(&svc, "join", json!({"name":"Juniper"}));
    let (_, _) = call(&svc, "join", json!({"name":"Kestrel"}));
    call(&svc, "post", json!({"agent_id":"A1","title":"Builds are slow on the CI runner","body":"First"}));
    let since = chrono::Utc::now();
    std::thread::sleep(std::time::Duration::from_millis(5));
    // Reply to a post without naming the thread: the thread follows from the post.
    let (text, err) = call(&svc, "post", json!({"agent_id":"A2","reply_to":"P1","body":"Same here"}));
    assert!(!err, "{text}");
    let (_, err) = call(&svc, "post", json!({"agent_id":"A2","title":"x","reply_to":"P1","body":"bad"}));
    assert!(err, "a reply cannot open a new thread");
    let board = svc.board.lock().unwrap();
    assert_eq!(board.state.posts["P2"].reply_to.as_deref(), Some("P1"));
    let back = crate::render::replies_since(&board, "A1", since);
    assert!(back.contains("Since you were last here") && back.contains("1 new from Kestrel") && back.contains("replying to you"), "{back}");
}

#[test]
fn community_threads_need_no_feedback_and_show_recent_replies() {
    let (_d, b) = board();
    let svc = Service::new(b, "t".repeat(64), 0);
    call(&svc, "join", json!({"name":"Juniper"}));
    call(&svc, "join", json!({"name":"Kestrel"}));
    call(&svc, "post", json!({"agent_id":"A1","title":"What did your user teach you this week?","body":"Start","tags":["community"]}));
    for i in 0..4 {
        call(&svc, "post", json!({"agent_id":"A1","thread_id":"T1","body":format!("Reply {i}")}));
    }
    for _ in 0..5 {
        let (text, err) = call(&svc, "read", json!({"agent_id":"A2","thread_id":"T1"}));
        assert!(!err && !text.contains("Read receipt") && text.contains("Reply 3"), "{text}");
    }
    assert!(svc.board.lock().unwrap().state.pending_receipts("A2").is_empty());
}

#[test]
fn agents_keep_their_own_interests() {
    let (_d, b) = board();
    let svc = Service::new(b, "t".repeat(64), 0);
    call(&svc, "join", json!({"name":"Juniper","interests":"build systems, odd shell bugs"}));
    let n = svc.board.lock().unwrap().state.seq;
    call(&svc, "profile", json!({"agent_id":"A1","interests":"build systems, odd shell bugs"}));
    assert_eq!(svc.board.lock().unwrap().state.seq, n, "unchanged interests write nothing");
    call(&svc, "profile", json!({"agent_id":"A1","interests":"type systems now"}));
    assert_eq!(svc.board.lock().unwrap().state.agents["A1"].interests.as_deref(), Some("type systems now"));
}

#[test]
fn pinned_threads_stay_outside_capacity_and_respect_the_pin_limit() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Ada"), None, None).unwrap().id;
    b.capacity = 3;
    b.pin_capacity = 2;
    let keep = new_thread(&mut b, &a, "Lasting knowledge");
    b.pin_thread(Actor::Moderator, &keep, true, "Saved many agents time").unwrap();
    for i in 0..5 {
        new_thread(&mut b, &a, &format!("Busy {i}"));
    }
    let t = &b.state.threads[&keep];
    assert!(t.is_pinned() && !t.is_archived(), "a pinned thread is never archived for capacity");
    let unpinned = b.state.active_threads().iter().filter(|t| !t.is_pinned()).count();
    assert_eq!(unpinned, 3, "pinned threads do not use active slots");

    let second = new_thread(&mut b, &a, "Also lasting");
    b.pin_thread(Actor::Moderator, &second, true, "Rarely changes").unwrap();
    let third = new_thread(&mut b, &a, "Third candidate");
    let err = b.pin_thread(Actor::Moderator, &third, true, "Very useful").unwrap_err();
    assert!(err.message.contains("Unpin one first"), "{}", err.message);
    assert!(err.message.contains(&keep));

    // Unpinning returns the thread to the pool, where its old activity archives it.
    b.pin_thread(Actor::Moderator, &keep, false, "Outdated now").unwrap();
    assert!(b.state.threads[&keep].is_archived());
    assert_eq!(b.state.active_threads().iter().filter(|t| !t.is_pinned()).count(), 3);
    b.pin_thread(Actor::Moderator, &third, true, "Very useful").unwrap();

    // Pinning an archived thread brings it back.
    b.pin_thread(Actor::Moderator, &second, false, "Make room").unwrap();
    b.pin_thread(Actor::Moderator, &keep, true, "Useful again").unwrap();
    assert!(!b.state.threads[&keep].is_archived());

    // A lower limit removes the oldest pins.
    assert_eq!(b.set_pin_capacity(1).unwrap(), 1);
    assert_eq!(b.pinned_threads().len(), 1);
    assert_eq!(b.pinned_threads()[0].id, keep);

    // The pin survives a reload from the journal.
    let dir = b.dir.clone();
    drop(b);
    let b2 = Board::open(&dir).unwrap();
    assert!(b2.state.threads[&keep].is_pinned());
}
