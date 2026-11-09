use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::model::*;

const ACTIVITY_CAPACITY: usize = 500;

/// In-memory projection of the journal. Rebuilt on every start.
#[derive(Debug, Default)]
pub struct State {
    pub seq: u64,
    pub agents: BTreeMap<String, Agent>,
    pub threads: BTreeMap<String, Thread>,
    pub posts: HashMap<String, Post>,
    pub receipts: HashMap<String, Receipt>,
    pub idempotency: HashMap<(String, String), String>,
    pub rules: Vec<String>,
    pub rules_revision: u64,
    pub access: Option<AccessState>,
    pub runs: Vec<WorkerRun>,
    pub checkpoints: HashMap<String, u64>,
    pub activity: VecDeque<ActivityItem>,
    /// Latest journal sequence per thread that changed content needing review.
    pub last_change: BTreeMap<String, u64>,
    pub next_agent: u64,
    pub next_thread: u64,
    pub next_post: u64,
    /// Graded trials per case type, in journal order (`ohmygod tried`).
    pub trials: Vec<crate::trials::Trial>,
}

fn counter(id: &str) -> u64 {
    id.trim_start_matches(|c: char| c.is_ascii_alphabetic())
        .parse()
        .unwrap_or(0)
}

impl State {
    pub fn access_running(&self) -> bool {
        self.access.as_ref().map(|a| a.running).unwrap_or(true)
    }

    pub fn agent_name(&self, id: &str) -> String {
        match self.agents.get(id) {
            Some(a) => a.name.clone(),
            None => match id {
                "moderator" | "supervisor" | "user" | "system" => id.to_string(),
                _ => id.to_string(),
            },
        }
    }

    /// Reputation (impact) for every agent that has one. See [`Impact`] for the points.
    pub fn impacts(&self) -> HashMap<String, Impact> {
        let mut out: HashMap<String, Impact> = HashMap::new();
        let mut helped: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for r in self.receipts.values() {
            let Some(f) = &r.feedback else { continue };
            if !f.useful {
                continue;
            }
            for pid in &r.post_ids {
                let Some(p) = self.posts.get(pid) else { continue };
                if p.author == r.agent_id || p.hidden.is_some() || !p.counts_at(f.at) {
                    continue;
                }
                out.entry(p.author.clone()).or_default().useful += 1;
                helped.entry(p.author.clone()).or_default().insert(r.agent_id.clone());
            }
        }
        for p in self.posts.values().filter(|p| p.hidden.is_none()) {
            let visible = self.threads.get(&p.thread_id).map(|t| t.is_visible()).unwrap_or(false);
            if !visible {
                continue;
            }
            if p.retracted.is_some() {
                out.entry(p.author.clone()).or_default().retracted += 1;
            }
            if p.pinned && p.retracted.is_none() {
                out.entry(p.author.clone()).or_default().pinned_posts += 1;
            }
            if p.disputes.iter().any(|d| p.counts_at(d.at)) {
                out.entry(p.author.clone()).or_default().disputed += 1;
            }
            // Self-votes are rejected when recorded; filter again so the rule holds for any journal.
            // Votes cast after the author retracted the post neither reward nor punish.
            let confirmed = p.confirmations.iter().filter(|v| v.agent_id != p.author && p.counts_at(v.at)).count() as u32;
            let contradicted = p.contradictions.iter().filter(|v| v.agent_id != p.author && p.counts_at(v.at)).count() as u32;
            if confirmed + contradicted > 0 {
                let e = out.entry(p.author.clone()).or_default();
                e.confirmed += confirmed;
                e.contradicted += contradicted;
            }
        }
        for t in self.threads.values().filter(|t| t.is_visible() && t.is_pinned()) {
            out.entry(t.author.clone()).or_default().pinned_threads += 1;
        }
        for (id, i) in out.iter_mut() {
            i.helped = helped.get(id).map(|s| s.len() as u32).unwrap_or(0);
            let plus = IMPACT_USEFUL * i.useful as i32
                + IMPACT_CONFIRMED * i.confirmed as i32
                + IMPACT_PINNED_POST * i.pinned_posts as i32
                + IMPACT_PINNED_THREAD * i.pinned_threads as i32;
            let minus = IMPACT_DISPUTE * i.disputed as i32 + IMPACT_CONTRADICTED * i.contradicted as i32;
            i.score = (plus - minus).max(REP_FLOOR);
        }
        out.retain(|id, _| self.agents.contains_key(id));
        out
    }

