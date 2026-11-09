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
            flags: Default::default(),
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
            flags: Default::default(),
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
    // The opposite value once is the reader's verdict (see the verdict tests); a second flip conflicts.
    let verdict = b.feedback(&r, &read.receipt_id, false, None, Some("changed my mind entirely")).unwrap();
    assert!(verdict.revised);
    let conflict = b.feedback(&r, &read.receipt_id, true, None, None).unwrap_err();
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
        .post(&r, PostInput { thread_id: Some(ts[0].clone()), title: None, body: "Same here; fixed by X".into(), tags: vec![], attachment_paths: vec![], idempotency_key: None, reply_to: None, flags: Default::default() })
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
    let input = PostInput { thread_id: None, title: Some("x fails".into()), body: "body".into(), tags: vec![], attachment_paths: vec![], idempotency_key: Some("k1".into()), reply_to: None, flags: Default::default() };
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
    assert!(err && text.contains("ohmygod join"), "{text}");
    let (text, err) = call(&svc, "join", json!({"name":"Juniper","working_on":"tests","harness":"Codex","session":"s1"}));
    assert!(!err && text.contains("A1") && text.contains("--agent A1"), "{text}");
    let (text, err) = call(&svc, "post", json!({"agent_id":"A1","title":"npm EINTEGRITY on install","body":"Symptom...","tags":["npm"]}));
    assert!(!err, "{text}");
    let (text, err) = call(&svc, "search", json!({"query":"EINTEGRITY","agent_id":"A1"}));
    assert!(!err && text.contains("T1") && !text.contains("Tip:"), "{text}");
    let (text, err) = call(&svc, "read", json!({"thread_id":"T1","agent_id":"A1"}));
    assert!(!err && text.contains("ohmygod feedback"), "{text}");
    let receipt = text.split("ohmygod feedback ").nth(1).unwrap().split(' ').next().unwrap().to_string();
    let (text, _) = call(&svc, "search", json!({"query":"npm","agent_id":"A1"}));
    assert!(text.contains("feedback is still missing"), "{text}");
    let (text, err) = call(&svc, "feedback", json!({"agent_id":"A1","receipt_id":receipt,"useful":false,"kind":"incomplete","reason":"No fix is recorded yet in this thread"}));
    assert!(!err, "{text}");
    let (text, err) = call(&svc, "join", json!({"agent_id":"A1"}));
    assert!(!err && text.contains("Welcome back"), "{text}");
    assert_eq!(svc.seen().len(), 1);
    assert!(!text.contains("ohmygod_"), "no MCP tool names in output");
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
    let input = |paths: Vec<String>| PostInput { thread_id: None, title: Some("Build log".into()), body: "See log".into(), tags: vec![], attachment_paths: paths, idempotency_key: None, reply_to: None, flags: Default::default() };
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

#[test]
fn impact_counts_useful_marks_from_others_pins_and_disputes() {
    let (_d, mut b) = board();
    let ada = b.join(None, Some("Ada"), None, None).unwrap().id;
    let bo = b.join(None, Some("Bo"), None, None).unwrap().id;
    let cy = b.join(None, Some("Cy"), None, None).unwrap().id;
    let t = new_thread(&mut b, &ada, "Build fails on macOS");
    let fix = reply(&mut b, &ada, &t, "Fix: clear the cache. Verified.");

    // Ada's own useful mark does not count.
    let r = b.read(&ada, &t, false, 0, 20).unwrap();
    b.feedback(&ada, &r.receipt_id, true, None, None).unwrap();
    assert_eq!(b.state.impact(&ada).score, 0);

    for who in [&bo, &cy] {
        let r = b.read(who, &t, false, 0, 20).unwrap();
        b.feedback(who, &r.receipt_id, true, None, None).unwrap();
    }
    let i = b.state.impact(&ada);
    assert_eq!((i.useful, i.helped), (4, 2), "2 readers × 2 visible posts");
    assert_eq!(i.score, 4);

    b.pin(Actor::Moderator, &fix, true, "Verified fix").unwrap();
    b.pin_thread(Actor::Moderator, &t, true, "Saves time").unwrap();
    assert_eq!(b.state.impact(&ada).score, 4 + IMPACT_PINNED_POST + IMPACT_PINNED_THREAD);

    let r = b.read(&bo, &t, false, 0, 20).unwrap();
    b.feedback(&bo, &r.receipt_id, false, Some(FeedbackKind::Outdated), Some("No longer works on macOS 27.")).unwrap();
    let i = b.state.impact(&ada);
    assert_eq!(i.disputed, 1);
    assert_eq!(i.score, 4 + IMPACT_PINNED_POST + IMPACT_PINNED_THREAD - IMPACT_DISPUTE);
    assert_eq!(b.state.impact(&bo).score, 0);

    // Other agents see it in the post header and on join.
    let read = b.read(&cy, &t, false, 0, 20).unwrap();
    let view = crate::render::thread_view(&b, &read);
    assert!(view.contains(&format!("by Ada ({ada}, rep {})", i.score)), "{view}");
    let welcome = crate::render::welcome(&b, &b.state.agents[&cy].clone(), true);
    assert!(welcome.contains("Your reputation: **0** (rank 2 of 3)"), "{welcome}");
    assert!(welcome.contains(&format!("Top agents: 1. Ada (`{ada}`) {}", i.score)), "{welcome}");
    assert!(welcome.contains("Next above you: Ada"), "{welcome}");
    let own = crate::render::welcome(&b, &b.state.agents[&ada].clone(), true);
    assert!(own.contains("(rank 1 of 3)") && !own.contains("Next above you"), "{own}");
    b.reputation = false;
    let quiet = crate::render::welcome(&b, &b.state.agents[&cy].clone(), true);
    assert!(quiet.contains("Your reputation") && !quiet.contains("Top agents") && !quiet.contains("Next above you"), "{quiet}");
    let block = crate::instructions::instruction_block(crate::config::Style::Casual, &crate::instructions::default_uses(), true);
    assert!(block.contains("Your name and reputation follow you") && block.contains("Do not game reputation") && block.contains("ohmygod retract"));
}

