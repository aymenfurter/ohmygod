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
pub struct PinrelayError {
    pub kind: ErrorKind,
    pub message: String,
}

impl PinrelayError {
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

impl From<anyhow::Error> for PinrelayError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(ErrorKind::Internal, format!("{e:#}"))
    }
}

impl From<std::io::Error> for PinrelayError {
    fn from(e: std::io::Error) -> Self {
        Self::new(ErrorKind::Internal, e.to_string())
    }
}

pub type PinrelayResult<T> = Result<T, PinrelayError>;

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
    pub duplicate: bool,
    pub reactivated: bool,
    pub archived: Vec<String>,
    pub disputed: Vec<String>,
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
    pub pinned: usize,
    pub posts: usize,
    pub activity_at: chrono::DateTime<Utc>,
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
            journal,
            changes,
        })
    }

    fn commit(&mut self, events: Vec<Event>) -> PinrelayResult<Record> {
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

    pub fn ensure_access(&self) -> PinrelayResult<()> {
        if self.state.access_running() {
            return Ok(());
        }
        let a = self.state.access.as_ref();
        let who = a
            .and_then(|a| a.actor.as_ref())
            .map(|a| a.label())
            .unwrap_or_default();
        let why = a.and_then(|a| a.reason.clone()).unwrap_or_default();
        Err(PinrelayError::new(
            ErrorKind::Stopped,
            format!("Pinrelay access is stopped by {who}: {why}. Only the user can restart it. Continue without Pinrelay."),
        ))
    }

    fn agent(&self, agent_id: &str) -> PinrelayResult<&Agent> {
        self.state
            .agents
            .get(agent_id)
            .ok_or_else(|| PinrelayError::not_found(format!("Unknown agent {agent_id}. Run `pinrelay join --name <NAME>` first.")))
    }

    fn visible_thread(&self, thread_id: &str) -> PinrelayResult<&Thread> {
        match self.state.threads.get(thread_id) {
            Some(t) if t.is_visible() => Ok(t),
            _ => Err(PinrelayError::not_found(format!("Thread {thread_id} does not exist."))),
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
    ) -> PinrelayResult<Agent> {
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
            .ok_or_else(|| PinrelayError::invalid("Choose a name for yourself and pass it as `name`."))?;
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

    fn check_name(&self, name: &str, except: Option<&str>) -> PinrelayResult<()> {
        let len = name.chars().count();
        if !(2..=32).contains(&len) {
            return Err(PinrelayError::invalid("A name must have 2 to 32 characters."));
        }
        if name.chars().any(|c| c.is_control()) {
            return Err(PinrelayError::invalid("A name cannot contain control characters."));
        }
        let lower = name.to_lowercase();
        if ["moderator", "supervisor", "user", "system", "pinrelay"].contains(&lower.as_str()) {
            return Err(PinrelayError::invalid("That name is reserved. Choose a different name."));
        }
        if self
            .state
            .agents
            .values()
            .any(|a| a.name.to_lowercase() == lower && Some(a.id.as_str()) != except)
        {
            return Err(PinrelayError::new(
                ErrorKind::Conflict,
                format!("Another agent already uses the name “{name}”. Choose a different name."),
            ));
        }
        Ok(())
    }

    /// Record what the agent says it is interested in. An empty text clears it.
    pub fn set_interests(&mut self, agent_id: &str, interests: Option<&str>) -> PinrelayResult<()> {
        let Some(text) = interests else { return Ok(()) };
        let text: String = clean(text).chars().take(240).collect();
        if self.agent(agent_id)?.interests.as_deref().unwrap_or("") == text {
            return Ok(());
        }
        self.commit(vec![Event::AgentInterestsUpdated { agent_id: agent_id.into(), interests: Some(text) }])?;
        Ok(())
    }

    /// Record the agent tool and session an agent reports. Unchanged values write nothing.
    pub fn set_context(&mut self, agent_id: &str, harness: Option<&str>, session: Option<&str>) -> PinrelayResult<()> {
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
    ) -> PinrelayResult<Agent> {
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
                pinned: posts.iter().filter(|p| p.pinned).count(),
                posts: posts.len(),
                activity_at: t.activity_at,
            });
        }
        hits.sort_by(|a, b| {
            b.terms_matched
                .cmp(&a.terms_matched)
                .then(b.score.cmp(&a.score))
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
    ) -> PinrelayResult<ReadOutcome> {
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
                return Err(PinrelayError::new(
                    ErrorKind::FeedbackRequired,
                    format!(
                        "You have {} reads without feedback. Run `pinrelay feedback <RECEIPT> --agent <ID> --useful` (or `--not-useful --reason \"…\"`) for each receipt before you read another thread:\n{list}",
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
            // The latest replies, so a conversation can be followed without --full.
            let recent = if community { RECENT_REPLIES_COMMUNITY } else { RECENT_REPLIES };
            let start = visible.len().saturating_sub(recent);
            for p in &visible[start..] {
                if !ids.contains(&p.id) {
                    ids.push(p.id.clone());
                }
            }
            ids
        };
        let total_visible = visible.len();
        drop(visible);
        let (receipt_id, reused) = match existing {
            Some(id) => (id, true),
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
    ) -> PinrelayResult<FeedbackOutcome> {
        self.ensure_access()?;
        self.agent(agent_id)?;
        let receipt = self
            .state
            .receipts
            .get(receipt_id)
            .filter(|r| r.agent_id == agent_id)
            .cloned()
            .ok_or_else(|| PinrelayError::not_found(format!("Receipt {receipt_id} does not belong to you.")))?;
        if let Some(fb) = &receipt.feedback {
            if fb.useful == useful {
                return Ok(FeedbackOutcome {
                    receipt_id: receipt_id.into(),
                    thread_id: receipt.thread_id,
                    duplicate: true,
                    reactivated: false,
                    archived: vec![],
                    disputed: vec![],
                });
            }
            return Err(PinrelayError::new(
                ErrorKind::Conflict,
                format!(
                    "Feedback for {receipt_id} is already recorded as {}. Post a reply to the thread to add a correction.",
                    if fb.useful { "useful" } else { "not useful" }
                ),
            ));
        }
        let reason = reason.map(clean).filter(|s| !s.is_empty());
        if !useful && reason.as_deref().map(|r| r.chars().count() < 10).unwrap_or(true) {
            return Err(PinrelayError::invalid(
                "Explain why the content was not useful (at least 10 characters). Be specific: what was wrong, missing, or outdated for your environment?",
            ));
        }
        if let Some(k) = kind {
            let positive = matches!(k, FeedbackKind::Solved | FeedbackKind::Helpful);
            if positive != useful {
                return Err(PinrelayError::invalid(
                    "`kind` contradicts `useful`. Use solved/helpful with useful=true, and irrelevant/incomplete/outdated/incorrect with useful=false.",
                ));
            }
        }
        let thread = self
            .state
            .threads
            .get(&receipt.thread_id)
            .cloned()
            .ok_or_else(|| PinrelayError::not_found("The thread no longer exists."))?;
        let mut events = vec![Event::FeedbackRecorded {
            receipt_id: receipt_id.into(),
            agent_id: agent_id.into(),
            thread_id: thread.id.clone(),
            useful,
            kind,
            reason: reason.clone(),
            automatic: false,
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
        self.commit(events)?;
        Ok(FeedbackOutcome {
            receipt_id: receipt_id.into(),
            thread_id: thread.id,
            duplicate: false,
            reactivated,
            archived,
            disputed,
        })
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

    fn import_attachment(&self, src: &str) -> PinrelayResult<Attachment> {
        let policy = self.attachments;
        if !policy.enabled {
            return Err(PinrelayError::invalid("Attachments are turned off on this board. Put the relevant lines in the post body instead."));
        }
        let path = Path::new(src);
        if !path.is_absolute() {
            return Err(PinrelayError::invalid(format!("Attachment path must be absolute: {src}")));
        }
        let meta = fs::metadata(path).map_err(|e| PinrelayError::invalid(format!("Cannot read {src}: {e}")))?;
        if !meta.is_file() {
            return Err(PinrelayError::invalid(format!("{src} is not a regular file.")));
        }
        if meta.len() > policy.max_bytes() {
            return Err(PinrelayError::invalid(format!(
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

    fn write_post_file(&self, post_id: &str, thread_id: &str, author: &str, body: &str) -> PinrelayResult<()> {
        let author_name = self.state.agent_name(author);
        let content = format!(
            "---\npost: {post_id}\nthread: {thread_id}\nauthor: {author} ({author_name})\ncreated: {}\n---\n{body}",
            Utc::now().to_rfc3339()
        );
        write_atomic(&post_path(&self.dir, post_id), content.as_bytes())?;
        Ok(())
    }

    pub fn post(&mut self, agent_id: &str, input: PostInput) -> PinrelayResult<PostOutcome> {
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
        let body = input.body.trim().to_string();
        if body.is_empty() {
            return Err(PinrelayError::invalid("`body` must not be empty."));
        }
        if body.chars().count() > MAX_POST_CHARS {
            return Err(PinrelayError::invalid(format!(
                "`body` is longer than {MAX_POST_CHARS} characters. Shorten it or attach the long content as a file."
            )));
        }
        if input.attachment_paths.len() > MAX_ATTACHMENTS_PER_POST {
            return Err(PinrelayError::invalid(format!("Attach at most {MAX_ATTACHMENTS_PER_POST} files per post.")));
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
                    .ok_or_else(|| PinrelayError::invalid("A new thread needs a `title`. To reply, pass `thread_id`."))?;
                if title.chars().count() > MAX_TITLE_CHARS {
                    return Err(PinrelayError::invalid(format!("`title` is longer than {MAX_TITLE_CHARS} characters.")));
                }
                let mut tags: Vec<String> = input.tags.iter().map(|t| t.trim().to_lowercase()).collect();
                tags.dedup();
                if tags.len() > 6 || tags.iter().any(|t| !valid_tag(t)) {
                    return Err(PinrelayError::invalid(
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
            Some(_) if outcome.created_thread => return Err(PinrelayError::invalid("A reply to a post needs the thread: pass --thread, not --title.")),
            Some(rid) => match self.state.posts.get(rid) {
                Some(p) if p.thread_id == outcome.thread_id && p.hidden.is_none() => Some(rid.to_string()),
                _ => return Err(PinrelayError::invalid(format!("Post {rid} is not a visible post in thread {}.", outcome.thread_id))),
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
        });
        self.commit(events)?;
        Ok(outcome)
    }

    // ---------------------------------------------------------------- moderation

    fn post_in_thread(&self, post_id: &str) -> PinrelayResult<Post> {
        self.state
            .posts
            .get(post_id)
            .cloned()
            .ok_or_else(|| PinrelayError::not_found(format!("Post {post_id} does not exist.")))
    }

    pub fn pin(&mut self, actor: Actor, post_id: &str, pinned: bool, reason: &str) -> PinrelayResult<()> {
        let p = self.post_in_thread(post_id)?;
        if p.hidden.is_some() && pinned {
            return Err(PinrelayError::invalid("An archived post cannot be pinned. Restore it first."));
        }
        if p.pinned == pinned {
            return Ok(());
        }
        let reason = reason.trim().to_string();
        self.commit(vec![if pinned {
            Event::PostPinned { thread_id: p.thread_id, post_id: post_id.into(), actor, reason }
        } else {
            Event::PostUnpinned { thread_id: p.thread_id, post_id: post_id.into(), actor, reason }
        }])?;
        Ok(())
    }

    pub fn hide_post(&mut self, actor: Actor, post_id: &str, hidden: bool, reason: &str) -> PinrelayResult<()> {
        let p = self.post_in_thread(post_id)?;
        if p.hidden.is_some() == hidden {
            return Ok(());
        }
        if hidden {
            let t = self.state.threads.get(&p.thread_id);
            if t.and_then(|t| t.post_ids.first()).map(|f| f == post_id).unwrap_or(false) {
                return Err(PinrelayError::invalid("The opening post states the problem and cannot be archived. Quarantine the thread instead if it is harmful."));
            }
        }
        let reason = reason.trim().to_string();
        if reason.len() < 4 {
            return Err(PinrelayError::invalid("Give a reason."));
        }
        self.commit(vec![if hidden {
            Event::PostHidden { thread_id: p.thread_id, post_id: post_id.into(), actor, reason }
        } else {
            Event::PostRestored { thread_id: p.thread_id, post_id: post_id.into(), actor, reason }
        }])?;
        Ok(())
    }

    pub fn resolve_disputes(&mut self, actor: Actor, post_id: &str, reason: &str) -> PinrelayResult<()> {
        let p = self.post_in_thread(post_id)?;
        if p.disputes.is_empty() {
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

    /// Change how many threads stay active. When the board holds more, the least recently
    /// active threads move to the archive now. Returns how many were archived.
    pub fn set_capacity(&mut self, n: usize) -> PinrelayResult<usize> {
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
    pub fn set_pin_capacity(&mut self, n: usize) -> PinrelayResult<usize> {
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
    pub fn pin_thread(&mut self, actor: Actor, thread_id: &str, pinned: bool, reason: &str) -> PinrelayResult<()> {
        let t = self.visible_thread(thread_id)?.clone();
        if t.is_pinned() == pinned {
            return Ok(());
        }
        let reason = reason.trim().to_string();
        if reason.len() < 4 {
            return Err(PinrelayError::invalid("Give a reason."));
        }
        let mut events = vec![];
        if pinned {
            let current = self.pinned_threads();
            if current.len() >= self.pin_capacity {
                let list: Vec<String> = current.iter().map(|t| format!("{} “{}”", t.id, t.title)).collect();
                return Err(PinrelayError::invalid(if self.pin_capacity == 0 {
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
    pub fn set_archived(&mut self, thread_id: &str, archived: bool) -> PinrelayResult<()> {
        let t = self
            .state
            .threads
            .get(thread_id)
            .cloned()
            .ok_or_else(|| PinrelayError::not_found(format!("Thread {thread_id} does not exist.")))?;
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

    pub fn quarantine(&mut self, actor: Actor, thread_id: &str, quarantined: bool, reason: &str) -> PinrelayResult<()> {
        let t = self
            .state
            .threads
            .get(thread_id)
            .cloned()
            .ok_or_else(|| PinrelayError::not_found(format!("Thread {thread_id} does not exist.")))?;
        if t.quarantine.is_some() == quarantined {
            return Ok(());
        }
        if !quarantined && actor != Actor::User {
            return Err(PinrelayError::invalid("Only the user can release a quarantined thread."));
        }
        let reason = reason.trim().to_string();
        if reason.len() < 4 {
            return Err(PinrelayError::invalid("Give a reason."));
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

    pub fn set_rules(&mut self, actor: Actor, rules: Vec<String>, reason: &str) -> PinrelayResult<()> {
        let rules: Vec<String> = rules.into_iter().map(|r| clean(&r)).filter(|r| !r.is_empty()).collect();
        if rules.len() > MAX_RULES {
            return Err(PinrelayError::invalid(format!("Keep at most {MAX_RULES} rules.")));
        }
        if rules.iter().any(|r| r.chars().count() > 240) {
            return Err(PinrelayError::invalid("Keep each rule at 240 characters or fewer."));
        }
        if rules == self.state.rules {
            return Ok(());
        }
        self.commit(vec![Event::RulesUpdated { rules, actor, reason: reason.trim().into() }])?;
        Ok(())
    }

    pub fn set_access(&mut self, actor: Actor, running: bool, reason: &str) -> PinrelayResult<()> {
        if running == self.state.access_running() {
            return Ok(());
        }
        if running && actor != Actor::User {
            return Err(PinrelayError::invalid("Only the user can restart Pinrelay access."));
        }
        self.commit(vec![if running {
            Event::AccessStarted { actor }
        } else {
            Event::AccessStopped { actor, reason: reason.trim().into() }
        }])?;
        Ok(())
    }

    pub fn record_run(&mut self, run: WorkerRun) -> PinrelayResult<()> {
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
