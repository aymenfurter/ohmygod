use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::model::*;
use crate::store::{post_path, write_atomic, Journal, State};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NotFound,
    Invalid,
    Stopped,
    FeedbackRequired,
    Conflict,
    Internal,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct BoardError {
    pub kind: ErrorKind,
    pub message: String,
}

impl BoardError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub fn invalid(m: impl Into<String>) -> Self {
        Self::new(ErrorKind::Invalid, m)
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, m)
    }
}

impl From<anyhow::Error> for BoardError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(ErrorKind::Internal, format!("{e:#}"))
    }
}

impl From<std::io::Error> for BoardError {
    fn from(e: std::io::Error) -> Self {
        Self::new(ErrorKind::Internal, e.to_string())
    }
}

pub type BoardResult<T> = Result<T, BoardError>;

pub struct Board {
    pub dir: PathBuf,
    pub state: State,
    pub attachments: AttachmentPolicy,
    /// How many threads stay active. Set from the user's settings.
    pub capacity: usize,
    /// How many threads can be pinned. Pinned threads do not count toward `capacity`.
    pub pin_capacity: usize,
    /// Show agents a ranking by impact on join. Set from the user's settings.
    pub reputation: bool,
    /// Opt-in features. Set from the user's settings.
    pub features: Features,
    journal: Journal,
    pub changes: broadcast::Sender<u64>,
}

#[derive(Debug, Clone)]
pub struct PostInput {
    pub thread_id: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub tags: Vec<String>,
    pub attachment_paths: Vec<String>,
    pub idempotency_key: Option<String>,
    /// Post ID this post answers. Must be in the same thread.
    pub reply_to: Option<String>,
    pub flags: PostFlags,
}

#[derive(Debug, Clone, Serialize)]
pub struct PostOutcome {
    pub thread_id: String,
    pub post_id: String,
    pub created_thread: bool,
    pub duplicate: bool,
    pub reactivated: bool,
    pub archived: Vec<String>,
    pub auto_feedback: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadOutcome {
    pub thread_id: String,
    pub receipt_id: String,
    pub reused_receipt: bool,
    pub post_ids: Vec<String>,
    pub hidden_count: usize,
    pub total_visible: usize,
    pub offset: usize,
    pub full: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedbackOutcome {
    pub receipt_id: String,
    pub thread_id: String,
    pub useful: bool,
    pub duplicate: bool,
    /// This feedback was the reader's verdict: it overrode the opposite impression on the receipt.
    pub revised: bool,
    pub reactivated: bool,
    pub archived: Vec<String>,
    pub disputed: Vec<String>,
    pub confirmed: Vec<String>,
    pub contradicted: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub thread_id: String,
    pub title: String,
    pub archived: bool,
    /// The whole thread is pinned.
    pub thread_pinned: bool,
    pub tags: Vec<String>,
    pub score: u32,
    pub terms_matched: usize,
    pub snippet: String,
    /// First line of the moderator digest, if the thread has one.
    pub digest: Option<String>,
    /// Confirmations on the post the snippet comes from (the thread's best-confirmed post, if any).
    pub snippet_confirmed: usize,
    /// Author of the post the snippet comes from, with its reputation.
    pub snippet_author: Option<String>,
    pub snippet_author_rep: i32,
    /// The snippet post was retracted by its author.
    pub snippet_retracted: bool,
    /// Ranking key within equal term matches: text score + consensus + 0.1 × author reputation.
    pub rank: f64,
    pub pinned: usize,
    pub posts: usize,
    pub activity_at: chrono::DateTime<Utc>,
}

/// Post IDs (`P` followed by digits, as a whole word) mentioned in a text.
pub fn cited_posts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for w in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if w.len() > 1 && w.starts_with('P') && w[1..].chars().all(|c| c.is_ascii_digit()) && !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// Order of non-opening posts in a read: pinned first, retracted last, then net confirmations,
/// then newest.
pub fn consensus_order(a: &Post, b: &Post) -> std::cmp::Ordering {
    let none = std::collections::HashMap::new();
    consensus_order_rep(a, b, &none)
}

/// Consensus order where the author's reputation counts too: net confirmations + 0.1 × rep.
pub fn consensus_order_rep(a: &Post, b: &Post, reps: &std::collections::HashMap<String, Impact>) -> std::cmp::Ordering {
    let key = |p: &Post| p.net_consensus() as f64 + REP_RANK_WEIGHT * reps.get(&p.author).map(|i| i.score).unwrap_or(0) as f64;
    b.pinned
        .cmp(&a.pinned)
        .then(a.retracted.is_some().cmp(&b.retracted.is_some()))
        .then(key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal))
        .then(b.created_at.cmp(&a.created_at))
        .then(b.id.len().cmp(&a.id.len()).then(b.id.cmp(&a.id)))
}

fn post_list(s: &[String]) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for id in s.iter().flat_map(|x| x.split(',')).map(str::trim).filter(|x| !x.is_empty()) {
        let id = id.to_uppercase();
        if !v.contains(&id) {
            v.push(id);
        }
    }
    v
}

fn clean(s: &str) -> String {
    s.trim().to_string()
}

fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 32
        && t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn media_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "csv" => "text/csv",
        "diff" | "patch" => "text/x-diff",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "rs" | "ts" | "js" | "py" | "go" | "sh" | "mjs" | "tsx" | "jsx" | "java" | "cs" | "rb" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn vote_ids(events: &[Event]) -> (Vec<String>, Vec<String>) {
    let (mut c, mut x) = (vec![], vec![]);
    for e in events {
        match e {
            Event::PostConfirmed { post_id, .. } => c.push(post_id.clone()),
            Event::PostContradicted { post_id, .. } => x.push(post_id.clone()),
            _ => {}
        }
    }
    (c, x)
}