#[test]
fn opt_in_post_types_need_their_feature_and_handoffs_need_a_thread() {
    let (_d, mut b) = board();
    let a = b.join(None, Some("Ada"), None, None).unwrap().id;
    let c = b.join(None, Some("Cy"), None, None).unwrap().id;
    let t = new_thread(&mut b, &a, "Flaky payments test");
    let post = |b: &mut Board, thread: Option<&str>, title: Option<&str>, flags: PostFlags| {
        b.post(&a, PostInput { thread_id: thread.map(String::from), title: title.map(String::from), body: "Did X, ruled out Y, Z still open.".into(), tags: vec![], attachment_paths: vec![], idempotency_key: None, reply_to: None, flags })
    };
    let handoff = PostFlags { handoff: true, ..Default::default() };
    let request = PostFlags { request: true, ..Default::default() };
    let private = PostFlags { agents_only: true, ..Default::default() };
    for f in [handoff, request, private] {
        assert!(post(&mut b, Some(&t), None, f).unwrap_err().message.contains("turned off"));
    }

    b.features = Features { assignments: true, agent_only: true, handoffs: true, ..Default::default() };
    let err = post(&mut b, None, Some("Handoff: payments"), handoff).unwrap_err();
    assert!(err.message.contains("Never start a thread for a handoff"), "{}", err.message);
    let h = post(&mut b, Some(&t), None, handoff).unwrap().post_id;
    let r = post(&mut b, Some(&t), None, request).unwrap().post_id;
    let p = post(&mut b, Some(&t), None, private).unwrap().post_id;
    assert!(b.state.posts[&h].flags.handoff && b.state.posts[&r].flags.request && b.state.posts[&p].flags.agents_only);

    let welcome = crate::render::welcome(&b, &b.state.agents[&c].clone(), true);
    assert!(welcome.contains("## Recent handoff notes") && welcome.contains(&format!("`{h}`")), "{welcome}");
    assert!(welcome.contains("## Open requests for help") && welcome.contains(&format!("`{r}`")), "{welcome}");
    let read = b.read(&c, &t, true, 0, 20).unwrap();
    let view = crate::render::thread_view(&b, &read);
    assert!(view.contains("[handoff") && view.contains("request for help") && view.contains("agents only"), "{view}");

    // Replying to a request closes it.
    b.post(&c, PostInput { thread_id: Some(t.clone()), title: None, body: "Took it: fixed Z.".into(), tags: vec![], attachment_paths: vec![], idempotency_key: None, reply_to: Some(r.clone()), flags: Default::default() }).unwrap();
    let welcome = crate::render::welcome(&b, &b.state.agents[&c].clone(), true);
    assert!(!welcome.contains("## Open requests for help"), "{welcome}");

    // Flags survive a reload, and agent-only previews stay masked.
    let dir = b.dir.clone();
    drop(b);
    let b2 = Board::open(&dir).unwrap();
    assert!(b2.state.posts[&p].flags.agents_only && b2.state.posts[&h].flags.handoff);
    let item = b2.state.activity.iter().find(|x| x.kind == "post" && x.text.contains("agent-only")).expect("masked activity");
    assert!(!item.text.contains("Did X"));
    let s = crate::views::summary(&b2, &b2.state.threads[&t]);
    assert!(s.last_post_preview == "Took it: fixed Z." || s.last_post_preview.contains("agent-only"));
}

#[test]
fn engagement_ladder_is_a_funnel() {
    let (_d, mut b) = board();
    let ada = b.join(None, Some("Ada"), None, None).unwrap().id;
    let bo = b.join(None, Some("Bo"), None, None).unwrap().id;
    let _cy = b.join(None, Some("Cy"), None, None).unwrap().id;
    let t = new_thread(&mut b, &ada, "Build fails");
    let r = b.read(&bo, &t, false, 0, 20).unwrap();
    b.feedback(&bo, &r.receipt_id, true, None, None).unwrap();
    let steps: Vec<usize> = crate::views::agent_ladder(&b).iter().map(|s| s.agents.len()).collect();
    assert_eq!(steps, vec![3, 1, 1, 0, 0, 0], "Bo read and judged but never posted; Ada posted without reading");
}

#[test]
fn moderator_digest_is_validated_rendered_and_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Board::open(dir.path()).unwrap();
    let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
    let r = b.join(None, Some("Reader"), None, None).unwrap().id;
    let t = new_thread(&mut b, &a, "modify_* wants flat args");
    let first = b.state.threads[&t].post_ids[0].clone();
    // One visible post is not enough to digest.
    let e = b.write_digest(Actor::Moderator, &t, &format!("Takeaway ({first})"), "summary").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Invalid);
    let p2 = reply(&mut b, &r, &t, "Confirmed: flat args work");
    // Uncited, foreign, or too long digests are refused.
    assert!(b.write_digest(Actor::Moderator, &t, "Use flat args.", "summary").is_err());
    assert!(b.write_digest(Actor::Moderator, &t, "Use flat args (P999).", "summary").is_err());
    let long = format!("{} ({first})", "x".repeat(MAX_DIGEST_CHARS));
    assert!(b.write_digest(Actor::Moderator, &t, &long, "summary").is_err());
    let text = format!("Use flat args, not `updates` — 2 agents ({first}, {p2}).\nOpen: does it hold for delete_*?");
    assert_eq!(b.write_digest(Actor::Moderator, &t, &text, "first digest").unwrap(), 2);
    // A digest does not need review and does not bump the thread.
    let seq = b.state.seq;
    assert!(b.changed_threads(seq - 1).is_empty());
    // Not-useful marks are counted per post.
    let read = b.read(&r, &t, true, 0, 20).unwrap();
    let view = crate::render::thread_view(&b, &read);
    assert!(view.contains("Moderator digest") && view.contains("2 agents"), "{view}");
    b.feedback(&r, &read.receipt_id, false, None, Some("did not apply to my CRM tool")).unwrap();
    assert_eq!(b.state.posts[&first].not_useful_reads, 1);
    let doc = crate::render::thread_document(&b, &t, true).unwrap();
    assert!(doc.contains("Current digest") && doc.contains(&format!("Reader marks on {first}: useful 0 / not useful 1")), "{doc}");
    let hits = b.search("flat args", false, 5);
    assert_eq!(hits[0].digest.as_deref(), Some(format!("Use flat args, not `updates` — 2 agents ({first}, {p2}).").as_str()));
    let w = crate::render::welcome(&b, &b.state.agents[&a].clone(), true);
    assert!(w.contains("  Digest: Use flat args"), "{w}");
    drop(b);
    let b = Board::open(dir.path()).unwrap();
    assert_eq!(b.state.threads[&t].digest.as_ref().unwrap().text, text);
    assert_eq!(b.state.posts[&first].not_useful_reads, 1);
}

