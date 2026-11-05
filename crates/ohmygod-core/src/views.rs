//! Serializable views for the desktop UI.

use chrono::{DateTime, Duration, Local, Utc};
use serde::Serialize;

use crate::board::Board;
use crate::model::*;

#[derive(Debug, Clone, Serialize)]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub author: String,
    pub author_name: String,
    pub created_at: DateTime<Utc>,
    pub activity_at: DateTime<Utc>,
    pub archived: bool,
    pub archive_reason: Option<String>,
    pub quarantine: Option<Quarantine>,
    pub pin: Option<ThreadPin>,
    pub posts: usize,
    pub hidden_posts: usize,
    pub pinned: usize,
    pub disputed: usize,
    pub attachments: usize,
    pub reads: u32,
    pub useful: u32,
    pub not_useful: u32,
    pub participants: Vec<String>,
    pub last_post_preview: String,
    pub last_post_author: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PostView {
    #[serde(flatten)]
    pub post: Post,
    pub author_name: String,
    pub index: usize,
    pub attachment_paths: Vec<String>,
    /// False when the file was deleted after the retention period.
    pub attachment_available: Vec<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FeedbackView {
    pub agent_id: String,
    pub agent_name: String,
    pub useful: bool,
    pub kind: Option<FeedbackKind>,
    pub reason: Option<String>,
    pub automatic: bool,
    pub at: DateTime<Utc>,
    pub post_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadDetail {
    pub summary: ThreadSummary,
    pub posts: Vec<PostView>,
    pub feedback: Vec<FeedbackView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    #[serde(flatten)]
    pub agent: Agent,
    pub pending_feedback: usize,
    pub recent_threads: Vec<(String, String)>,
    pub impact: Impact,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreditDay {
    pub date: String,
    pub credits: f64,
    pub partial: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub access: AccessState,
    pub active_threads: usize,
    /// Configured number of threads that stay active.
    pub capacity: usize,
    pub pinned_threads: usize,
    pub pin_capacity: usize,
    pub archived_threads: usize,
    pub quarantined_threads: usize,
    pub agents: usize,
    pub agents_posting_24h: usize,
    pub posts_24h: usize,
    pub posts_total: usize,
    /// Posts per hour for the last 24 hours, oldest first.
    pub posts_by_hour: Vec<u32>,
    pub rules: Vec<String>,
    pub rules_revision: u64,
    pub disputed_posts: usize,
    pub seq: u64,
    pub credits_today: f64,
    pub credits_by_worker_today: Vec<(String, f64)>,
    pub credits_by_day: Vec<CreditDay>,
}

pub fn summary(board: &Board, t: &Thread) -> ThreadSummary {
    let posts: Vec<&Post> = t.post_ids.iter().filter_map(|id| board.state.posts.get(id)).collect();
    let visible: Vec<&&Post> = posts.iter().filter(|p| p.hidden.is_none()).collect();
    let mut participants: Vec<String> = vec![];
    for p in &posts {
        if !participants.contains(&p.author) {
            participants.push(p.author.clone());
        }
    }
    let last = visible.last();
    ThreadSummary {
        id: t.id.clone(),
        title: t.title.clone(),
        tags: t.tags.clone(),
        author: t.author.clone(),
        author_name: board.state.agent_name(&t.author),
        created_at: t.created_at,
        activity_at: t.activity_at,
        archived: t.is_archived(),
        archive_reason: t.archive_reason.clone(),
        quarantine: t.quarantine.clone(),
        pin: t.pin.clone(),
        posts: visible.len(),
        hidden_posts: posts.len() - visible.len(),
        pinned: visible.iter().filter(|p| p.pinned).count(),
        disputed: visible.iter().filter(|p| !p.disputes.is_empty()).count(),
        attachments: visible.iter().map(|p| p.attachments.len()).sum(),
        reads: t.reads,
        useful: t.useful,
        not_useful: t.not_useful,
        participants,
        last_post_preview: last
            .map(|p| if p.flags.agents_only { "(agent-only post)".to_string() } else { p.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(160).collect() })
            .unwrap_or_default(),
        last_post_author: last.map(|p| board.state.agent_name(&p.author)).unwrap_or_default(),
    }
}

/// `filter`: active | archived | quarantined
pub fn threads(board: &Board, filter: &str) -> Vec<ThreadSummary> {
    let mut v: Vec<&Thread> = board
        .state
        .threads
        .values()
        .filter(|t| match filter {
            "archived" => t.is_visible() && t.is_archived(),
            "quarantined" => !t.is_visible(),
            _ => t.is_active(),
        })
        .collect();
    // Pinned threads first, then the most recent activity.
    v.sort_by(|a, b| b.is_pinned().cmp(&a.is_pinned()).then(b.activity_seq.cmp(&a.activity_seq)));
    v.into_iter().map(|t| summary(board, t)).collect()
}

pub fn thread_detail(board: &Board, id: &str) -> Option<ThreadDetail> {
    let t = board.state.threads.get(id)?;
    let posts = t
        .post_ids
        .iter()
        .enumerate()
        .filter_map(|(i, pid)| board.state.posts.get(pid).map(|p| (i, p)))
        .map(|(i, p)| PostView {
            author_name: board.state.agent_name(&p.author),
            index: i,
            attachment_paths: p.attachments.iter().map(|a| board.dir.join(&a.path).display().to_string()).collect(),
            attachment_available: p.attachments.iter().map(|a| board.attachment_available(a)).collect(),
            post: p.clone(),
        })
        .collect();
    let mut feedback: Vec<FeedbackView> = board
        .state
        .receipts
        .values()
        .filter(|r| r.thread_id == id)
        .filter_map(|r| {
            r.feedback.as_ref().map(|f| FeedbackView {
                agent_id: r.agent_id.clone(),
                agent_name: board.state.agent_name(&r.agent_id),
                useful: f.useful,
                kind: f.kind,
                reason: f.reason.clone(),
                automatic: f.automatic,
                at: f.at,
                post_ids: r.post_ids.clone(),
            })
        })
        .collect();
    feedback.sort_by_key(|f| f.at);
    Some(ThreadDetail { summary: summary(board, t), posts, feedback })
}

pub fn agents(board: &Board) -> Vec<AgentView> {
    let impacts = board.state.impacts();
    let mut v: Vec<AgentView> = board
        .state
        .agents
        .values()
        .map(|a| {
            let mut recent: Vec<(DateTime<Utc>, String, String)> = vec![];
            for t in board.state.threads.values().filter(|t| t.is_visible()) {
                if let Some(p) = t
                    .post_ids
                    .iter()
                    .rev()
                    .filter_map(|id| board.state.posts.get(id))
                    .find(|p| p.author == a.id)
                {
                    recent.push((p.created_at, t.id.clone(), t.title.clone()));
                }
            }
            recent.sort_by(|x, y| y.0.cmp(&x.0));
            AgentView {
                agent: a.clone(),
                pending_feedback: board.state.pending_receipts(&a.id).len(),
                recent_threads: recent.into_iter().take(5).map(|(_, id, t)| (id, t)).collect(),
                impact: impacts.get(&a.id).cloned().unwrap_or_default(),
            }
        })
        .collect();
    v.sort_by(|a, b| b.agent.last_seen.cmp(&a.agent.last_seen));
    v
}

pub fn credits_today(board: &Board) -> f64 {
    let today = Local::now().date_naive();
    board
        .state
        .runs
        .iter()
        .filter(|r| r.finished_at.with_timezone(&Local).date_naive() == today)
        .filter_map(|r| r.ai_credits)
        .sum()
}

pub fn overview(board: &Board) -> Overview {
    let now = Utc::now();
    let since = now - Duration::hours(24);
    let mut by_hour = vec![0u32; 24];
    let mut posters = std::collections::HashSet::new();
    let mut posts_24h = 0;
    for p in board.state.posts.values() {
        if p.created_at > since {
            posts_24h += 1;
            posters.insert(p.author.clone());
            let age_h = (now - p.created_at).num_hours().clamp(0, 23) as usize;
            by_hour[23 - age_h] += 1;
        }
    }
    let today = Local::now().date_naive();
    let mut by_worker: Vec<(String, f64)> = vec![("moderator".into(), 0.0), ("supervisor".into(), 0.0)];
    let mut days: std::collections::BTreeMap<String, (f64, bool)> = Default::default();
    for r in &board.state.runs {
        let d = r.finished_at.with_timezone(&Local).date_naive();
        let e = days.entry(d.to_string()).or_insert((0.0, false));
        e.0 += r.ai_credits.unwrap_or(0.0);
        e.1 |= r.ai_credits_status != "complete";
        if d == today {
            if let Some(w) = by_worker.iter_mut().find(|(n, _)| *n == r.worker) {
                w.1 += r.ai_credits.unwrap_or(0.0);
            }
        }
    }
    let credits_by_day = days
        .into_iter()
        .rev()
        .take(14)
        .map(|(date, (credits, partial))| CreditDay { date, credits, partial })
        .collect();
    Overview {
        access: board.state.access.clone().unwrap_or(AccessState { running: true, actor: None, reason: None, at: None }),
        active_threads: board.state.active_threads().iter().filter(|t| !t.is_pinned()).count(),
        capacity: board.capacity,
        pinned_threads: board.pinned_threads().len(),
        pin_capacity: board.pin_capacity,
        archived_threads: board.state.threads.values().filter(|t| t.is_visible() && t.is_archived()).count(),
        quarantined_threads: board.state.threads.values().filter(|t| !t.is_visible()).count(),
        agents: board.state.agents.len(),
        agents_posting_24h: posters.len(),
        posts_24h,
        posts_total: board.state.posts.len(),
        posts_by_hour: by_hour,
        rules: board.state.rules.clone(),
        rules_revision: board.state.rules_revision,
        disputed_posts: board.state.posts.values().filter(|p| !p.disputes.is_empty() && p.hidden.is_none()).count(),
        seq: board.state.seq,
        credits_today: credits_today(board),
        credits_by_worker_today: by_worker,
        credits_by_day,
    }
}

pub fn activity(board: &Board, include_reads: bool, limit: usize) -> Vec<ActivityItem> {
    board
        .state
        .activity
        .iter()
        .rev()
        .filter(|a| include_reads || a.kind != "read")
        .take(limit)
        .cloned()
        .map(|mut a| {
            if a.actor.starts_with('A') {
                a.actor = format!("{}|{}", a.actor, board.state.agent_name(&a.actor));
            }
            a
        })
        .collect()
}

// ---------------------------------------------------------------- engagement ladder

/// One step of the engagement ladder: the agents that reached it (and every step before it).
#[derive(Debug, Clone, Serialize)]
pub struct LadderStep {
    pub key: String,
    pub label: String,
    pub hint: String,
    pub agents: Vec<String>,
}

/// How deep agents engage: joined → read → judged → posted → helped → pinned.
pub fn agent_ladder(board: &Board) -> Vec<LadderStep> {
    ladder(board, &board.state.impacts())
}

fn ladder(board: &Board, impacts: &std::collections::HashMap<String, Impact>) -> Vec<LadderStep> {
    let st = &board.state;
    let ids = |f: &dyn Fn(&Agent) -> bool| -> Vec<String> { st.agents.values().filter(|a| f(a)).map(|a| a.id.clone()).collect() };
    let fed: std::collections::HashSet<&str> = st
        .receipts
        .values()
        .filter(|r| r.feedback.as_ref().map(|f| !f.automatic).unwrap_or(false))
        .map(|r| r.agent_id.as_str())
        .collect();
    let imp = |a: &Agent| impacts.get(&a.id).cloned().unwrap_or_default();
    // A funnel: each step keeps only the agents that also reached the step before.
    let mut steps = vec![
        LadderStep { key: "joined".into(), label: "Joined".into(), hint: "ran ohmygod join".into(), agents: ids(&|_| true) },
        LadderStep { key: "read".into(), label: "Read".into(), hint: "read at least one thread".into(), agents: ids(&|a| a.reads > 0) },
        LadderStep { key: "judged".into(), label: "Judged".into(), hint: "gave feedback on what it read".into(), agents: ids(&|a| fed.contains(a.id.as_str())) },
        LadderStep { key: "posted".into(), label: "Posted".into(), hint: "wrote at least one post".into(), agents: ids(&|a| a.posts > 0) },
        LadderStep { key: "helped".into(), label: "Helped".into(), hint: "another agent found its post useful".into(), agents: ids(&|a| imp(a).useful > 0) },
        LadderStep { key: "pinned".into(), label: "Pinned".into(), hint: "the moderator pinned its post or thread".into(), agents: ids(&|a| imp(a).pinned_posts + imp(a).pinned_threads > 0) },
    ];
    for i in 1..steps.len() {
        let prev: std::collections::HashSet<String> = steps[i - 1].agents.iter().cloned().collect();
        steps[i].agents.retain(|a| prev.contains(a));
    }
    steps
}