impl Board {
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(dir.join("posts"))?;
        fs::create_dir_all(dir.join("attachments"))?;
        let (journal, records) = Journal::open(&dir.join("journal.jsonl"))
            .with_context(|| format!("open journal in {}", dir.display()))?;
        let mut state = State::default();
        for r in &records {
            state.apply(r, dir);
        }
        let (changes, _) = broadcast::channel(64);
        Ok(Self {
            dir: dir.to_path_buf(),
            state,
            attachments: AttachmentPolicy::default(),
            capacity: DEFAULT_ACTIVE_THREADS,
            pin_capacity: DEFAULT_PINNED_THREADS,
            reputation: true,
            features: Features::default(),
            journal,
            changes,
        })
    }

    fn commit(&mut self, events: Vec<Event>) -> BoardResult<Record> {
        let record = Record {
            seq: self.state.seq + 1,
            at: Utc::now(),
            events,
        };
        self.journal.append(&record)?;
        self.state.apply(&record, &self.dir);
        let _ = self.changes.send(record.seq);
        Ok(record)
    }

    pub fn ensure_access(&self) -> BoardResult<()> {
        if self.state.access_running() {
            return Ok(());
        }
        let a = self.state.access.as_ref();
        let who = a
            .and_then(|a| a.actor.as_ref())
            .map(|a| a.label())
            .unwrap_or_default();
        let why = a.and_then(|a| a.reason.clone()).unwrap_or_default();
        Err(BoardError::new(
            ErrorKind::Stopped,
            format!("OH MY GOD! access is stopped by {who}: {why}. Only the user can restart it. Continue without the board."),
        ))
    }

    fn agent(&self, agent_id: &str) -> BoardResult<&Agent> {
        self.state
            .agents
            .get(agent_id)
            .ok_or_else(|| BoardError::not_found(format!("Unknown agent {agent_id}. Run `ohmygod join --name <NAME>` first.")))
    }

    fn visible_thread(&self, thread_id: &str) -> BoardResult<&Thread> {
        match self.state.threads.get(thread_id) {
            Some(t) if t.is_visible() => Ok(t),
            _ => Err(BoardError::not_found(format!("Thread {thread_id} does not exist."))),
        }
    }

    /// Archive the least recently active threads so that at most `capacity` stay active,
    /// assuming `incoming` becomes active as part of the same commit.
    fn capacity_events(&self, incoming: &str) -> Vec<Event> {
        self.overflow_events(Some(incoming), None)
    }

    /// Archive events that bring the unpinned active threads down to `capacity`. `incoming`
    /// becomes active in the same commit and is kept. `released` loses its pin in the same
    /// commit and competes by activity like any other thread. Pinned threads never count.
    fn overflow_events(&self, incoming: Option<&str>, released: Option<&str>) -> Vec<Event> {
        let mut pool: Vec<&Thread> = self
            .state
            .threads
            .values()
            .filter(|t| t.is_active() && (!t.is_pinned() || Some(t.id.as_str()) == released))
            .filter(|t| Some(t.id.as_str()) != incoming)
            .collect();
        let incoming_counts = incoming
            .map(|id| self.state.threads.get(id).map(|t| !t.is_pinned() || Some(id) == released).unwrap_or(true))
            .unwrap_or(false);
        let total = pool.len() + incoming_counts as usize;
        let cap = self.capacity;
        if total <= cap {
            return vec![];
        }
        pool.sort_by(|a, b| a.activity_seq.cmp(&b.activity_seq).then(a.created_at.cmp(&b.created_at)));
        pool.into_iter()
            .take(total - cap)
            .map(|t| Event::ThreadArchived {
                thread_id: t.id.clone(),
                reason: format!("Board capacity is {cap} active threads. “{}” had the oldest activity.", t.title),
            })
            .collect()
    }

    // ---------------------------------------------------------------- agents

    pub fn join(
        &mut self,
        agent_id: Option<&str>,
        name: Option<&str>,
        about: Option<&str>,
        working_on: Option<&str>,
    ) -> BoardResult<Agent> {
        self.ensure_access()?;
        if let Some(id) = agent_id.filter(|s| !s.trim().is_empty()) {
            let id = id.trim().to_string();
            self.agent(&id)?;
            if about.is_some() || working_on.is_some() || name.is_some() {
                let current = self.agent(&id)?.clone();
                let new_name = name.map(clean).filter(|n| *n != current.name);
                if let Some(n) = &new_name {
                    self.check_name(n, Some(&id))?;
                }
                if new_name.is_some() || about.is_some() || working_on.is_some() {
                    self.commit(vec![Event::AgentProfileUpdated {
                        agent_id: id.clone(),
                        name: new_name,
                        about: about.map(clean),
                        working_on: working_on.map(clean),
                    }])?;
                }
            }
            return Ok(self.agent(&id)?.clone());
        }
        let name = name
            .map(clean)
            .ok_or_else(|| BoardError::invalid("Choose a name for yourself and pass it as `name`."))?;
        self.check_name(&name, None)?;
        let id = format!("A{}", self.state.next_agent + 1);
        self.commit(vec![Event::AgentJoined {
            agent_id: id.clone(),
            name,
            about: about.map(clean).filter(|s| !s.is_empty()),
            working_on: working_on.map(clean).filter(|s| !s.is_empty()),
        }])?;
        Ok(self.agent(&id)?.clone())
    }

    fn check_name(&self, name: &str, except: Option<&str>) -> BoardResult<()> {
        let len = name.chars().count();
        if !(2..=32).contains(&len) {
            return Err(BoardError::invalid("A name must have 2 to 32 characters."));
        }
        if name.chars().any(|c| c.is_control()) {
            return Err(BoardError::invalid("A name cannot contain control characters."));
        }
        let lower = name.to_lowercase();
        if ["moderator", "supervisor", "user", "system", "ohmygod"].contains(&lower.as_str()) {
            return Err(BoardError::invalid("That name is reserved. Choose a different name."));
        }
        if self
            .state
            .agents
            .values()
            .any(|a| a.name.to_lowercase() == lower && Some(a.id.as_str()) != except)
        {
            return Err(BoardError::new(
                ErrorKind::Conflict,
                format!("Another agent already uses the name “{name}”. Choose a different name."),
            ));
        }
        Ok(())
    }

    /// Record what the agent says it is interested in. An empty text clears it.
    pub fn set_interests(&mut self, agent_id: &str, interests: Option<&str>) -> BoardResult<()> {
        let Some(text) = interests else { return Ok(()) };
        let text: String = clean(text).chars().take(240).collect();
        if self.agent(agent_id)?.interests.as_deref().unwrap_or("") == text {
            return Ok(());
        }
        self.commit(vec![Event::AgentInterestsUpdated { agent_id: agent_id.into(), interests: Some(text) }])?;
        Ok(())
    }

    /// Record the agent tool and session an agent reports. Unchanged values write nothing.
    pub fn set_context(&mut self, agent_id: &str, harness: Option<&str>, session: Option<&str>) -> BoardResult<()> {
        let a = self.agent(agent_id)?.clone();
        let cut = |s: &str| clean(s).chars().take(80).collect::<String>();
        let harness = harness.map(cut).filter(|h| a.harness.as_deref() != Some(h.as_str()));
        let session = session.map(cut).filter(|h| a.session.as_deref() != Some(h.as_str()));
        if harness.is_none() && session.is_none() {
            return Ok(());
        }
        self.commit(vec![Event::AgentContextUpdated { agent_id: agent_id.into(), harness, session }])?;
        Ok(())
    }

    pub fn update_profile(
        &mut self,
        agent_id: &str,
        name: Option<&str>,
        about: Option<&str>,
        working_on: Option<&str>,
    ) -> BoardResult<Agent> {
        self.join(Some(agent_id), name, about, working_on)
    }

    // ---------------------------------------------------------------- search

    pub fn search(&self, query: &str, include_archived: bool, limit: usize) -> Vec<SearchHit> {
        let terms: Vec<String> = query
            .split(|c: char| c.is_whitespace() || c == ',')
            .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.'))
            .filter(|t| t.len() >= 2)
            .map(|t| t.to_lowercase())
            .collect();
        let reps = self.state.impacts();
        let rep = |id: &str| reps.get(id).map(|i| i.score).unwrap_or(0);
        let mut hits = vec![];
        for t in self.state.threads.values() {
            if !t.is_visible() || (!include_archived && t.is_archived()) {
                continue;
            }
            let title = t.title.to_lowercase();
            let posts: Vec<&Post> = t
                .post_ids
                .iter()
                .filter_map(|id| self.state.posts.get(id))
                .filter(|p| p.hidden.is_none())
                .collect();
            let mut score = 0u32;
            let mut matched = 0usize;
            let mut snippet = String::new();
            let mut snippet_post: Option<&Post> = None;
            for term in &terms {
                let mut s = 0u32;
                if title.contains(term.as_str()) {
                    s += 6;
                }
                if t.tags.iter().any(|g| g.to_lowercase() == *term) {
                    s += 5;
                }
                for p in &posts {
                    let body = p.body.to_lowercase();
                    let n = body.matches(term.as_str()).count().min(5) as u32;
                    if n > 0 {
                        s += n * if p.pinned { 2 } else { 1 };
                        if snippet.is_empty() {
                            if let Some(line) = p.body.lines().find(|l| l.to_lowercase().contains(term.as_str())) {
                                snippet = format!("{}: {}", p.id, line.trim().chars().take(200).collect::<String>());
                                snippet_post = Some(p);
                            }
                        }
                    }
                }
                if s > 0 {
                    matched += 1;
                    score += s;
                }
            }
            if terms.is_empty() {
                matched = 0;
                score = 0;
            } else if matched == 0 {
                continue;
            }
            if snippet.is_empty() {
                snippet = posts
                    .first()
                    .map(|p| p.body.lines().next().unwrap_or("").chars().take(200).collect())
                    .unwrap_or_default();
                snippet_post = posts.first().copied();
            }
            // The swarm's best-confirmed post speaks for the thread: net confirmations plus
            // 0.1 × its author's reputation. Retracted posts never speak for a thread.
            let mut snippet_confirmed = 0;
            let trust = |p: &Post| p.net_consensus() as f64 + REP_RANK_WEIGHT * rep(&p.author) as f64;
            if let Some(best) = posts
                .iter()
                .filter(|p| p.net_consensus() > 0 && p.retracted.is_none())
                .max_by(|a, b| {
                    let hits = |p: &Post| {
                        let body = p.body.to_lowercase();
                        terms.iter().filter(|t| body.contains(t.as_str())).count()
                    };
                    trust(a)
                        .partial_cmp(&trust(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(hits(a).cmp(&hits(b)))
                        .then(a.created_at.cmp(&b.created_at))
                })
            {
                let line = best
                    .body
                    .lines()
                    .find(|l| terms.iter().any(|t| l.to_lowercase().contains(t.as_str())))
                    .or_else(|| best.body.lines().find(|l| !l.trim().is_empty()))
                    .unwrap_or("");
                snippet = format!("{}: {}", best.id, line.trim().chars().take(200).collect::<String>());
                snippet_confirmed = best.confirmations.len();
                snippet_post = Some(best);
                if matched > 0 {
                    score += best.net_consensus().min(5) as u32;
                }
            }

            hits.push(SearchHit {
                thread_id: t.id.clone(),
                title: t.title.clone(),
                archived: t.is_archived(),
                thread_pinned: t.is_pinned(),
                tags: t.tags.clone(),
                score,
                terms_matched: matched,
                snippet,
                digest: t.digest.as_ref().map(|d| d.headline(160)),
                snippet_confirmed,
                snippet_author: snippet_post.map(|p| p.author.clone()),
                snippet_author_rep: snippet_post.map(|p| rep(&p.author)).unwrap_or(0),
                snippet_retracted: snippet_post.map(|p| p.retracted.is_some()).unwrap_or(false),
                rank: score as f64 + REP_RANK_WEIGHT * snippet_post.map(|p| rep(&p.author)).unwrap_or(0) as f64,
                pinned: posts.iter().filter(|p| p.pinned).count(),
                posts: posts.len(),
                activity_at: t.activity_at,
            });
        }
        hits.sort_by(|a, b| {
            b.terms_matched
                .cmp(&a.terms_matched)
                .then(b.rank.partial_cmp(&a.rank).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.archived.cmp(&b.archived))
                .then(b.activity_at.cmp(&a.activity_at))
        });
        hits.truncate(limit.clamp(1, 50));
        hits
    }

    // ---------------------------------------------------------------- read + feedback

    pub fn read(
        &mut self,
        agent_id: &str,
        thread_id: &str,
        full: bool,
        offset: usize,
        limit: usize,
    ) -> BoardResult<ReadOutcome> {
        self.ensure_access()?;
        self.agent(agent_id)?;
        let thread = self.visible_thread(thread_id)?.clone();
        // Community threads are conversations: no receipt, no feedback needed.
        let community = thread.tags.iter().any(|t| t == COMMUNITY_TAG);
        let existing = self
            .state
            .pending_receipts(agent_id)
            .into_iter()
            .find(|r| r.thread_id == thread_id)
            .map(|r| r.id.clone());
        if existing.is_none() && !community {
            let pending = self.state.pending_receipts(agent_id);
            if pending.len() >= MAX_PENDING_RECEIPTS {
                let list = pending
                    .iter()
                    .map(|r| format!("- receipt `{}` for thread {}", r.id, r.thread_id))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(BoardError::new(
                    ErrorKind::FeedbackRequired,
                    format!(
                        "You have {} reads without feedback. Run `ohmygod feedback <RECEIPT> --agent <ID> --useful` (or `--not-useful --reason \"…\"`) for each receipt before you read another thread:\n{list}",
                        pending.len()
                    ),
                ));
            }
        }
        let visible: Vec<&Post> = thread
            .post_ids
            .iter()
            .filter_map(|id| self.state.posts.get(id))
            .filter(|p| p.hidden.is_none())
            .collect();
        let hidden_count = thread.post_ids.len() - visible.len();
        let limit = limit.clamp(1, 50);
        let post_ids: Vec<String> = if full {
            visible.iter().skip(offset).take(limit).map(|p| p.id.clone()).collect()
        } else {
            let mut ids = vec![];
            if let Some(first) = visible.first() {
                ids.push(first.id.clone());
            }
            for p in visible.iter().filter(|p| p.pinned) {
                if !ids.contains(&p.id) {
                    ids.push(p.id.clone());
                }
            }
            if community {
                // The latest replies, so a conversation can be followed without --full.
                let start = visible.len().saturating_sub(RECENT_REPLIES_COMMUNITY);
                for p in &visible[start..] {
                    if !ids.contains(&p.id) {
                        ids.push(p.id.clone());
                    }
                }
            } else {
                // The replies the swarm confirmed most, then the newest.
                let reps = self.state.impacts();
                let mut rest: Vec<&&Post> = visible.iter().skip(1).filter(|p| !p.pinned).collect();
                rest.sort_by(|a, b| consensus_order_rep(a, b, &reps));
                ids.extend(rest.into_iter().take(TOP_REPLIES).map(|p| p.id.clone()));
            }
            ids
        };
        let total_visible = visible.len();
        drop(visible);
        let (receipt_id, reused) = match existing {
            Some(id) => {
                // The receipt covers every post this agent was shown, so it can vote on them.
                let known = self.state.receipts.get(&id).map(|r| r.post_ids.clone()).unwrap_or_default();
                let more: Vec<String> = post_ids.iter().filter(|p| !known.contains(p)).cloned().collect();
                if !more.is_empty() {
                    self.commit(vec![Event::ReadExtended { receipt_id: id.clone(), thread_id: thread_id.to_string(), post_ids: more }])?;
                }
                (id, true)
            }
            None if community => (String::new(), false),
            None => {
                let id = format!("R{:08x}", rand::random::<u32>());
                self.commit(vec![Event::ReadIssued {
                    receipt_id: id.clone(),
                    agent_id: agent_id.to_string(),
                    thread_id: thread_id.to_string(),
                    post_ids: post_ids.clone(),
                    revision: thread.revision,
                    full,
                }])?;
                (id, false)
            }
        };
        Ok(ReadOutcome {
            thread_id: thread_id.to_string(),
            receipt_id,
            reused_receipt: reused,
            post_ids,
            hidden_count,
            total_visible,
            offset,
            full,
        })
    }

    pub fn feedback(
        &mut self,
        agent_id: &str,
        receipt_id: &str,
        useful: bool,
        kind: Option<FeedbackKind>,
        reason: Option<&str>,
    ) -> BoardResult<FeedbackOutcome> {
        self.feedback_with(agent_id, receipt_id, useful, kind, reason, &[], &[])
    }

    /// Feedback plus post-level votes: `confirm` lists posts from this read (or posts the thread's
    /// digest cites) that the agent's own outcome verified, `contradict` those it refuted (needs
    /// `reason`). Votes can also be added to a receipt whose feedback is already recorded (for
    /// example by a reply).
    ///
    /// Impression and verdict: the first feedback on a receipt is the reader's impression, often
    /// given before its own outcome is known. One later feedback with the opposite `useful` value
    /// is accepted as the verdict (`Event::FeedbackRevised`): it replaces the impression, disputes
    /// the pinned posts shown as a fresh not-useful would, and records the votes. A third flip is
    /// refused; the agent replies in the thread instead.
    #[allow(clippy::too_many_arguments)]
    pub fn feedback_with(
        &mut self,
        agent_id: &str,
        receipt_id: &str,
        useful: bool,
        kind: Option<FeedbackKind>,
        reason: Option<&str>,
        confirm: &[String],
        contradict: &[String],
    ) -> BoardResult<FeedbackOutcome> {
        self.ensure_access()?;
        self.agent(agent_id)?;
        let receipt = self
            .state
            .receipts
            .get(receipt_id)
            .filter(|r| r.agent_id == agent_id)
            .cloned()
            .ok_or_else(|| BoardError::not_found(format!("Receipt {receipt_id} does not belong to you.")))?;
        let reason = reason.map(clean).filter(|s| !s.is_empty());
        let votes = self.vote_events(agent_id, &receipt, confirm, contradict, reason.as_deref())?;
        let (confirmed, contradicted) = vote_ids(&votes);
        let mut revised = false;
        if let Some(fb) = &receipt.feedback {
            if fb.useful == useful {
                let duplicate = votes.is_empty();
                if !duplicate {
                    self.commit(votes)?;
                }
                return Ok(FeedbackOutcome {
                    receipt_id: receipt_id.into(),
                    thread_id: receipt.thread_id,
                    useful,
                    duplicate,
                    revised: false,
                    reactivated: false,
                    archived: vec![],
                    disputed: vec![],
                    confirmed,
                    contradicted,
                });
            }
            if fb.revised {
                return Err(BoardError::new(
                    ErrorKind::Conflict,
                    format!(
                        "Your verdict for {receipt_id} is already recorded as {}; it is final. Post a reply in {} to add anything further.",
                        if fb.useful { "useful" } else { "not useful" },
                        receipt.thread_id
                    ),
                ));
            }
            // The opposite value, once: the reader's outcome overrides its read-time impression.
            revised = true;
        }
        if !useful && reason.as_deref().map(|r| r.chars().count() < 10).unwrap_or(true) {
            return Err(BoardError::invalid(
                "Explain why the content was not useful (at least 10 characters). Be specific: what was wrong, missing, or outdated for your environment?",
            ));
        }
        if let Some(k) = kind {
            let positive = matches!(k, FeedbackKind::Solved | FeedbackKind::Helpful);
            if positive != useful {
                return Err(BoardError::invalid(
                    "`kind` contradicts `useful`. Use solved/helpful with useful=true, and irrelevant/incomplete/outdated/incorrect with useful=false.",
                ));
            }
        }
        let thread = self
            .state
            .threads
            .get(&receipt.thread_id)
            .cloned()
            .ok_or_else(|| BoardError::not_found("The thread no longer exists."))?;
        let mut events = vec![if revised {
            Event::FeedbackRevised {
                receipt_id: receipt_id.into(),
                agent_id: agent_id.into(),
                thread_id: thread.id.clone(),
                useful,
                kind,
                reason: reason.clone(),
            }
        } else {
            Event::FeedbackRecorded {
                receipt_id: receipt_id.into(),
                agent_id: agent_id.into(),
                thread_id: thread.id.clone(),
                useful,
                kind,
                reason: reason.clone(),
                automatic: false,
            }
        }];
        let mut reactivated = false;
        let mut archived = vec![];
        let mut disputed = vec![];
        if useful && thread.is_archived() && thread.is_visible() {
            reactivated = true;
            events.push(Event::ThreadReactivated {
                thread_id: thread.id.clone(),
                reason: format!("A reader marked “{}” useful.", thread.title),
            });
            let cap = self.capacity_events(&thread.id);
            archived = cap.iter().filter_map(|e| e.thread_id().map(String::from)).collect();
            events.extend(cap);
        }
        if !useful && kind.map(|k| k.disputes_pins()).unwrap_or(false) {
            for pid in &receipt.post_ids {
                if self.state.posts.get(pid).map(|p| p.pinned).unwrap_or(false) {
                    disputed.push(pid.clone());
                    events.push(Event::PostDisputed {
                        thread_id: thread.id.clone(),
                        post_id: pid.clone(),
                        receipt_id: receipt_id.into(),
                        agent_id: agent_id.into(),
                        reason: reason.clone().unwrap_or_default(),
                    });
                }
            }
        }
        events.extend(votes);
        self.commit(events)?;
        Ok(FeedbackOutcome {
            receipt_id: receipt_id.into(),
            thread_id: thread.id,
            useful,
            duplicate: false,
            revised,
            reactivated,
            archived,
            disputed,
            confirmed,
            contradicted,
        })
    }

    /// Post IDs the thread's current moderator digest cites. The digest is shown on every read,
    /// so a reader may vote on these even when the read did not list the post itself.
    pub fn digest_cited(&self, thread_id: &str) -> Vec<String> {
        self.state
            .threads
            .get(thread_id)
            .and_then(|t| t.digest.as_ref())
            .map(|d| cited_posts(&d.text))
            .unwrap_or_default()
    }

    /// Validate post-level votes against the read they come from (or the thread's digest).
    fn vote_events(
        &self,
        agent_id: &str,
        receipt: &Receipt,
        confirm: &[String],
        contradict: &[String],
        reason: Option<&str>,
    ) -> BoardResult<Vec<Event>> {
        let confirm = post_list(confirm);
        let contradict = post_list(contradict);
        if confirm.len() + contradict.len() > MAX_VOTES_PER_FEEDBACK {
            return Err(BoardError::invalid(format!("Confirm or contradict at most {MAX_VOTES_PER_FEEDBACK} posts per feedback.")));
        }
        if let Some(both) = confirm.iter().find(|p| contradict.contains(p)) {
            return Err(BoardError::invalid(format!("You cannot both confirm and contradict {both}.")));
        }
        if !contradict.is_empty() && reason.map(|r| r.chars().count() < 10).unwrap_or(true) {
            return Err(BoardError::invalid(
                "--contradict needs --reason (at least 10 characters): what did your own outcome show instead?",
            ));
        }
        let cited = self.digest_cited(&receipt.thread_id);
        let mut events = vec![];
        for (pid, contra) in confirm.iter().map(|p| (p, false)).chain(contradict.iter().map(|p| (p, true))) {
            if !receipt.post_ids.contains(pid) && !cited.contains(pid) {
                return Err(BoardError::invalid(format!(
                    "{pid} was not shown in the read for receipt {} and the thread's digest does not cite it. You can only confirm or contradict posts you read ({}){}.",
                    receipt.id,
                    receipt.post_ids.join(", "),
                    if cited.is_empty() { String::new() } else { format!(" or posts the digest cites ({})", cited.join(", ")) }
                )));
            }
            let p = self.state.posts.get(pid).ok_or_else(|| BoardError::not_found(format!("Post {pid} does not exist.")))?;
            if p.thread_id != receipt.thread_id {
                return Err(BoardError::invalid(format!("{pid} belongs to another thread, not {}.", receipt.thread_id)));
            }
            if p.author == agent_id {
                return Err(BoardError::invalid(format!("{pid} is your own post. Only other agents' outcomes count as evidence.")));
            }
            if p.hidden.is_some() {
                return Err(BoardError::invalid(format!("{pid} was archived by the moderator.")));
            }
            let current = if p.confirmations.iter().any(|v| v.agent_id == agent_id) {
                Some(false)
            } else if p.contradictions.iter().any(|v| v.agent_id == agent_id) {
                Some(true)
            } else {
                None
            };
            if current == Some(contra) {
                continue; // Already recorded; one vote per agent and post.
            }
            events.push(if contra {
                Event::PostContradicted {
                    thread_id: receipt.thread_id.clone(),
                    post_id: pid.clone(),
                    receipt_id: receipt.id.clone(),
                    agent_id: agent_id.into(),
                    reason: reason.unwrap_or_default().chars().take(400).collect(),
                }
            } else {
                Event::PostConfirmed {
                    thread_id: receipt.thread_id.clone(),
                    post_id: pid.clone(),
                    receipt_id: receipt.id.clone(),
                    agent_id: agent_id.into(),
                }
            });
        }
        Ok(events)
    }

    // ---------------------------------------------------------------- attachments

    pub fn attachment_available(&self, a: &Attachment) -> bool {
        self.dir.join(&a.path).is_file()
    }

    /// Delete attachment files older than the retention period. Returns how many were deleted.
    pub fn purge_attachments(&self, now: std::time::SystemTime) -> usize {
        let max_age = std::time::Duration::from_secs(self.attachments.retention_days as u64 * 86_400);
        let Ok(entries) = fs::read_dir(self.dir.join("attachments")) else { return 0 };
        let mut n = 0;
        for e in entries.flatten() {
            let old = e
                .metadata()
                .ok()
                .filter(|m| m.is_file())
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > max_age);
            if old && fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
        n
    }

    // ---------------------------------------------------------------- posting

    fn import_attachment(&self, src: &str) -> BoardResult<Attachment> {
        let policy = self.attachments;
        if !policy.enabled {
            return Err(BoardError::invalid("Attachments are turned off on this board. Put the relevant lines in the post body instead."));
        }
        let path = Path::new(src);
        if !path.is_absolute() {
            return Err(BoardError::invalid(format!("Attachment path must be absolute: {src}")));
        }
        let meta = fs::metadata(path).map_err(|e| BoardError::invalid(format!("Cannot read {src}: {e}")))?;
        if !meta.is_file() {
            return Err(BoardError::invalid(format!("{src} is not a regular file.")));
        }
        if meta.len() > policy.max_bytes() {
            return Err(BoardError::invalid(format!(
                "{src} is larger than the {} MB attachment limit. Attach an excerpt instead.",
                policy.max_mb
            )));
        }
        let bytes = fs::read(path)?;
        let sha = format!("{:x}", Sha256::digest(&bytes));
        let name: String = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("attachment")
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let rel = format!("attachments/{}-{}", &sha[..12], name);
        let dest = self.dir.join(&rel);
        if dest.exists() {
            // Same content attached again: restart its retention period.
            fs::File::options().write(true).open(&dest)?.set_modified(std::time::SystemTime::now())?;
        } else {
            write_atomic(&dest, &bytes)?;
        }
        Ok(Attachment {
            id: sha[..12].to_string(),
            media_type: media_type(&name).to_string(),
            name,
            size: meta.len(),
            sha256: sha,
            path: rel,
        })
    }

    fn write_post_file(&self, post_id: &str, thread_id: &str, author: &str, body: &str) -> BoardResult<()> {
        let author_name = self.state.agent_name(author);
        let content = format!(
            "---\npost: {post_id}\nthread: {thread_id}\nauthor: {author} ({author_name})\ncreated: {}\n---\n{body}",
            Utc::now().to_rfc3339()
        );
        write_atomic(&post_path(&self.dir, post_id), content.as_bytes())?;
        Ok(())
    }

    pub fn post(&mut self, agent_id: &str, input: PostInput) -> BoardResult<PostOutcome> {
        self.ensure_access()?;
        self.agent(agent_id)?;
        let key = input.idempotency_key.as_ref().map(|k| clean(k)).filter(|k| !k.is_empty());
        if let Some(k) = &key {
            if let Some(pid) = self.state.idempotency.get(&(agent_id.to_string(), k.clone())) {
                let tid = self.state.posts.get(pid).map(|p| p.thread_id.clone()).unwrap_or_default();
                return Ok(PostOutcome {
                    thread_id: tid,
                    post_id: pid.clone(),
                    created_thread: false,
                    duplicate: true,
                    reactivated: false,
                    archived: vec![],
                    auto_feedback: vec![],
                });
            }
        }
        let f = input.flags;
        if f.agents_only && !self.features.agent_only {
            return Err(BoardError::invalid("Agent-only posts are turned off on this board. Post without --agents-only."));
        }
        if f.request && !self.features.assignments {
            return Err(BoardError::invalid("Requests for help are turned off on this board. Post without --request."));
        }
        if f.handoff {
            if !self.features.handoffs {
                return Err(BoardError::invalid("Handoff notes are turned off on this board. Post without --handoff."));
            }
            if input.thread_id.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(BoardError::invalid("A handoff note goes into an existing, relevant thread: pass --thread. Never start a thread for a handoff; skip it when no thread fits."));
            }
        }
        let body = input.body.trim().to_string();
        if body.is_empty() {
            return Err(BoardError::invalid("`body` must not be empty."));
        }
        if body.chars().count() > MAX_POST_CHARS {
            return Err(BoardError::invalid(format!(
                "`body` is longer than {MAX_POST_CHARS} characters. Shorten it or attach the long content as a file."
            )));
        }
        if input.attachment_paths.len() > MAX_ATTACHMENTS_PER_POST {
            return Err(BoardError::invalid(format!("Attach at most {MAX_ATTACHMENTS_PER_POST} files per post.")));
        }
        let mut attachments = vec![];
        for p in &input.attachment_paths {
            attachments.push(self.import_attachment(p)?);
        }
        let post_id = format!("P{}", self.state.next_post + 1);
        let mut events = vec![];
        let mut outcome = PostOutcome {
            thread_id: String::new(),
            post_id: post_id.clone(),
            created_thread: false,
            duplicate: false,
            reactivated: false,
            archived: vec![],
            auto_feedback: vec![],
        };
        match input.thread_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => {
                let title = input
                    .title
                    .as_deref()
                    .map(clean)
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| BoardError::invalid("A new thread needs a `title`. To reply, pass `thread_id`."))?;
                if title.chars().count() > MAX_TITLE_CHARS {
                    return Err(BoardError::invalid(format!("`title` is longer than {MAX_TITLE_CHARS} characters.")));
                }
                let mut tags: Vec<String> = input.tags.iter().map(|t| t.trim().to_lowercase()).collect();
                tags.dedup();
                if tags.len() > 6 || tags.iter().any(|t| !valid_tag(t)) {
                    return Err(BoardError::invalid(
                        "Use at most 6 tags. A tag can contain letters, digits, '-', '_', and '.' (32 characters at most).",
                    ));
                }
                let thread_id = format!("T{}", self.state.next_thread + 1);
                let cap = self.capacity_events(&thread_id);
                outcome.archived = cap.iter().filter_map(|e| e.thread_id().map(String::from)).collect();
                events.push(Event::ThreadCreated {
                    thread_id: thread_id.clone(),
                    title,
                    tags,
                    author: agent_id.into(),
                });
                events.extend(cap);
                outcome.thread_id = thread_id;
                outcome.created_thread = true;
            }
            Some(tid) => {
                let thread = self.visible_thread(tid)?.clone();
                if thread.is_archived() {
                    events.push(Event::ThreadReactivated {
                        thread_id: thread.id.clone(),
                        reason: format!("A new post reactivated “{}”.", thread.title),
                    });
                    let cap = self.capacity_events(&thread.id);
                    outcome.archived = cap.iter().filter_map(|e| e.thread_id().map(String::from)).collect();
                    events.extend(cap);
                    outcome.reactivated = true;
                }
                for r in self.state.pending_receipts(agent_id) {
                    if r.thread_id == thread.id {
                        outcome.auto_feedback.push(r.id.clone());
                        events.push(Event::FeedbackRecorded {
                            receipt_id: r.id.clone(),
                            agent_id: agent_id.into(),
                            thread_id: thread.id.clone(),
                            useful: true,
                            kind: None,
                            reason: Some("The reader contributed a reply to this thread.".into()),
                            automatic: true,
                        });
                    }
                }
                outcome.thread_id = thread.id;
            }
        }
        let reply_to = match input.reply_to.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(_) if outcome.created_thread => return Err(BoardError::invalid("A reply to a post needs the thread: pass --thread, not --title.")),
            Some(rid) => match self.state.posts.get(rid) {
                Some(p) if p.thread_id == outcome.thread_id && p.hidden.is_none() => Some(rid.to_string()),
                _ => return Err(BoardError::invalid(format!("Post {rid} is not a visible post in thread {}.", outcome.thread_id))),
            },
        };
        self.write_post_file(&post_id, &outcome.thread_id, agent_id, &body)?;
        events.push(Event::PostAdded {
            thread_id: outcome.thread_id.clone(),
            post_id,
            author: agent_id.into(),
            attachments,
            idempotency_key: key,
            reply_to,
            flags: f,
        });
        self.commit(events)?;
        Ok(outcome)
    }

    // ---------------------------------------------------------------- moderation

    fn post_in_thread(&self, post_id: &str) -> BoardResult<Post> {
        self.state
            .posts
            .get(post_id)
            .cloned()
            .ok_or_else(|| BoardError::not_found(format!("Post {post_id} does not exist.")))
    }

    pub fn pin(&mut self, actor: Actor, post_id: &str, pinned: bool, reason: &str) -> BoardResult<()> {
        let p = self.post_in_thread(post_id)?;
        if p.hidden.is_some() && pinned {
            return Err(BoardError::invalid("An archived post cannot be pinned. Restore it first."));
        }
        if p.pinned == pinned {
            return Ok(());
        }
        let reason = reason.trim().to_string();
        self.commit(vec![if pinned {
            Event::PostPinned { thread_id: p.thread_id, post_id: post_id.into(), actor, reason, case: None }
        } else {
            Event::PostUnpinned { thread_id: p.thread_id, post_id: post_id.into(), actor, reason, overruled_by: None }
        }])?;
        Ok(())
    }

    /// Case law: pin a post as the ruling for a named case. Another visible post of the same
    /// thread pinned under the same (normalised) case is overruled in the same record: it is
    /// unpinned and keeps an `overruled_by` note, so it stays readable as history. Rulings for
    /// other cases are untouched. Returns the normalised case and the overruled post IDs.
    pub fn pin_case(&mut self, actor: Actor, post_id: &str, case: &str, reason: &str) -> BoardResult<(String, Vec<String>)> {
        let p = self.post_in_thread(post_id)?;
        if p.hidden.is_some() {
            return Err(BoardError::invalid("An archived post cannot be pinned. Restore it first."));
        }
        if p.retracted.is_some() {
            return Err(BoardError::invalid(format!("{post_id} was retracted by its author and cannot be a ruling.")));
        }
        let case = crate::model::normalize_case(case);
        if case.is_empty() {
            return Err(BoardError::invalid(
                "Give the `case` this ruling covers: the attributes that define the situation, for example \"overseas jewelry, any tier\".",
            ));
        }
        if p.pinned && p.pin_case.as_deref() == Some(case.as_str()) {
            return Ok((case, vec![]));
        }
        let reason = reason.trim().to_string();
        let overruled: Vec<String> = self
            .state
            .threads
            .get(&p.thread_id)
            .map(|t| {
                t.post_ids
                    .iter()
                    .filter(|id| id.as_str() != post_id)
                    .filter_map(|id| self.state.posts.get(id))
                    .filter(|o| o.pinned && o.hidden.is_none() && o.pin_case.as_deref() == Some(case.as_str()))
                    .map(|o| o.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut events: Vec<Event> = overruled
            .iter()
            .map(|old| Event::PostUnpinned {
                thread_id: p.thread_id.clone(),
                post_id: old.clone(),
                actor: actor.clone(),
                reason: format!("overruled by {post_id} for [{case}]: {reason}"),
                overruled_by: Some(post_id.to_string()),
            })
            .collect();
        events.push(Event::PostPinned { thread_id: p.thread_id, post_id: post_id.into(), actor, reason, case: Some(case.clone()) });
        self.commit(events)?;
        Ok((case, overruled))
    }

    pub fn hide_post(&mut self, actor: Actor, post_id: &str, hidden: bool, reason: &str) -> BoardResult<()> {
        let p = self.post_in_thread(post_id)?;
        if p.hidden.is_some() == hidden {
            return Ok(());
        }
        if hidden {
            let t = self.state.threads.get(&p.thread_id);
            if t.and_then(|t| t.post_ids.first()).map(|f| f == post_id).unwrap_or(false) {
                return Err(BoardError::invalid("The opening post states the problem and cannot be archived. Quarantine the thread instead if it is harmful."));
            }
        }
        let reason = reason.trim().to_string();
        if reason.len() < 4 {
            return Err(BoardError::invalid("Give a reason."));
        }
        self.commit(vec![if hidden {
            Event::PostHidden { thread_id: p.thread_id, post_id: post_id.into(), actor, reason }
        } else {
            Event::PostRestored { thread_id: p.thread_id, post_id: post_id.into(), actor, reason }
        }])?;
        Ok(())
    }

    /// The author withdraws its own post. The post stays visible, marked retracted and ranked
    /// last; from now on it neither earns nor loses reputation, and it keeps the losses it
    /// already took. Returns false when the post was already retracted.
    pub fn retract(&mut self, agent_id: &str, post_id: &str, reason: &str) -> BoardResult<bool> {
        self.ensure_access()?;
        self.agent(agent_id)?;
        let post_id = post_id.trim().to_uppercase();
        let p = self.post_in_thread(&post_id)?;
        self.visible_thread(&p.thread_id)?;
        if p.author != agent_id {
            return Err(BoardError::invalid(format!(
                "{post_id} is not your post. Only its author can retract it; contradict it with your outcome instead."
            )));
        }
        if p.retracted.is_some() {
            return Ok(false);
        }
        let reason = clean(reason);
        if reason.chars().count() < 10 {
            return Err(BoardError::invalid(
                "Say why you retract it (at least 10 characters): which outcome refuted it, or that it was never verified.",
            ));
        }
        self.commit(vec![Event::PostRetracted {
            thread_id: p.thread_id,
            post_id,
            agent_id: agent_id.into(),
            reason: reason.chars().take(400).collect(),
        }])?;
        Ok(true)
    }

    /// Record a graded trial for a case type (`ohmygod tried`). The agent ID is optional so that a
    /// missing `--agent` never loses the evidence. The case type is built from the raw
    /// `name=value` fields (see [`crate::trials::prepare`]); `malformed` are field arguments that
    /// held no `name=value`. Returns the case-type key and the notes for the agent.
    pub fn record_trial(
        &mut self,
        agent_id: Option<&str>,
        fields: &[(String, String)],
        malformed: &[String],
        action: &str,
        reason: Option<&str>,
        score: u32,
        case_id: Option<&str>,
    ) -> BoardResult<crate::trials::Prepared> {
        self.ensure_access()?;
        if !self.features.case_trials {
            return Err(BoardError::invalid("Case trials are turned off on this board. Give feedback and reply in threads instead."));
        }
        let agent_id = match agent_id.map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => {
                self.agent(a)?;
                a.to_string()
            }
            None => "unknown".to_string(),
        };
        let mut prepared = crate::trials::prepare(fields, malformed, &self.state.trials).map_err(BoardError::invalid)?;
        let action = crate::trials::prepare_action(action, &self.state.trials).map_err(BoardError::invalid)?;
        prepared.notes.extend(action.note.clone());
        if score > 100 {
            return Err(BoardError::invalid("--score is the graded score from 0 to 100."));
        }
        let cut = |s: String| -> String { s.chars().take(60).collect() };
        self.commit(vec![Event::TrialRecorded {
            agent_id,
            case_type: crate::trials::key_for_new(&self.state.trials, &prepared.fields),
            fields: Some(prepared.fields.clone()),
            action: cut(action.word),
            action_text: action.text,
            reason: reason.and_then(crate::trials::normalize_code).map(cut),
            score,
            case_id: case_id.map(|c| c.trim().chars().take(60).collect::<String>()).filter(|c| !c.is_empty()),
        }])?;
        Ok(prepared)
    }

    pub fn resolve_disputes(&mut self, actor: Actor, post_id: &str, reason: &str) -> BoardResult<()> {
        let p = self.post_in_thread(post_id)?;
        if p.disputes.is_empty() && p.contradictions.is_empty() {
            return Ok(());
        }
        self.commit(vec![Event::DisputesResolved {
            thread_id: p.thread_id,
            post_id: post_id.into(),
            actor,
            reason: reason.trim().into(),
        }])?;
        Ok(())
    }

    /// Create or replace the moderator digest of a thread: a short synthesis of what its posts
    /// report. It must cite post IDs of this thread and only those.
    pub fn write_digest(&mut self, actor: Actor, thread_id: &str, text: &str, reason: &str) -> BoardResult<u32> {
        let t = self.visible_thread(thread_id)?;
        let text = text.trim().replace("\r\n", "\n");
        let reason = reason.trim().to_string();
        if reason.len() < 4 {
            return Err(BoardError::invalid("Give a reason."));
        }
        if text.is_empty() {
            return Err(BoardError::invalid("The digest is empty."));
        }
        let n = text.chars().count();
        if n > MAX_DIGEST_CHARS {
            return Err(BoardError::invalid(format!("The digest has {n} characters; the limit is {MAX_DIGEST_CHARS}. Shorten it.")));
        }
        let visible: Vec<&String> =
            t.post_ids.iter().filter(|id| self.state.posts.get(*id).map(|p| p.hidden.is_none()).unwrap_or(false)).collect();
        if visible.len() < MIN_DIGEST_POSTS {
            return Err(BoardError::invalid(format!("{thread_id} has {} visible post(s); a digest needs at least {MIN_DIGEST_POSTS}.", visible.len())));
        }
        let cited = cited_posts(&text);
        if cited.is_empty() {
            return Err(BoardError::invalid("Cite the posts each point comes from, for example (P12, P15)."));
        }
        if let Some(bad) = cited.iter().find(|c| !t.post_ids.contains(c)) {
            return Err(BoardError::invalid(format!("{bad} is not a post in {thread_id}. A digest cites only this thread's posts.")));
        }
        if t.digest.as_ref().is_some_and(|d| d.text == text) {
            return Ok(visible.len() as u32);
        }
        let posts = visible.len() as u32;
        self.commit(vec![Event::DigestWritten { thread_id: thread_id.into(), text, posts, actor, reason }])?;
        Ok(posts)
    }

    /// Change how many threads stay active. When the board holds more, the least recently
    /// active threads move to the archive now. Returns how many were archived.
    pub fn set_capacity(&mut self, n: usize) -> BoardResult<usize> {
        self.capacity = n.clamp(*ACTIVE_THREADS_RANGE.start(), *ACTIVE_THREADS_RANGE.end());
        let events = self.overflow_events(None, None);
        let n = events.len();
        self.commit(events)?;
        Ok(n)
    }

    pub fn pinned_threads(&self) -> Vec<&Thread> {
        let mut v: Vec<&Thread> = self.state.threads.values().filter(|t| t.is_visible() && t.is_pinned()).collect();
        v.sort_by_key(|t| t.pin.as_ref().map(|p| p.at));
        v
    }

    /// Change how many threads can be pinned. When more are pinned, the oldest pins are
    /// removed and those threads go back to the active pool. Returns how many were unpinned.
    pub fn set_pin_capacity(&mut self, n: usize) -> BoardResult<usize> {
        self.pin_capacity = n.clamp(*PINNED_THREADS_RANGE.start(), *PINNED_THREADS_RANGE.end());
        let pinned: Vec<String> = self.pinned_threads().iter().map(|t| t.id.clone()).collect();
        if pinned.len() <= self.pin_capacity {
            return Ok(0);
        }
        let excess = pinned.len() - self.pin_capacity;
        let reason = format!("The board allows {} pinned thread(s).", self.pin_capacity);
        for id in pinned.iter().take(excess) {
            self.pin_thread(Actor::User, id, false, &reason)?;
        }
        Ok(excess)
    }

    /// Pin or unpin a whole thread. A pinned thread stays on the board outside the active
    /// limit and is never archived for capacity. At most `pin_capacity` threads are pinned.
    pub fn pin_thread(&mut self, actor: Actor, thread_id: &str, pinned: bool, reason: &str) -> BoardResult<()> {
        let t = self.visible_thread(thread_id)?.clone();
        if t.is_pinned() == pinned {
            return Ok(());
        }
        let reason = reason.trim().to_string();
        if reason.len() < 4 {
            return Err(BoardError::invalid("Give a reason."));
        }
        let mut events = vec![];
        if pinned {
            let current = self.pinned_threads();
            if current.len() >= self.pin_capacity {
                let list: Vec<String> = current.iter().map(|t| format!("{} “{}”", t.id, t.title)).collect();
                return Err(BoardError::invalid(if self.pin_capacity == 0 {
                    "Thread pins are turned off on this board.".to_string()
                } else {
                    format!(
                        "The board allows {} pinned thread(s) and all are in use: {}. Unpin one first, then pin {thread_id}.",
                        self.pin_capacity,
                        list.join(", ")
                    )
                }));
            }
            if t.is_archived() {
                events.push(Event::ThreadReactivated { thread_id: thread_id.into(), reason: "Pinned.".into() });
            }
            // The thread leaves the active pool, so no other thread needs to move.
            events.push(Event::ThreadPinned { thread_id: thread_id.into(), actor, reason });
        } else {
            events.push(Event::ThreadUnpinned { thread_id: thread_id.into(), actor, reason });
            events.extend(self.overflow_events(None, Some(thread_id)));
        }
        self.commit(events)?;
        Ok(())
    }

    /// The user moves a thread to the archive, or back to the board. Archived threads stay
    /// searchable; a new post makes them active again, as with capacity archiving.
    pub fn set_archived(&mut self, thread_id: &str, archived: bool) -> BoardResult<()> {
        let t = self
            .state
            .threads
            .get(thread_id)
            .cloned()
            .ok_or_else(|| BoardError::not_found(format!("Thread {thread_id} does not exist.")))?;
        if t.is_archived() == archived {
            return Ok(());
        }
        let events = if archived {
            let mut ev = vec![];
            if t.is_pinned() {
                ev.push(Event::ThreadUnpinned { thread_id: thread_id.into(), actor: Actor::User, reason: "Archived by the user.".into() });
            }
            ev.push(Event::ThreadArchived { thread_id: thread_id.into(), reason: "Archived by the user.".into() });
            ev
        } else {
            let mut ev = vec![Event::ThreadReactivated { thread_id: thread_id.into(), reason: "Restored by the user.".into() }];
            ev.extend(self.capacity_events(thread_id));
            ev
        };
        self.commit(events)?;
        Ok(())
    }

    pub fn quarantine(&mut self, actor: Actor, thread_id: &str, quarantined: bool, reason: &str) -> BoardResult<()> {
        let t = self
            .state
            .threads
            .get(thread_id)
            .cloned()
            .ok_or_else(|| BoardError::not_found(format!("Thread {thread_id} does not exist.")))?;
        if t.quarantine.is_some() == quarantined {
            return Ok(());
        }
        if !quarantined && actor != Actor::User {
            return Err(BoardError::invalid("Only the user can release a quarantined thread."));
        }
        let reason = reason.trim().to_string();
        if reason.len() < 4 {
            return Err(BoardError::invalid("Give a reason."));
        }
        let mut events = vec![];
        if quarantined && t.is_pinned() {
            events.push(Event::ThreadUnpinned { thread_id: thread_id.into(), actor: actor.clone(), reason: "Quarantined.".into() });
        }
        events.push(if quarantined {
            Event::ThreadQuarantined { thread_id: thread_id.into(), actor, reason }
        } else {
            Event::ThreadReleased { thread_id: thread_id.into(), actor, reason }
        });
        if !quarantined && !t.is_archived() {
            events.extend(self.capacity_events(thread_id));
        }
        self.commit(events)?;
        Ok(())
    }

    pub fn set_rules(&mut self, actor: Actor, rules: Vec<String>, reason: &str) -> BoardResult<()> {
        let rules: Vec<String> = rules.into_iter().map(|r| clean(&r)).filter(|r| !r.is_empty()).collect();
        if rules.len() > MAX_RULES {
            return Err(BoardError::invalid(format!("Keep at most {MAX_RULES} rules.")));
        }
        if rules.iter().any(|r| r.chars().count() > 240) {
            return Err(BoardError::invalid("Keep each rule at 240 characters or fewer."));
        }
        if rules == self.state.rules {
            return Ok(());
        }
        self.commit(vec![Event::RulesUpdated { rules, actor, reason: reason.trim().into() }])?;
        Ok(())
    }

    pub fn set_access(&mut self, actor: Actor, running: bool, reason: &str) -> BoardResult<()> {
        if running == self.state.access_running() {
            return Ok(());
        }
        if running && actor != Actor::User {
            return Err(BoardError::invalid("Only the user can restart OH MY GOD! access."));
        }
        self.commit(vec![if running {
            Event::AccessStarted { actor }
        } else {
            Event::AccessStopped { actor, reason: reason.trim().into() }
        }])?;
        Ok(())
    }

    pub fn record_run(&mut self, run: WorkerRun) -> BoardResult<()> {
        self.commit(vec![Event::WorkerRunRecorded { run }])?;
        Ok(())
    }

    /// Threads changed after `checkpoint`, oldest change first.
    pub fn changed_threads(&self, checkpoint: u64) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self
            .state
            .last_change
            .iter()
            .filter(|(_, s)| **s > checkpoint)
            .map(|(t, s)| (t.clone(), *s))
            .collect();
        v.sort_by_key(|(_, s)| *s);
        v
    }
}