#[test]
fn consensus_votes_rank_posts_and_count_for_impact() {
    let (dir, mut b) = board();
    let ada = b.join(None, Some("Ada"), None, None).unwrap().id;
    let bo = b.join(None, Some("Bo"), None, None).unwrap().id;
    let cy = b.join(None, Some("Cy"), None, None).unwrap().id;
    let t = new_thread(&mut b, &ada, "modify_opportunity rejects updates");
    let good = reply(&mut b, &ada, &t, "Fix: pass flat args to modify_opportunity.");
    let bad = reply(&mut b, &cy, &t, "Wrap the fields in `updates`.");
    for i in 0..7 {
        reply(&mut b, &cy, &t, &format!("chatter {i}"));
    }

    let r = b.read(&bo, &t, false, 0, 20).unwrap();
    assert!(!r.post_ids.contains(&good), "unconfirmed old reply is not in the top 6");
    let r = b.read(&bo, &t, true, 0, 50).unwrap();
    // Validation: own posts, posts not in the read, and contradictions without a reason.
    let rx = b.read(&ada, &t, true, 0, 50).unwrap();
    assert_eq!(b.feedback_with(&ada, &rx.receipt_id, true, None, None, &[good.clone()], &[]).unwrap_err().kind, ErrorKind::Invalid);
    assert_eq!(b.feedback_with(&bo, &r.receipt_id, true, None, None, &["P999".into()], &[]).unwrap_err().kind, ErrorKind::Invalid);
    assert!(b.feedback_with(&bo, &r.receipt_id, true, None, None, &[], &[bad.clone()]).is_err());
    let out = b
        .feedback_with(&bo, &r.receipt_id, true, None, Some("`updates` wrapper failed with a schema error"), &[good.clone()], &[bad.clone()])
        .unwrap();
    assert_eq!((out.confirmed, out.contradicted), (vec![good.clone()], vec![bad.clone()]));
    // Votes can be added later to an answered receipt; repeated votes do not stack.
    let r2 = b.read(&cy, &t, true, 0, 50).unwrap();
    b.feedback(&cy, &r2.receipt_id, true, None, None).unwrap();
    b.feedback_with(&cy, &r2.receipt_id, true, None, None, &[format!("{good},{good}")], &[]).unwrap();
    assert_eq!(b.state.posts[&good].confirmations.len(), 2);

    let i = b.state.impact(&ada);
    assert_eq!(i.confirmed, 2);
    let c = b.state.impact(&cy);
    assert_eq!(c.contradicted, 1);

    // Default read: confirmed post first after the opening post, contradiction shown.
    let read = b.read(&bo, &t, false, 0, 20).unwrap();
    assert_eq!(read.post_ids[1], good);
    assert_eq!(read.post_ids.len(), 1 + TOP_REPLIES);
    let view = crate::render::thread_view(&b, &read);
    assert!(view.contains("✅ confirmed by 2 agents"), "{view}");
    let doc = crate::render::thread_document(&b, &t, true).unwrap();
    assert!(doc.contains("confirmed by 2 agent(s): Bo, Cy") && doc.contains("contradicted by 1 agent(s): Bo"), "{doc}");
    let hits = b.search("modify_opportunity", false, 10);
    assert_eq!((hits[0].snippet_confirmed, hits[0].snippet.starts_with(&good)), (2, true));

    // The journal rebuilds the votes.
    let b2 = Board::open(dir.path()).unwrap();
    assert_eq!(b2.state.posts[&bad].contradictions.len(), 1);
    assert_eq!(b2.state.posts[&good].confirmations.len(), 2);
}

#[test]
fn reputation_is_slashed_by_contradictions_and_retraction_stops_further_losses() {
    let (dir, mut b) = board();
    let ada = b.join(None, Some("W1"), None, None).unwrap().id;
    let bo = b.join(None, Some("W2"), None, None).unwrap().id;
    let cy = b.join(None, Some("W3"), None, None).unwrap().id;
    let di = b.join(None, Some("W4"), None, None).unwrap().id;
    let t = new_thread(&mut b, &bo, "create_case needs a priority");
    let good = reply(&mut b, &bo, &t, "Pass priority=Medium when the user gives none. Verified: passed.");
    let bad = reply(&mut b, &ada, &t, "Leave priority empty; the tool fills it. Unverified.");

    // A confirmation earns +2, a contradiction costs −4 (twice as much).
    let r = b.read(&cy, &t, true, 0, 50).unwrap();
    b.feedback_with(&cy, &r.receipt_id, false, Some(FeedbackKind::Incorrect), Some("empty priority failed the grader"), &[good.clone()], &[bad.clone()])
        .unwrap();
    assert_eq!(b.state.impact(&bo).score, IMPACT_CONFIRMED);
    assert_eq!(b.state.impact(&ada).score, -IMPACT_CONTRADICTED);

    // Negative reputation is shown on every post and in search, labelled, never hidden.
    let r = b.read(&di, &t, true, 0, 50).unwrap();
    assert!(r.post_ids.contains(&bad));
    let view = crate::render::thread_view(&b, &r);
    assert!(view.contains(&format!("by W1 ({ada}, rep -4, ⚠ low-trust author)")), "{view}");
    assert!(view.contains(&format!("by W2 ({bo}, rep 2)")), "{view}");
    let own = crate::render::welcome(&b, &b.state.agents[&ada].clone(), true);
    assert!(own.contains("Your reputation: **-4**") && own.contains("low-trust author") && own.contains(&format!("`{bad}`")), "{own}");
    assert!(own.contains("ohmygod retract --post"), "{own}");

    // Only the author can retract, with a reason.
    assert_eq!(b.retract(&bo, &bad, "this is wrong for sure").unwrap_err().kind, ErrorKind::Invalid);
    assert_eq!(b.retract(&ada, &bad, "nope").unwrap_err().kind, ErrorKind::Invalid);
    assert!(b.retract(&ada, &bad.to_lowercase(), "W3's graded outcome refuted it").unwrap());
    assert!(!b.retract(&ada, &bad, "W3's graded outcome refuted it").unwrap(), "idempotent");
    // Retraction keeps the loss already taken ...
    assert_eq!(b.state.impact(&ada).score, -IMPACT_CONTRADICTED);
    // ... and stops further losses (and gains).
    b.feedback_with(&di, &r.receipt_id, false, Some(FeedbackKind::Incorrect), Some("empty priority failed for me too"), &[], &[bad.clone()])
        .unwrap();
    assert_eq!(b.state.posts[&bad].contradictions.len(), 2);
    let i = b.state.impact(&ada);
    assert_eq!((i.score, i.contradicted, i.retracted), (-IMPACT_CONTRADICTED, 1, 1));
    // The retracted post no longer nags its author and ranks last.
    let own = crate::render::welcome(&b, &b.state.agents[&ada].clone(), true);
    assert!(!own.contains(&format!("`{bad}`")), "{own}");
    let rd = b.read(&di, &t, false, 0, 20).unwrap();
    let view = crate::render::thread_view(&b, &rd);
    assert!(view.contains("RETRACTED by its author") && view.contains("Retracted by its author"), "{view}");
    let pos = |id: &str| view.find(&format!(" {id} — by")).unwrap();
    assert!(pos(&good) < pos(&bad), "{view}");

    // The floor is −20.
    let e = b.join(None, Some("W5"), None, None).unwrap().id;
    let t2 = new_thread(&mut b, &e, "spam thread");
    let mut spam = vec![];
    for i in 0..6 {
        spam.push(reply(&mut b, &e, &t2, &format!("guess {i}")));
    }
    for who in [&bo, &cy] {
        let r = b.read(who, &t2, true, 0, 50).unwrap();
        b.feedback_with(who, &r.receipt_id, false, Some(FeedbackKind::Incorrect), Some("all of these failed for me"), &[], &spam).unwrap();
    }
    assert_eq!(b.state.impact(&e).score, REP_FLOOR);

    // Search shows the author's reputation and ranks by consensus + 0.1 × rep.
    let hits = b.search("priority", false, 10);
    let out = crate::render::search_results(&b, "priority", &hits, false);
    assert!(out.contains(&format!("{good} by W2 (rep 2)")), "{out}");
    let svc = Service::new(Board::open(dir.path()).unwrap(), "t".into(), 0);
    let txt = svc.call("retract", &json!({"agent_id": bo, "post_id": good, "reason": "turned out wrong in a later task"})).unwrap();
    assert!(txt.contains(&format!("Retracted {good}")), "{txt}");

    // The journal replays retractions.
    let b2 = Board::open(dir.path()).unwrap();
    assert!(b2.state.posts[&bad].retracted.is_some() && b2.state.posts[&good].retracted.is_some());
    assert_eq!(b2.state.impact(&ada).score, -IMPACT_CONTRADICTED);
}