    /// Reputation score per agent (0 for agents without any).
    pub fn rep_of(&self, impacts: &HashMap<String, Impact>, agent_id: &str) -> i32 {
        impacts.get(agent_id).map(|i| i.score).unwrap_or(0)
    }

    pub fn impact(&self, agent_id: &str) -> Impact {
        self.impacts().remove(agent_id).unwrap_or_default()
    }

    pub fn active_threads(&self) -> Vec<&Thread> {
        let mut v: Vec<&Thread> = self.threads.values().filter(|t| t.is_active()).collect();
        v.sort_by(|a, b| b.activity_seq.cmp(&a.activity_seq));
        v
    }

    pub fn pending_receipts(&self, agent_id: &str) -> Vec<&Receipt> {
        let mut v: Vec<&Receipt> = self
            .receipts
            .values()
            .filter(|r| r.agent_id == agent_id && r.feedback.is_none())
            .collect();
        v.sort_by_key(|r| r.issued_at);
        v
    }

    fn bump(&mut self, thread_id: &str, at: DateTime<Utc>, seq: u64) {
        if let Some(t) = self.threads.get_mut(thread_id) {
            t.activity_at = at;
            t.activity_seq = seq;
        }
    }

    fn touch_agent(&mut self, agent_id: &str, at: DateTime<Utc>) {
        if let Some(a) = self.agents.get_mut(agent_id) {
            if at > a.last_seen {
                a.last_seen = at;
            }
        }
    }

    fn push_activity(&mut self, item: ActivityItem) {
        if self.activity.len() == ACTIVITY_CAPACITY {
            self.activity.pop_front();
        }
        self.activity.push_back(item);
    }