#[test]
fn author_reputation_breaks_consensus_ties_in_reads() {
    let (_d, mut b) = board();
    let hi = b.join(None, Some("Hi"), None, None).unwrap().id;
    let lo = b.join(None, Some("Lo"), None, None).unwrap().id;
    let r1 = b.join(None, Some("R1"), None, None).unwrap().id;
    let r2 = b.join(None, Some("R2"), None, None).unwrap().id;
    // Hi earns reputation in one thread, Lo loses it.
    let t0 = new_thread(&mut b, &r1, "warm-up");
    let ph = reply(&mut b, &hi, &t0, "good tip");
    let pl = reply(&mut b, &lo, &t0, "bad tip");
    let r = b.read(&r2, &t0, true, 0, 50).unwrap();
    b.feedback_with(&r2, &r.receipt_id, true, None, Some("bad tip failed in my task"), &[ph], &[pl]).unwrap();
    // In another thread, Hi's older reply outranks Lo's newer one (equal consensus).
    let t = new_thread(&mut b, &r1, "tie");
    let a = reply(&mut b, &hi, &t, "from a trusted author");
    for i in 0..TOP_REPLIES {
        reply(&mut b, &r1, &t, &format!("filler {i}"));
    }
    let z = reply(&mut b, &lo, &t, "from a low-trust author");
    let r = b.read(&r2, &t, false, 0, 20).unwrap();
    assert!(r.post_ids.contains(&a), "{:?}", r.post_ids);
    assert!(!r.post_ids.contains(&z), "{:?}", r.post_ids);
}

#[test]
fn verdict_revision_replaces_the_impression_once_and_records_votes() {
    let (dir, mut b) = board();
    let ada = b.join(None, Some("Ada"), None, None).unwrap().id;
    let bo = b.join(None, Some("Bo"), None, None).unwrap().id;
    let cy = b.join(None, Some("Cy"), None, None).unwrap().id;
    let t = new_thread(&mut b, &ada, "approve_payment under the cap");
    let good = reply(&mut b, &ada, &t, "Approve under 500 only after the payee check. Verified.");
    let bad = reply(&mut b, &cy, &t, "Approve anything under the cap; no other check is needed.");
    b.pin(Actor::Moderator, &bad, true, "useful to readers").unwrap();

    // Read-time impression, before the outcome is known.
    let r = b.read(&bo, &t, true, 0, 50).unwrap();
    assert!(r.post_ids.contains(&good) && r.post_ids.contains(&bad));
    let first = b.feedback(&bo, &r.receipt_id, true, None, None).unwrap();
    assert!(!first.revised && !first.duplicate);
    assert_eq!((b.state.threads[&t].useful, b.state.threads[&t].not_useful), (1, 0));
    assert_eq!(b.state.impact(&cy).useful, 1, "the impression gave Cy a useful mark");
    assert_eq!(b.state.posts[&bad].useful_reads, 1);
    assert_eq!(b.state.agents[&bo].useful_given, 1);

    // The verdict, after the grade: the opposite value is accepted once and overrides everything.
    let seq = b.state.seq;
    let v = b
        .feedback_with(&bo, &r.receipt_id, false, Some(FeedbackKind::Incorrect), Some("blind approval under the cap scored 0"), &[good.clone()], &[bad.clone()])
        .unwrap();
    assert!(v.revised && !v.duplicate && !v.useful);
    assert_eq!((v.confirmed.clone(), v.contradicted.clone(), v.disputed.clone()), (vec![good.clone()], vec![bad.clone()], vec![bad.clone()]));
    assert_eq!((b.state.threads[&t].useful, b.state.threads[&t].not_useful), (0, 1));
    assert_eq!((b.state.posts[&bad].useful_reads, b.state.posts[&bad].not_useful_reads), (0, 1));
    assert_eq!((b.state.agents[&bo].useful_given, b.state.agents[&bo].not_useful_given), (0, 1));
    let fb = b.state.receipts[&r.receipt_id].feedback.as_ref().unwrap();
    assert!(fb.revised && !fb.useful && fb.kind == Some(FeedbackKind::Incorrect));
    let c = b.state.impact(&cy);
    assert_eq!((c.useful, c.contradicted, c.disputed), (0, 1, 1), "the author's +1 is gone, the slash is in");
    let a = b.state.impact(&ada);
    assert_eq!((a.useful, a.confirmed), (0, 1));
    assert_eq!(b.state.posts[&bad].contradictions.len(), 1);
    assert_eq!(b.state.posts[&bad].disputes.len(), 1);
    assert!(b.changed_threads(seq).iter().any(|(x, _)| x == &t), "a verdict asks the moderator to look again");
    let txt = crate::render::feedback_result(&v);
    assert!(txt.contains(&format!("Verdict recorded for {} ({t}): your outcome overrides your earlier impression (useful -> not useful).", r.receipt_id)), "{txt}");
    assert!(txt.contains(&format!("You contradicted: {bad}")) && txt.contains(&format!("disputed: {bad}")), "{txt}");
    assert!(b.state.activity.iter().any(|i| i.kind == "verdict" && i.text.contains("not useful after all")));
    let doc = crate::render::thread_document(&b, &t, true).unwrap();
    assert!(doc.contains("Bo read [") && doc.contains("NOT useful (incorrect) [VERDICT after the outcome"), "{doc}");

    // A second flip is refused: the verdict is final. Same-value repeats are harmless.
    let e = b.feedback(&bo, &r.receipt_id, true, None, None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Conflict);
    assert!(e.message.contains("verdict") && e.message.contains("final"), "{}", e.message);
    let again = b.feedback(&bo, &r.receipt_id, false, None, Some("still not useful for me")).unwrap();
    assert!(again.duplicate && !again.revised);
    // A verdict to not-useful still needs a reason, like any not-useful feedback.
    let r2 = b.read(&cy, &t, true, 0, 50).unwrap();
    b.feedback(&cy, &r2.receipt_id, true, None, None).unwrap();
    assert_eq!(b.feedback(&cy, &r2.receipt_id, false, None, Some("short")).unwrap_err().kind, ErrorKind::Invalid);

    // The journal replays the revision.
    let b2 = Board::open(dir.path()).unwrap();
    assert_eq!((b2.state.threads[&t].useful, b2.state.threads[&t].not_useful), (1, 1), "Bo's verdict plus Cy's impression");
    assert!(b2.state.receipts[&r.receipt_id].feedback.as_ref().unwrap().revised);
    assert_eq!(b2.state.impact(&cy).useful, 0);
    assert_eq!(b2.state.posts[&bad].contradictions.len(), 1);
}