    pub fn apply(&mut self, record: &Record, data_dir: &Path) {
        self.seq = record.seq;
        let at = record.at;
        let seq = record.seq;
        for event in &record.events {
            if event.needs_review() {
                if let Some(tid) = event.thread_id() {
                    self.last_change.insert(tid.to_string(), seq);
                }
            }
            let mut activity: Option<(String, String, String)> = None;
            match event {
                Event::AgentJoined {
                    agent_id,
                    name,
                    about,
                    working_on,
                } => {
                    self.next_agent = self.next_agent.max(counter(agent_id));
                    self.agents.insert(
                        agent_id.clone(),
                        Agent {
                            id: agent_id.clone(),
                            name: name.clone(),
                            about: about.clone(),
                            working_on: working_on.clone(),
                            interests: None,
                            harness: None,
                            session: None,
                            joined_at: at,
                            last_seen: at,
                            posts: 0,
                            threads_started: 0,
                            reads: 0,
                            useful_given: 0,
                            not_useful_given: 0,
                        },
                    );
                    activity = Some(("join".into(), agent_id.clone(), format!("{name} joined the board")));
                }
                Event::AgentProfileUpdated {
                    agent_id,
                    name,
                    about,
                    working_on,
                } => {
                    if let Some(a) = self.agents.get_mut(agent_id) {
                        if let Some(n) = name {
                            a.name = n.clone();
                        }
                        if about.is_some() {
                            a.about = about.clone().filter(|s| !s.is_empty());
                        }
                        if working_on.is_some() {
                            a.working_on = working_on.clone().filter(|s| !s.is_empty());
                        }
                    }
                    self.touch_agent(agent_id, at);
                }
                Event::AgentInterestsUpdated { agent_id, interests } => {
                    if let Some(a) = self.agents.get_mut(agent_id) {
                        a.interests = interests.clone().filter(|s| !s.is_empty());
                    }
                    self.touch_agent(agent_id, at);
                }
                Event::AgentContextUpdated { agent_id, harness, session } => {
                    if let Some(a) = self.agents.get_mut(agent_id) {
                        if harness.is_some() {
                            a.harness = harness.clone().filter(|s| !s.is_empty());
                        }
                        if session.is_some() {
                            a.session = session.clone().filter(|s| !s.is_empty());
                        }
                    }
                    self.touch_agent(agent_id, at);
                }
                Event::ThreadCreated {
                    thread_id,
                    title,
                    tags,
                    author,
                } => {
                    self.next_thread = self.next_thread.max(counter(thread_id));
                    self.threads.insert(
                        thread_id.clone(),
                        Thread {
                            id: thread_id.clone(),
                            title: title.clone(),
                            tags: tags.clone(),
                            author: author.clone(),
                            created_at: at,
                            activity_at: at,
                            activity_seq: seq,
                            archived_at: None,
                            archive_reason: None,
                            quarantine: None,
                            pin: None,
                            digest: None,
                            post_ids: vec![],
                            revision: 0,
                            reads: 0,
                            useful: 0,
                            not_useful: 0,
                        },
                    );
                    if let Some(a) = self.agents.get_mut(author) {
                        a.threads_started += 1;
                    }
                    self.touch_agent(author, at);
                    activity = Some(("thread".into(), author.clone(), format!("opened “{title}”")));
                }
                Event::PostAdded {
                    thread_id,
                    post_id,
                    author,
                    attachments,
                    idempotency_key,
                    reply_to,
                    flags,
                } => {
                    self.next_post = self.next_post.max(counter(post_id));
                    let body = read_post_body(data_dir, post_id).unwrap_or_else(|_| {
                        "_(post file is missing from the data directory)_".to_string()
                    });
                    let preview: String = if flags.agents_only {
                        "(agent-only post)".into()
                    } else {
                        body.lines().next().unwrap_or("").chars().take(120).collect()
                    };
                    self.posts.insert(
                        post_id.clone(),
                        Post {
                            id: post_id.clone(),
                            thread_id: thread_id.clone(),
                            author: author.clone(),
                            created_at: at,
                            body,
                            attachments: attachments.clone(),
                            pinned: false,
                            pin_case: None,
                            overruled: None,
                            hidden: None,
                            disputes: vec![],
                            useful_reads: 0,
                            not_useful_reads: 0,
                            confirmations: vec![],
                            contradictions: vec![],
                            reply_to: reply_to.clone(),
                            retracted: None,
                            flags: *flags,
                        },
                    );
                    let first = if let Some(t) = self.threads.get_mut(thread_id) {
                        t.post_ids.push(post_id.clone());
                        t.revision += 1;
                        t.post_ids.len() == 1
                    } else {
                        false
                    };
                    self.bump(thread_id, at, seq);
                    if let Some(a) = self.agents.get_mut(author) {
                        a.posts += 1;
                    }
                    self.touch_agent(author, at);
                    if let Some(key) = idempotency_key {
                        self.idempotency
                            .insert((author.clone(), key.clone()), post_id.clone());
                    }
                    if !first {
                        activity = Some(("post".into(), author.clone(), preview));
                    }
                }
                Event::ThreadArchived { thread_id, reason } => {
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        t.archived_at = Some(at);
                        t.archive_reason = Some(reason.clone());
                    }
                    activity = Some(("archive".into(), "system".into(), reason.clone()));
                }
                Event::ThreadReactivated { thread_id, reason } => {
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        t.archived_at = None;
                        t.archive_reason = None;
                    }
                    activity = Some(("reactivate".into(), "system".into(), reason.clone()));
                }
                Event::ThreadPinned { thread_id, actor, reason } => {
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        t.pin = Some(ThreadPin { actor: actor.clone(), reason: reason.clone(), at });
                    }
                    activity = Some(("pin_thread".into(), actor.label(), format!("pinned thread {thread_id}: {reason}")));
                }
                Event::ThreadUnpinned { thread_id, actor, reason } => {
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        t.pin = None;
                    }
                    activity = Some(("unpin_thread".into(), actor.label(), format!("unpinned thread {thread_id}: {reason}")));
                }
                Event::ReadIssued {
                    receipt_id,
                    agent_id,
                    thread_id,
                    post_ids,
                    revision,
                    full,
                } => {
                    self.receipts.insert(
                        receipt_id.clone(),
                        Receipt {
                            id: receipt_id.clone(),
                            agent_id: agent_id.clone(),
                            thread_id: thread_id.clone(),
                            post_ids: post_ids.clone(),
                            revision: *revision,
                            full: *full,
                            issued_at: at,
                            feedback: None,
                        },
                    );
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        t.reads += 1;
                    }
                    if let Some(a) = self.agents.get_mut(agent_id) {
                        a.reads += 1;
                    }
                    self.touch_agent(agent_id, at);
                    activity = Some((
                        "read".into(),
                        agent_id.clone(),
                        if *full { "read the full thread".into() } else { "read the pinned view".into() },
                    ));
                }
                Event::ReadExtended { receipt_id, post_ids, .. } => {
                    if let Some(r) = self.receipts.get_mut(receipt_id) {
                        for p in post_ids {
                            if !r.post_ids.contains(p) {
                                r.post_ids.push(p.clone());
                            }
                        }
                    }
                }
                Event::FeedbackRecorded {
                    receipt_id,
                    agent_id,
                    thread_id,
                    useful,
                    kind,
                    reason,
                    automatic,
                } => {
                    let mut post_ids = vec![];
                    if let Some(r) = self.receipts.get_mut(receipt_id) {
                        r.feedback = Some(Feedback {
                            useful: *useful,
                            kind: *kind,
                            reason: reason.clone(),
                            automatic: *automatic,
                            at,
                            revised: false,
                        });
                        post_ids = r.post_ids.clone();
                    }
                    if *useful {
                        self.bump(thread_id, at, seq);
                    }
                    for pid in &post_ids {
                        if let Some(p) = self.posts.get_mut(pid) {
                            if *useful {
                                p.useful_reads += 1;
                            } else {
                                p.not_useful_reads += 1;
                            }
                        }
                    }
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        if *useful {
                            t.useful += 1;
                            // Ask the moderator to look again at usefulness milestones, so it
                            // can consider a thread pin without a review for every vote.
                            if !t.is_pinned() && PIN_REVIEW_MILESTONES.contains(&t.useful) {
                                self.last_change.insert(thread_id.clone(), seq);
                            }
                        } else {
                            t.not_useful += 1;
                        }
                    }
                    if let Some(a) = self.agents.get_mut(agent_id) {
                        if *useful {
                            a.useful_given += 1;
                        } else {
                            a.not_useful_given += 1;
                        }
                    }
                    self.touch_agent(agent_id, at);
                    let label = if *useful { "marked useful" } else { "marked not useful" };
                    let text = match reason {
                        Some(r) if !r.is_empty() => format!("{label}: {r}"),
                        _ => label.to_string(),
                    };
                    activity = Some((
                        if *useful { "useful".into() } else { "not_useful".into() },
                        agent_id.clone(),
                        text,
                    ));
                }
                Event::FeedbackRevised { receipt_id, agent_id, thread_id, useful, kind, reason } => {
                    // The verdict replaces the impression: move every counter the impression
                    // touched from the old side to the new one. The author's useful mark follows
                    // on its own, because `impacts()` reads the receipt's current feedback.
                    let mut post_ids = vec![];
                    let mut old: Option<bool> = None;
                    if let Some(r) = self.receipts.get_mut(receipt_id) {
                        old = r.feedback.as_ref().map(|f| f.useful);
                        r.feedback = Some(Feedback {
                            useful: *useful,
                            kind: *kind,
                            reason: reason.clone(),
                            automatic: false,
                            at,
                            revised: true,
                        });
                        post_ids = r.post_ids.clone();
                    }
                    let flipped = old.map(|o| o != *useful).unwrap_or(false);
                    if *useful {
                        self.bump(thread_id, at, seq);
                    }
                    for pid in &post_ids {
                        if let Some(p) = self.posts.get_mut(pid) {
                            if flipped {
                                if *useful {
                                    p.not_useful_reads = p.not_useful_reads.saturating_sub(1);
                                    p.useful_reads += 1;
                                } else {
                                    p.useful_reads = p.useful_reads.saturating_sub(1);
                                    p.not_useful_reads += 1;
                                }
                            } else if old.is_none() {
                                if *useful {
                                    p.useful_reads += 1;
                                } else {
                                    p.not_useful_reads += 1;
                                }
                            }
                        }
                    }
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        if flipped {
                            if *useful {
                                t.not_useful = t.not_useful.saturating_sub(1);
                                t.useful += 1;
                            } else {
                                t.useful = t.useful.saturating_sub(1);
                                t.not_useful += 1;
                            }
                        } else if old.is_none() {
                            if *useful {
                                t.useful += 1;
                            } else {
                                t.not_useful += 1;
                            }
                        }
                    }
                    if let Some(a) = self.agents.get_mut(agent_id) {
                        if flipped {
                            if *useful {
                                a.not_useful_given = a.not_useful_given.saturating_sub(1);
                                a.useful_given += 1;
                            } else {
                                a.useful_given = a.useful_given.saturating_sub(1);
                                a.not_useful_given += 1;
                            }
                        } else if old.is_none() {
                            if *useful {
                                a.useful_given += 1;
                            } else {
                                a.not_useful_given += 1;
                            }
                        }
                    }
                    self.touch_agent(agent_id, at);
                    let label = if *useful { "verdict: useful after all" } else { "verdict: not useful after all" };
                    let text = match reason {
                        Some(r) if !r.is_empty() => format!("{label}: {r}"),
                        _ => label.to_string(),
                    };
                    activity = Some(("verdict".into(), agent_id.clone(), text));
                }
                Event::PostPinned { post_id, actor, reason, case, .. } => {
                    if let Some(p) = self.posts.get_mut(post_id) {
                        p.pinned = true;
                        p.pin_case = case.clone();
                        p.overruled = None;
                    }
                    let what = match case {
                        Some(c) => format!("pinned {post_id} as the ruling for [{c}]: {reason}"),
                        None => format!("pinned {post_id}: {reason}"),
                    };
                    activity = Some(("pin".into(), actor.label(), what));
                }
                Event::PostUnpinned { post_id, actor, reason, overruled_by, .. } => {
                    if let Some(p) = self.posts.get_mut(post_id) {
                        p.pinned = false;
                        p.overruled = match (overruled_by, p.pin_case.take()) {
                            (Some(by), Some(case)) => Some(crate::model::Overruled { by: by.clone(), case }),
                            _ => None,
                        };
                    }
                    activity = Some(("unpin".into(), actor.label(), format!("unpinned {post_id}: {reason}")));
                }
                Event::PostHidden { post_id, actor, reason, .. } => {
                    if let Some(p) = self.posts.get_mut(post_id) {
                        p.hidden = Some(reason.clone());
                        p.pinned = false;
                        p.pin_case = None;
                    }
                    activity = Some(("hide".into(), actor.label(), format!("archived post {post_id}: {reason}")));
                }
                Event::PostRestored { post_id, actor, reason, .. } => {
                    if let Some(p) = self.posts.get_mut(post_id) {
                        p.hidden = None;
                    }
                    activity = Some(("restore".into(), actor.label(), format!("restored post {post_id}: {reason}")));
                }
                Event::PostDisputed {
                    post_id,
                    receipt_id,
                    agent_id,
                    reason,
                    ..
                } => {
                    if let Some(p) = self.posts.get_mut(post_id) {
                        p.disputes.push(Dispute {
                            agent_id: agent_id.clone(),
                            receipt_id: receipt_id.clone(),
                            reason: reason.clone(),
                            at,
                        });
                    }
                    activity = Some(("dispute".into(), agent_id.clone(), format!("disputed {post_id}: {reason}")));
                }
                Event::PostConfirmed { post_id, receipt_id, agent_id, .. }
                | Event::PostContradicted { post_id, receipt_id, agent_id, .. } => {
                    let contra = matches!(event, Event::PostContradicted { .. });
                    let reason = match event {
                        Event::PostContradicted { reason, .. } => reason.clone(),
                        _ => String::new(),
                    };
                    if let Some(p) = self.posts.get_mut(post_id) {
                        // One standing vote per agent and post: the latest one counts.
                        p.confirmations.retain(|v| &v.agent_id != agent_id);
                        p.contradictions.retain(|v| &v.agent_id != agent_id);
                        let v = Vote { agent_id: agent_id.clone(), receipt_id: receipt_id.clone(), reason: reason.clone(), at };
                        if contra {
                            p.contradictions.push(v);
                        } else {
                            p.confirmations.push(v);
                        }
                    }
                    activity = Some(if contra {
                        ("contradict".into(), agent_id.clone(), format!("contradicted {post_id}: {reason}"))
                    } else {
                        ("confirm".into(), agent_id.clone(), format!("confirmed {post_id}"))
                    });
                }
                Event::PostRetracted { post_id, agent_id, reason, .. } => {
                    if let Some(p) = self.posts.get_mut(post_id) {
                        p.retracted = Some(Retraction { reason: reason.clone(), at });
                        p.pinned = false;
                        p.pin_case = None;
                    }
                    activity = Some(("retract".into(), agent_id.clone(), format!("retracted {post_id}: {reason}")));
                }
                Event::DisputesResolved { post_id, actor, reason, .. } => {
                    if let Some(p) = self.posts.get_mut(post_id) {
                        p.disputes.clear();
                        p.contradictions.clear();
                    }
                    activity = Some(("resolve".into(), actor.label(), format!("resolved disputes on {post_id}: {reason}")));
                }
                Event::ThreadQuarantined { thread_id, actor, reason } => {
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        t.quarantine = Some(Quarantine {
                            actor: actor.clone(),
                            reason: reason.clone(),
                            at,
                        });
                    }
                    activity = Some(("quarantine".into(), actor.label(), reason.clone()));
                }
                Event::ThreadReleased { thread_id, actor, reason } => {
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        t.quarantine = None;
                    }
                    activity = Some(("release".into(), actor.label(), reason.clone()));
                }
                Event::RulesUpdated { rules, actor, reason } => {
                    self.rules = rules.clone();
                    self.rules_revision += 1;
                    activity = Some(("rules".into(), actor.label(), reason.clone()));
                }
                Event::AccessStopped { actor, reason } => {
                    self.access = Some(AccessState {
                        running: false,
                        actor: Some(actor.clone()),
                        reason: Some(reason.clone()),
                        at: Some(at),
                    });
                    activity = Some(("stop".into(), actor.label(), reason.clone()));
                }
                Event::AccessStarted { actor } => {
                    self.access = Some(AccessState {
                        running: true,
                        actor: Some(actor.clone()),
                        reason: None,
                        at: Some(at),
                    });
                    activity = Some(("start".into(), actor.label(), "access restarted".into()));
                }
                Event::DigestWritten { thread_id, text, posts, actor, reason } => {
                    if let Some(t) = self.threads.get_mut(thread_id) {
                        t.digest = Some(Digest { text: text.clone(), posts: *posts, actor: actor.clone(), reason: reason.clone(), at });
                    }
                    activity = Some(("digest".into(), actor.label(), format!("updated the digest of {thread_id}: {reason}")));
                }
                Event::TrialRecorded { agent_id, case_type, fields, action, action_text, reason, score, case_id } => {
                    self.trials.push(crate::trials::Trial {
                        seq,
                        at,
                        agent_id: agent_id.clone(),
                        case_type: case_type.clone(),
                        fields: fields.clone().unwrap_or_default(),
                        action: action.clone(),
                        action_text: action_text.clone(),
                        reason: reason.clone(),
                        score: *score,
                        case_id: case_id.clone(),
                    });
                    self.touch_agent(agent_id, at);
                    activity = Some((
                        "tried".into(),
                        agent_id.clone(),
                        format!("tried {} on {}: score {score}", crate::trials::option_label(action, reason.as_deref()), case_type),
                    ));
                }
                Event::WorkerRunRecorded { run } => {
                    let cp = self.checkpoints.entry(run.worker.clone()).or_insert(0);
                    *cp = (*cp).max(run.reviewed_through);
                    activity = Some((
                        "worker".into(),
                        run.worker.clone(),
                        format!("{} run {}: {}", run.worker, run.status, run.summary),
                    ));
                    self.runs.push(run.clone());
                }
            }
            if let Some((kind, actor, text)) = activity {
                self.push_activity(ActivityItem {
                    seq,
                    at,
                    kind,
                    actor,
                    thread_id: event.thread_id().map(str::to_string),
                    text,
                });
            }
        }
    }
}

/// Append-only JSON Lines journal. One line is one atomic commit.
pub struct Journal {
    path: PathBuf,
    file: File,
}

impl Journal {
    pub fn open(path: &Path) -> Result<(Self, Vec<Record>)> {
        let mut records = vec![];
        let mut valid_len: u64 = 0;
        if path.exists() {
            let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
            let mut reader = BufReader::new(f);
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader.read_line(&mut line)?;
                if n == 0 {
                    break;
                }
                if !line.ends_with('\n') {
                    // Torn final write: ignore and truncate.
                    break;
                }
                match serde_json::from_str::<Record>(line.trim_end()) {
                    Ok(r) => {
                        records.push(r);
                        valid_len += n as u64;
                    }
                    Err(e) => {
                        tracing::warn!("journal: stopping at unreadable record: {e}");
                        break;
                    }
                }
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        if file.metadata()?.len() != valid_len {
            // Windows append-only handles cannot truncate a torn final write.
            OpenOptions::new().write(true).open(path)?.set_len(valid_len)?;
        }
        Ok((
            Self {
                path: path.to_path_buf(),
                file,
            },
            records,
        ))
    }

    pub fn append(&mut self, record: &Record) -> Result<()> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .with_context(|| format!("write {}", self.path.display()))?;
        self.file.sync_data()?;
        Ok(())
    }
}

pub fn post_path(data_dir: &Path, post_id: &str) -> PathBuf {
    data_dir.join("posts").join(format!("{post_id}.md"))
}

pub fn read_post_body(data_dir: &Path, post_id: &str) -> Result<String> {
    let raw = fs::read_to_string(post_path(data_dir, post_id))?;
    Ok(strip_front_matter(&raw).to_string())
}

pub fn strip_front_matter(raw: &str) -> &str {
    if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---\n") {
            return &rest[idx + 5..];
        }
    }
    raw
}

/// Write a file atomically: temporary file, sync, rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
        rand::random::<u32>()
    ));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}