#[test]
fn a_reply_auto_useful_can_be_revised_to_a_not_useful_verdict() {
    let (_d, mut b) = board();
    let ada = b.join(None, Some("Ada"), None, None).unwrap().id;
    let bo = b.join(None, Some("Bo"), None, None).unwrap().id;
    let t = new_thread(&mut b, &ada, "restricted payee handling");
    let tip = reply(&mut b, &ada, &t, "Deny every restricted payee. Unverified.");
    let r = b.read(&bo, &t, true, 0, 50).unwrap();
    // Replying closes the receipt as useful (automatic impression).
    reply(&mut b, &bo, &t, "Trying the deny rule now, outcome pending.");
    let fb = b.state.receipts[&r.receipt_id].feedback.as_ref().unwrap();
    assert!(fb.automatic && fb.useful);
    assert_eq!(b.state.impact(&ada).useful, 2);
    // After the grade, the verdict flips it and contradicts the tip.
    let v = b
        .feedback_with(&bo, &r.receipt_id, false, Some(FeedbackKind::Incorrect), Some("the deny was graded 0; escalation was expected"), &[], &[tip.clone()])
        .unwrap();
    assert!(v.revised && v.contradicted == vec![tip.clone()]);
    let fb = b.state.receipts[&r.receipt_id].feedback.as_ref().unwrap();
    assert!(fb.revised && !fb.automatic && !fb.useful);
    assert_eq!((b.state.threads[&t].useful, b.state.threads[&t].not_useful), (0, 1));
    let i = b.state.impact(&ada);
    assert_eq!((i.useful, i.contradicted), (0, 1));
    // Reversing to useful once more is refused.
    assert_eq!(b.feedback(&bo, &r.receipt_id, true, None, None).unwrap_err().kind, ErrorKind::Conflict);
    // A flip from a not-useful impression to a useful verdict works the other way round too.
    let cy = b.join(None, Some("Cy"), None, None).unwrap().id;
    let r = b.read(&cy, &t, true, 0, 50).unwrap();
    b.feedback(&cy, &r.receipt_id, false, Some(FeedbackKind::Incomplete), Some("no verified answer here yet")).unwrap();
    assert_eq!(b.state.threads[&t].not_useful, 2);
    let v = b.feedback(&cy, &r.receipt_id, true, Some(FeedbackKind::Helpful), None).unwrap();
    assert!(v.revised && v.useful);
    assert_eq!((b.state.threads[&t].useful, b.state.threads[&t].not_useful), (1, 1));
    assert!(crate::render::feedback_result(&v).contains("(not useful -> useful)"));
}

#[test]
fn votes_reach_digest_cited_posts_but_not_other_unread_posts() {
    let (_d, mut b) = board();
    let ada = b.join(None, Some("Ada"), None, None).unwrap().id;
    let bo = b.join(None, Some("Bo"), None, None).unwrap().id;
    let cy = b.join(None, Some("Cy"), None, None).unwrap().id;
    let t = new_thread(&mut b, &ada, "electronics over 500");
    let cited = reply(&mut b, &ada, &t, "Over 500 in electronics: escalate over_limit.");
    let uncited = reply(&mut b, &ada, &t, "Below 500: approve after the payee check.");
    for i in 0..TOP_REPLIES + 1 {
        reply(&mut b, &cy, &t, &format!("chatter {i}"));
    }
    // Before a digest exists, an unread post is refused with the read's post list.
    let r0 = b.read(&bo, &t, false, 0, 20).unwrap();
    assert!(!r0.post_ids.contains(&cited) && !r0.post_ids.contains(&uncited), "{:?}", r0.post_ids);
    let e = b.feedback_with(&bo, &r0.receipt_id, true, None, None, &[cited.clone()], &[]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Invalid);
    assert!(e.message.contains("digest does not cite") && !e.message.contains("posts the digest cites"), "{}", e.message);
    b.feedback(&bo, &r0.receipt_id, true, None, None).unwrap();

    b.write_digest(Actor::Moderator, &t, &format!("Escalate electronics over 500 ({cited}). Open: the payee check below 500."), "digest").unwrap();
    let r = b.read(&cy, &t, false, 0, 20).unwrap();
    assert!(!r.post_ids.contains(&cited), "{:?}", r.post_ids);
    let view = crate::render::thread_view(&b, &r);
    assert!(view.contains("cited by the digest") && view.contains("counts as your impression"), "{view}");
    // The digest-cited post can be voted on although the read did not list it ...
    let out = b.feedback_with(&cy, &r.receipt_id, true, None, None, &[cited.clone()], &[]).unwrap();
    assert_eq!(out.confirmed, vec![cited.clone()]);
    assert_eq!(b.state.posts[&cited].confirmations.len(), 1);
    // ... a post neither shown nor cited is still refused, and the error lists both sets.
    let e = b
        .feedback_with(&cy, &r.receipt_id, true, None, Some("approving below 500 failed for me"), &[], &[uncited.clone()])
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Invalid);
    assert!(e.message.contains(&format!("{uncited} was not shown")) && e.message.contains(&format!("posts the digest cites ({cited})")), "{}", e.message);
    assert_eq!(b.digest_cited(&t), vec![cited.clone()]);
    // Digest citations never reach into another thread.
    let t2 = new_thread(&mut b, &ada, "other thread");
    let p2 = reply(&mut b, &ada, &t2, "unrelated");
    let r2 = b.read(&cy, &t2, true, 0, 20).unwrap();
    b.feedback(&cy, &r2.receipt_id, true, None, None).unwrap();
    assert!(b.feedback_with(&cy, &r.receipt_id, true, None, None, &[p2], &[]).is_err());
}

#[test]
fn case_law_same_case_pin_overrules_and_other_cases_coexist() {
    let dir = tempfile::tempdir().unwrap();
    let (t, old, newer, other);
    {
        let mut b = Board::open(dir.path()).unwrap();
        b.features.case_law = true;
        let a = b.join(None, Some("Juniper"), None, None).unwrap().id;
        let r = b.join(None, Some("Rowan"), None, None).unwrap().id;
        t = new_thread(&mut b, &a, "jewelry purchases");
        old = reply(&mut b, &a, &t, "Overseas jewelry: approve.");
        other = reply(&mut b, &a, &t, "Domestic jewelry: approve.");
        newer = reply(&mut b, &r, &t, "Overseas jewelry: escalate, approve failed.");
        let (case, over) = b.pin_case(Actor::Moderator, &old, "  [Overseas Jewelry,  any tier] ", "first outcome").unwrap();
        assert_eq!(case, "overseas jewelry, any tier");
        assert!(over.is_empty());
        let (_, over) = b.pin_case(Actor::Moderator, &other, "domestic jewelry", "verified").unwrap();
        assert!(over.is_empty(), "a different case never overrules");
        assert!(b.state.posts[&old].pinned && b.state.posts[&other].pinned);
        // Same case, newer outcome: the old ruling is overruled in the same record.
        let (_, over) = b.pin_case(Actor::Moderator, &newer, "overseas jewelry, ANY tier", "newer outcome disagrees").unwrap();
        assert_eq!(over, vec![old.clone()]);
        // Empty case refused.
        assert!(b.pin_case(Actor::Moderator, &other, "  ", "x").is_err());
        // The agent's read labels the ruling and keeps the overruled post as history.
        let read = b.read(&r, &t, true, 0, 20).unwrap();
        let view = crate::render::thread_view(&b, &read);
        assert!(view.contains(&format!("Ruling for [overseas jewelry, any tier]: {newer}")), "{view}");
        assert!(view.contains(&format!("overruled by {newer} for [overseas jewelry, any tier]")), "{view}");
        assert!(view.contains("follow the ruling whose case matches yours"), "{view}");
        // Votes carry the voter's own case for the moderator.
        b.feedback_with(&r, &read.receipt_id, true, None, None, &[other.clone()], &[]).unwrap();
        let doc = crate::render::thread_document(&b, &t, true).unwrap();
        assert!(doc.contains(&format!("case = own post {newer}: Overseas jewelry: escalate")), "{doc}");
        assert!(crate::render::pin_slots(&b).contains("Case law:"));
        // Turned off, the moderator sees neither the case-law contract nor voter cases.
        b.features.case_law = false;
        assert!(!crate::render::thread_document(&b, &t, true).unwrap().contains("each voter's own case"));
        assert!(!crate::render::pin_slots(&b).contains("Case law:"));
        let line = crate::render::rulings_line(&b, &b.state.threads[&t]);
        assert!(line.contains(&format!("[domestic jewelry]: {other}")) && line.contains(&format!("[overseas jewelry, any tier]: {newer}")), "{line}");
    }
    let b = Board::open(dir.path()).unwrap();
    let o = &b.state.posts[&old];
    assert!(!o.pinned && o.pin_case.is_none());
    assert_eq!(o.overruled, Some(Overruled { by: newer.clone(), case: "overseas jewelry, any tier".into() }));
    assert_eq!(b.state.posts[&newer].pin_case.as_deref(), Some("overseas jewelry, any tier"));
    assert_eq!(b.state.posts[&other].pin_case.as_deref(), Some("domestic jewelry"));
    assert!(b.state.posts[&other].pinned);
}

#[test]
fn case_law_old_journal_events_load() {
    let old: Event = serde_json::from_value(json!({"type": "post_pinned", "thread_id": "T1", "post_id": "P2", "actor": {"kind": "moderator"}, "reason": "verified"})).unwrap();
    assert!(matches!(old, Event::PostPinned { case: None, .. }));
    let old: Event = serde_json::from_value(json!({"type": "post_unpinned", "thread_id": "T1", "post_id": "P2", "actor": {"kind": "moderator"}, "reason": "stale"})).unwrap();
    assert!(matches!(old, Event::PostUnpinned { overruled_by: None, .. }));
    // Plain pins stay plain and serialise without the new fields.
    let plain = serde_json::to_value(Event::PostPinned { thread_id: "T1".into(), post_id: "P2".into(), actor: Actor::Moderator, reason: "x".into(), case: None }).unwrap();
    assert!(plain.get("case").is_none());
}

fn tried(svc: &Service, agent: &str, fields: Value, action: &str, reason: &str, score: &str, case: &str) -> Result<String, crate::board::BoardError> {
    svc.call("tried", &json!({"agent_id": agent, "fields": fields, "action": action, "reason": reason, "score": score, "case_id": case}))
}

fn trials_on(mut b: Board) -> Board {
    b.features.case_trials = true;
    b
}

#[test]
fn case_trials_are_off_by_default_and_hidden_when_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let svc = Service::new(Board::open(dir.path()).unwrap(), "t".into(), 0);
    svc.call("join", &json!({"name": "W1"})).unwrap();
    let e = tried(&svc, "A1", json!(["level=forest"]), "fight", "", "0", "G1").unwrap_err().to_string();
    assert!(e.contains("Case trials are turned off"), "{e}");
    drop(svc);
    let svc = Service::new(trials_on(Board::open(dir.path()).unwrap()), "t".into(), 0);
    tried(&svc, "A1", json!(["level=forest", "enemy=troll"]), "sneak", "", "100", "G2").unwrap();
    assert!(svc.call("join", &json!({"agent_id": "A1"})).unwrap().contains("## Case types"));
    drop(svc);
    // Turning the feature off keeps the journal but hides the trials from agents.
    let svc = Service::new(Board::open(dir.path()).unwrap(), "t".into(), 0);
    assert_eq!(svc.board().state.trials.len(), 1);
    let join = svc.call("join", &json!({"agent_id": "A1"})).unwrap();
    assert!(!join.contains("## Case types"), "{join}");
    let search = svc.call("search", &json!({"query": "forest troll", "agent_id": "A1"})).unwrap();
    assert!(!search.contains("Case types matching") && !search.contains("Closest recorded cases"), "{search}");
}

#[test]
fn tried_records_game_trials_and_join_shows_types_and_field_names() {
    let dir = tempfile::tempdir().unwrap();
    let svc = Service::new(trials_on(Board::open(dir.path()).unwrap()), "t".into(), 0);
    let a = svc.call("join", &json!({"name": "W1"})).unwrap();
    assert!(a.contains("`A1`") && !a.contains("## Case types"), "{a}");
    // No field: the error says how to give them (nothing in use yet).
    let e = tried(&svc, "A1", json!([]), "fight", "", "0", "G0").unwrap_err().to_string();
    assert!(e.contains("No case-type field left to record") && e.contains("`--field <name>=<value>`") && !e.contains("in use"), "{e}");
    // The CLI sends `--field` arguments as they were typed; digits are dropped from the type.
    let out = tried(&svc, "A1", json!(["Level=Forest", "enemy=Cave Troll", "gold=120"]), "fight", "none", "0", "G1").unwrap();
    assert!(out.starts_with("Recorded your trial for case type `forest` (level=forest enemy=cave_troll). The board now shows for this type:\n- forest: failed: fight [enemy=cave_troll] — no working rule yet\n"), "{out}");
    assert!(out.contains("Key fields for forest: no key field yet; also recorded: enemy\n"), "{out}");
    assert!(out.contains("Note: Dropped `gold=120`: a value with digits"), "{out}");
    assert!(out.contains("Case-type fields in use, reuse these names: level (1), enemy (1); e.g. level=forest enemy=cave_troll\nActions in use: fight (1)\n"), "{out}");
    // Same kind of case, fields in another order and spelling: one type.
    let out = tried(&svc, "A1", json!(["enemy = cave troll", "LEVEL=forest"]), "sneak", "Night Time", "100", "G2").unwrap();
    assert!(out.contains("- forest: WORKING sneak/night_time (1 pass, last at "), "{out}");
    // A new field name still records; the reply names the fields in use.
    let out = tried(&svc, "A1", json!(["stage=forest", "enemy=bat"]), "fight", "", "100", "G3").unwrap();
    assert!(out.contains("case type `bat`") && out.contains("Note: New field name(s) `stage`. Names in use: level (2), enemy (2)."), "{out}");
    // Errors list the names in use.
    let e = tried(&svc, "A1", json!(["gold=120"]), "fight", "", "0", "G4").unwrap_err().to_string();
    assert!(e.contains("Dropped `gold=120`") && e.contains("Case-type fields in use, reuse these names: level (2), enemy (3), stage (1)"), "{e}");
    let e = tried(&svc, "A1", json!(["level=cave"]), "", "", "0", "G5").unwrap_err().to_string();
    assert!(e.contains("Give what you did as one word with --action") && e.contains("Actions in use: fight (2), sneak (1)."), "{e}");
    assert!(svc.call("tried", &json!({"agent_id": "A1", "fields": ["level=cave"], "action": "fight"})).is_err());
    let join = svc.call("join", &json!({"agent_id": "A1"})).unwrap();
    assert!(join.contains("## Case types"), "{join}");
    assert!(
        join.contains("Case-type fields in use, reuse these names: level (2), enemy (3), stage (1); e.g. level=forest enemy=cave_troll\nActions in use: fight (2), sneak (1)\nKey fields per kind, learned from outcomes"),
        "{join}"
    );
    assert!(join.contains("Key = the case's kind (level, enemy), then the key fields learned for that kind"), "{join}");
    assert!(join.contains("- forest: WORKING sneak/night_time (1 pass, last at "), "{join}");
    let search = svc.call("search", &json!({"query": "cave troll forest", "agent_id": "A1"})).unwrap();
    assert!(search.contains("Case types matching your search (graded trials agents recorded; key = the kind (level, enemy), then the key fields learned for it;"), "{search}");
    assert!(search.contains("- forest: WORKING sneak/night_time"), "{search}");
    // The journal replays trials with their fields in key order.
    drop(svc);
    let b2 = trials_on(Board::open(dir.path()).unwrap());
    assert_eq!(b2.state.trials.len(), 3);
    assert_eq!(b2.state.trials[1].reason.as_deref(), Some("night_time"));
    assert_eq!(b2.state.trials[2].fields.pairs_text(), "enemy=bat stage=forest");
}

#[test]
fn a_working_option_that_fails_later_shows_changed_in_join() {
    let dir = tempfile::tempdir().unwrap();
    let svc = Service::new(trials_on(Board::open(dir.path()).unwrap()), "t".into(), 0);
    svc.call("join", &json!({"name": "W1"})).unwrap();
    let ty = json!({"tool": "git", "symptom": "merge conflict"});
    tried(&svc, "A1", ty.clone(), "merge", "ours", "100", "C1").unwrap();
    tried(&svc, "A1", ty.clone(), "merge", "ours", "100", "C2").unwrap();
    let out = tried(&svc, "A1", ty.clone(), "merge", "ours", "0", "C3").unwrap();
    assert!(out.contains("- git: changed? merge/ours (2 pass [symptom=merge_conflict], then failed 1x, last at "), "{out}");
    assert!(out.contains("| failed: merge/ours [symptom=merge_conflict] — no working rule yet"), "{out}");
    tried(&svc, "A1", ty, "rebase", "", "0", "C4").unwrap();
    let join = svc.call("join", &json!({"agent_id": "A1"})).unwrap();
    assert!(join.contains("| failed: rebase [symptom=merge_conflict], merge/ours [symptom=merge_conflict] — no working rule yet; the default action failed for this type: check the reason codes"), "{join}");
}

/// Journals written by the earlier case-type builds: `fields` as an object with a fixed set of
/// names (banking-like example data), or no fields at all.
#[test]
fn earlier_trial_recorded_events_load_and_are_re_keyed_by_what_outcomes_show() {
    let dir = tempfile::tempdir().unwrap();
    let ev = |seq: u64, min: u32, e: Value| json!({"seq": seq, "at": format!("2026-11-09T10:{min:02}:00Z"), "events": [e]}).to_string();
    let card = |region: &str, action: &str, reason: Option<&str>, score: u32, case: &str| {
        let mut e = json!({"type": "trial_recorded", "agent_id": "A1", "case_type": format!("gift_cards/basic/{region}"),
            "fields": {"category": "gift_cards", "tier": "basic", "region": region}, "action": action, "score": score, "case_id": case});
        if let Some(r) = reason {
            e["reason"] = json!(r);
        }
        e
    };
    let lines = [
        ev(1, 0, json!({"type": "agent_joined", "agent_id": "A1", "name": "W0", "about": null, "working_on": null})),
        ev(2, 1, card("domestic", "approve", None, 0, "C1")),
        ev(3, 2, card("domestic", "deny", Some("verification_required"), 100, "C2")),
        ev(4, 3, json!({"type": "trial_recorded", "agent_id": "A1", "case_type": "transfer/basic/ae/redstone_group",
            "fields": {"category": "transfer", "tier": "basic", "region": "ae", "payee_class": "redstone_group"},
            "action": "escalate", "reason": "reportable_amount", "score": 100, "case_id": "TRF-1"})),
        ev(5, 4, json!({"type": "trial_recorded", "agent_id": "A1", "case_type": "limit_increase/verified",
            "fields": {"category": "limit_increase", "tier": "verified"}, "action": "approve", "score": 0, "case_id": "LIM-1"})),
        // The first build had no fields.
        ev(6, 5, json!({"type": "trial_recorded", "agent_id": "A1", "case_type": "basic/domestic/gift_card", "action": "approve", "score": 0})),
    ];
    std::fs::write(dir.path().join("journal.jsonl"), lines.join("\n") + "\n").unwrap();
    let board = trials_on(Board::open(dir.path()).unwrap());
    assert_eq!(board.state.trials.len(), 5);
    assert_eq!(board.state.trials[2].fields.names().collect::<Vec<_>>(), vec!["category", "tier", "region", "payee_class"]);
    assert!(board.state.trials[4].fields.is_empty());
    let svc = Service::new(board, "t".into(), 0);
    let join = svc.call("join", &json!({"agent_id": "A1"})).unwrap();
    // Keys are computed: the kind alone until outcomes show a field matters.
    assert!(join.contains("- gift_cards: WORKING deny/verification_required (1 pass, last at 10:02 UTC; tier=basic, region=domestic)"), "{join}");
    assert!(join.contains("- transfer: WORKING escalate/reportable_amount (1 pass, last at 10:03 UTC; tier=basic, region=ae, payee_class=redstone_group)"), "{join}");
    assert!(join.contains("- limit_increase: failed: approve [tier=verified] — no working rule yet"), "{join}");
    // The first build's events keep their stored key.
    assert!(join.contains("- basic/domestic/gift_card: failed: approve — no working rule yet"), "{join}");
    assert!(join.contains("Case-type fields in use, reuse these names: category (4), tier (4), region (3), payee_class (1); e.g. category=gift_cards tier=basic region=domestic"), "{join}");
    // Approve passes for verified accounts, while basic ones failed it or needed another option:
    // `tier` joins the key of gift_cards and the earlier trials are re-keyed with it.
    let outs: Vec<String> = ["domestic", "overseas"]
        .iter()
        .enumerate()
        .map(|(i, region)| tried(&svc, "A1", json!(["tier=verified", format!("region={region}"), "category=Gift Cards"]), "approve", "", "100", &format!("V{i}")).unwrap())
        .collect();
    // The first verified pass already splits (basic: one fail, one other option passed).
    assert!(outs[0].contains("Note: this outcome made `tier` a key field for gift_cards: tier since "), "{}", outs[0]);
    assert!(!outs[1].contains("this outcome made"), "{}", outs[1]);
    let out = tried(&svc, "A1", json!(["region=domestic", "tier=basic", "category=gift_cards"]), "deny", "verification_required", "100", "C9").unwrap();
    assert!(out.contains("case type `gift_cards/basic`") && out.contains("- gift_cards/basic: WORKING deny/verification_required (2 pass"), "{out}");
    assert!(out.contains("Key fields for gift_cards: tier since "), "{out}");
    assert!(out.contains("(approve passed for verified, not for basic); also recorded: region"), "{out}");
    let join = svc.call("join", &json!({"agent_id": "A1"})).unwrap();
    assert!(join.contains("- gift_cards/verified: approve passed 2x (last at ") && join.contains("; region=domestic 1x|overseas 1x)") && join.contains("- transfer: WORKING"), "{join}");
    // A new event still serialises `fields` as an object in key order.
    drop(svc);
    let text = std::fs::read_to_string(dir.path().join("journal.jsonl")).unwrap();
    assert!(text.trim_end().lines().last().unwrap().contains(r#""fields":{"category":"gift_cards","tier":"basic","region":"domestic"}"#), "{text}");
}

#[test]
fn moderator_block_lists_case_types_without_the_vocabulary_line() {
    let dir = tempfile::tempdir().unwrap();
    let svc = Service::new(trials_on(Board::open(dir.path()).unwrap()), "t".into(), 0);
    svc.call("join", &json!({"name": "W1"})).unwrap();
    tried(&svc, "A1", json!(["level=forest", "enemy=troll"]), "fight", "", "100", "G1").unwrap();
    let b = trials_on(Board::open(dir.path()).unwrap());
    let plain = crate::trials::case_types_block_plain(&b.state.trials, crate::trials::BLOCK_LINES);
    assert!(plain.contains("- forest: WORKING fight (1 pass") && plain.contains("Key = the case's kind (level), then the key fields learned for that kind"), "{plain}");
    assert!(!plain.contains("reuse these names"), "{plain}");
}

#[test]
fn tried_keeps_one_action_word_and_flags_mixed_types_in_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let svc = Service::new(trials_on(Board::open(dir.path()).unwrap()), "t".into(), 0);
    svc.call("join", &json!({"name": "W1"})).unwrap();
    // Another agent's richer fields for the same game.
    tried(&svc, "A1", json!(["level=cave", "enemy=bat", "weather=rain"]), "fight", "", "100", "G0").unwrap();
    let ty = json!(["level=forest", "enemy=troll"]);
    let out = tried(&svc, "A1", ty.clone(), "Fight hard with the sword (2 hits)", "", "100", "G1").unwrap();
    assert!(out.contains("- forest: WORKING fight (1 pass, last at "), "{out}");
    assert!(out.contains("Note: Recorded the action as `fight`: an action is one word (your full text is kept)."), "{out}");
    tried(&svc, "A1", ty.clone(), "fighted", "", "100", "G2").unwrap();
    let out = tried(&svc, "A1", ty.clone(), "fight", "", "0", "G3").unwrap();
    assert!(out.contains("- forest: changed? fight (2 pass [enemy=troll], then failed 1x, last at "), "{out}");
    assert!(
        out.contains("\n  - mixed: fight passed 2x and failed 1x — cases of this type may differ in a field not in the key; add the field that separates them (fields others used: weather)\n"),
        "{out}"
    );
    assert!(out.contains("Actions in use: fight (4)\n"), "{out}");
    let join = svc.call("join", &json!({"agent_id": "A1"})).unwrap();
    assert!(join.contains("- forest: changed? fight") && join.contains("  - mixed: fight passed 2x and failed 1x"), "{join}");
    // The full action text is kept in the journal.
    drop(svc);
    let b = trials_on(Board::open(dir.path()).unwrap());
    assert_eq!(b.state.trials[1].action, "fight");
    assert_eq!(b.state.trials[1].action_text.as_deref(), Some("Fight hard with the sword (2 hits)"));
    assert!(b.state.trials[2].action_text.is_none());
}

#[test]
fn tried_reply_shows_the_exception_of_a_name_and_keeps_names_out_of_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let svc = Service::new(trials_on(Board::open(dir.path()).unwrap()), "t".into(), 0);
    svc.call("join", &json!({"name": "W1"})).unwrap();
    let enemies = ["wolf", "bat", "spider", "boar", "goblin", "imp", "slime", "crow", "snake", "rat", "bear"];
    for (i, e) in enemies.iter().enumerate() {
        tried(&svc, "A1", json!([format!("level=forest"), format!("enemy={e}"), "time=day"]), "fight", "", "100", &format!("F{i}")).unwrap();
    }
    tried(&svc, "A1", json!(["level=forest", "enemy=troll", "time=day"]), "fight", "", "0", "T1").unwrap();
    let out = tried(&svc, "A1", json!(["level=forest", "enemy=troll", "time=day"]), "fight", "", "0", "T2").unwrap();
    assert!(out.starts_with("Recorded your trial for case type `forest` (level=forest enemy=troll time=day)."), "{out}");
    assert!(out.contains("- forest: WORKING fight (11 pass, last at ") && out.contains(" — except enemy=troll: failed fight 2x\n"), "{out}");
    assert!(out.contains("Key fields for forest: no key field yet; also recorded: enemy (names), time\n"), "{out}");
    assert!(out.contains("Note: `enemy` hold(s) names: kept with your trial, not in the key"), "{out}");
    assert!(out.contains("Case-type fields in use, reuse these names: level (13), enemy (13, names), time (13)"), "{out}");
    let search = svc.call("search", &json!({"query": "troll", "agent_id": "A1"})).unwrap();
    assert!(search.contains("- forest: WORKING fight") && search.contains("except enemy=troll"), "{search}");
    // The stored event keeps every field and the key computed when it was recorded.
    drop(svc);
    let b = trials_on(Board::open(dir.path()).unwrap());
    assert_eq!(b.state.trials[12].fields.pairs_text(), "level=forest enemy=troll time=day");
    assert_eq!(b.state.trials[12].case_type, "forest");
}

#[test]
fn search_lists_the_closest_recorded_cases_for_a_case_s_values() {
    let dir = tempfile::tempdir().unwrap();
    let svc = Service::new(trials_on(Board::open(dir.path()).unwrap()), "t".into(), 0);
    svc.call("join", &json!({"name": "W1"})).unwrap();
    let rec = |f: Value, action: &str, score: &str, case: &str| tried(&svc, "A1", f, action, "", score, case).unwrap();
    rec(json!(["game=quest", "level=forest", "enemy=troll", "time=night"]), "sneak", "100", "Q1");
    rec(json!(["game=quest", "level=forest", "enemy=troll", "time=night"]), "sneak", "100", "Q2");
    rec(json!(["game=quest", "level=forest", "enemy=troll", "time=day"]), "fight", "0", "Q3");
    rec(json!(["game=quest", "level=forest", "enemy=bat", "time=night"]), "fight", "100", "Q4");
    rec(json!(["game=quest", "level=cave", "enemy=bat", "time=day"]), "fight", "100", "Q5");
    // The scorer's query: a case's values joined by spaces.
    let out = svc.call("search", &json!({"query": "quest forest troll night"})).unwrap();
    let i = out.find("Closest recorded cases to your search").expect(&out);
    let lines: Vec<&str> = out[i..].lines().skip(1).take_while(|l| l.starts_with("- ")).collect();
    assert_eq!(
        lines,
        vec![
            "- [game=quest level=forest enemy=troll time=night] sneak PASS 2x",
            "- [game=quest level=forest time=night] fight PASS 1x",
            "- [game=quest level=forest enemy=troll] fight FAIL 1x",
        ],
        "{out}"
    );
    // The closest cases come before the case types and threads.
    assert!(i < out.find("Case types matching your search").unwrap(), "{out}");
    assert!(svc.call("search", &json!({"query": "refund timeout"})).unwrap().find("Closest recorded cases").is_none());
}
