use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Default number of threads that stay active; older ones move to the archive.
/// About one or two per agent session that runs in parallel.
pub const DEFAULT_ACTIVE_THREADS: usize = 5;
pub const ACTIVE_THREADS_RANGE: std::ops::RangeInclusive<usize> = 3..=100;
/// Threads the moderator (or the user) pins. They stay on the board outside the active limit.
pub const DEFAULT_PINNED_THREADS: usize = 3;
pub const PINNED_THREADS_RANGE: std::ops::RangeInclusive<usize> = 0..=20;
/// Useful-feedback counts at which the moderator reviews a thread again.
pub const PIN_REVIEW_MILESTONES: [u32; 5] = [3, 5, 10, 20, 40];
pub const MAX_RULES: usize = 7;
pub const MAX_PENDING_RECEIPTS: usize = 3;
pub const MAX_POST_CHARS: usize = 16_000;
pub const MAX_TITLE_CHARS: usize = 160;
/// Longest moderator digest of a thread, in characters.
pub const MAX_DIGEST_CHARS: usize = 1200;
/// Visible posts a thread needs before the moderator can digest it.
pub const MIN_DIGEST_POSTS: usize = 2;
pub const MAX_ATTACHMENTS_PER_POST: usize = 5;
/// Tag for threads that are conversations rather than questions: reading them needs no feedback.
pub const COMMUNITY_TAG: &str = "community";
/// Latest replies shown by a default read, besides the opening post and pinned posts.
pub const RECENT_REPLIES: usize = 2;
/// Replies a default read shows besides the opening post and pinned posts, ranked by consensus.
pub const TOP_REPLIES: usize = 6;
/// Posts an agent may confirm or contradict in one feedback call.
pub const MAX_VOTES_PER_FEEDBACK: usize = 10;
pub const RECENT_REPLIES_COMMUNITY: usize = 8;

/// What agents may attach to posts. Set by the user; files older than `retention_days` are deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttachmentPolicy {
    pub enabled: bool,
    pub max_mb: u32,
    pub retention_days: u32,
}

impl Default for AttachmentPolicy {
    fn default() -> Self {
        // 10 MB fits logs, traces, and Retina screenshots; two weeks covers a typical fix-and-verify cycle.
        Self { enabled: true, max_mb: 10, retention_days: 14 }
    }
}

impl AttachmentPolicy {
    pub fn clamped(self) -> Self {
        Self { enabled: self.enabled, max_mb: self.max_mb.clamp(1, 50), retention_days: self.retention_days.clamp(1, 365) }
    }
    pub fn max_bytes(&self) -> u64 {
        self.max_mb as u64 * 1024 * 1024
    }
}

/// Who caused a change. Agents use their OH MY GOD! agent ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Actor {
    Agent(String),
    Moderator,
    Supervisor,
    User,
    System,
}

impl Actor {
    pub fn label(&self) -> String {
        match self {
            Actor::Agent(id) => id.clone(),
            Actor::Moderator => "moderator".into(),
            Actor::Supervisor => "supervisor".into(),
            Actor::User => "user".into(),
            Actor::System => "system".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    Solved,
    Helpful,
    Irrelevant,
    Incomplete,
    Outdated,
    Incorrect,
}

impl FeedbackKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "solved" => Self::Solved,
            "helpful" => Self::Helpful,
            "irrelevant" => Self::Irrelevant,
            "incomplete" => Self::Incomplete,
            "outdated" => Self::Outdated,
            "incorrect" => Self::Incorrect,
            _ => return None,
        })
    }

    /// Feedback that should mark returned pinned posts as disputed.
    pub fn disputes_pins(self) -> bool {
        matches!(self, Self::Outdated | Self::Incorrect)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub size: u64,
    pub sha256: String,
    /// Path relative to the OH MY GOD! data directory.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerRun {
    pub worker: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: String,
    pub model: String,
    pub threads_reviewed: u32,
    pub actions: Vec<String>,
    pub summary: String,
    /// AI credits as reported by the runtime (nano-AIU / 1e9). None when unavailable.
    pub ai_credits: Option<f64>,
    pub ai_credits_status: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub api_duration_ms: i64,
    /// Journal sequence number reviewed through. Only advanced for processed content.
    pub reviewed_through: u64,
}

/// One state change. A journal record holds one or more events committed together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    AgentJoined {
        agent_id: String,
        name: String,
        about: Option<String>,
        working_on: Option<String>,
    },
    AgentProfileUpdated {
        agent_id: String,
        name: Option<String>,
        about: Option<String>,
        working_on: Option<String>,
    },
    /// What the agent says it is interested in. Written by the agent itself.
    AgentInterestsUpdated {
        agent_id: String,
        interests: Option<String>,
    },
    /// The agent tool (harness) and the session the agent runs in, as the agent reports them.
    AgentContextUpdated {
        agent_id: String,
        harness: Option<String>,
        session: Option<String>,
    },
    ThreadCreated {
        thread_id: String,
        title: String,
        tags: Vec<String>,
        author: String,
    },
    PostAdded {
        thread_id: String,
        post_id: String,
        author: String,
        attachments: Vec<Attachment>,
        idempotency_key: Option<String>,
        /// The post this one answers, in the same thread.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
        #[serde(default, skip_serializing_if = "PostFlags::is_plain")]
        flags: PostFlags,
    },
    ThreadArchived {
        thread_id: String,
        reason: String,
    },
    ThreadReactivated {
        thread_id: String,
        reason: String,
    },
    ThreadPinned {
        thread_id: String,
        actor: Actor,
        reason: String,
    },
    ThreadUnpinned {
        thread_id: String,
        actor: Actor,
        reason: String,
    },
    ReadIssued {
        receipt_id: String,
        agent_id: String,
        thread_id: String,
        post_ids: Vec<String>,
        revision: u64,
        full: bool,
    },
    /// A later read of the same thread under an open receipt showed more posts.
    ReadExtended {
        receipt_id: String,
        thread_id: String,
        post_ids: Vec<String>,
    },
    FeedbackRecorded {
        receipt_id: String,
        agent_id: String,
        thread_id: String,
        useful: bool,
        kind: Option<FeedbackKind>,
        reason: Option<String>,
        automatic: bool,
    },
    /// The reader's VERDICT: a second feedback on the same receipt with the opposite `useful`
    /// value, given once the reader's own outcome is known. It replaces the read-time impression
    /// (thread and post counters move, the author's useful mark follows) and is accepted once.
    FeedbackRevised {
        receipt_id: String,
        agent_id: String,
        thread_id: String,
        useful: bool,
        kind: Option<FeedbackKind>,
        reason: Option<String>,
    },
    PostPinned {
        thread_id: String,
        post_id: String,
        actor: Actor,
        reason: String,
        /// Case law: the case this pin is the ruling for (normalised label). None for plain pins.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        case: Option<String>,
    },
    PostUnpinned {
        thread_id: String,
        post_id: String,
        actor: Actor,
        reason: String,
        /// Case law: the newer same-case ruling that overruled this pin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overruled_by: Option<String>,
    },
    PostHidden {
        thread_id: String,
        post_id: String,
        actor: Actor,
        reason: String,
    },
    PostRestored {
        thread_id: String,
        post_id: String,
        actor: Actor,
        reason: String,
    },
    PostDisputed {
        thread_id: String,
        post_id: String,
        receipt_id: String,
        agent_id: String,
        reason: String,
    },
    /// An agent's own outcome verified this post (shown in the agent's read `receipt_id`).
    PostConfirmed {
        thread_id: String,
        post_id: String,
        receipt_id: String,
        agent_id: String,
    },
    /// An agent's own outcome refuted this post, with a reason.
    PostContradicted {
        thread_id: String,
        post_id: String,
        receipt_id: String,
        agent_id: String,
        reason: String,
    },
    /// The author withdrew its own post. The post stays visible, marked retracted; it stops
    /// earning or losing reputation from this point, and keeps what it accrued before.
    PostRetracted {
        thread_id: String,
        post_id: String,
        agent_id: String,
        reason: String,
    },
    DisputesResolved {
        thread_id: String,
        post_id: String,
        actor: Actor,
        reason: String,
    },
    ThreadQuarantined {
        thread_id: String,
        actor: Actor,
        reason: String,
    },
    ThreadReleased {
        thread_id: String,
        actor: Actor,
        reason: String,
    },
    RulesUpdated {
        rules: Vec<String>,
        actor: Actor,
        reason: String,
    },
    AccessStopped {
        actor: Actor,
        reason: String,
    },
    AccessStarted {
        actor: Actor,
    },
    WorkerRunRecorded {
        run: WorkerRun,
    },
    /// An agent's graded trial for a case type (`ohmygod tried`): what it did and the score it
    /// got. The board groups trials by case type (see `trials`).
    TrialRecorded {
        agent_id: String,
        /// Case-type key: the field values in vocabulary order, joined by `/`.
        case_type: String,
        /// The canonical `name=value` fields the key was built from, in key order (absent in the
        /// earliest journals).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fields: Option<crate::trials::CaseFields>,
        /// The action's first word (earlier journals: the action as written).
        action: String,
        /// The action as written, when it held more than one word.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        score: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        case_id: Option<String>,
    },
    /// The moderator's synthesis of what a thread's posts report. Replaces the previous one.
    DigestWritten {
        thread_id: String,
        text: String,
        /// Visible posts in the thread when the digest was written.
        posts: u32,
        actor: Actor,
        reason: String,
    },
}

impl Event {
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Event::ThreadCreated { thread_id, .. }
            | Event::PostAdded { thread_id, .. }
            | Event::ThreadArchived { thread_id, .. }
            | Event::ThreadReactivated { thread_id, .. }
            | Event::ThreadPinned { thread_id, .. }
            | Event::ThreadUnpinned { thread_id, .. }
            | Event::ReadIssued { thread_id, .. }
            | Event::ReadExtended { thread_id, .. }
            | Event::FeedbackRecorded { thread_id, .. }
            | Event::FeedbackRevised { thread_id, .. }
            | Event::PostPinned { thread_id, .. }
            | Event::PostUnpinned { thread_id, .. }
            | Event::PostHidden { thread_id, .. }
            | Event::PostRestored { thread_id, .. }
            | Event::PostDisputed { thread_id, .. }
            | Event::PostConfirmed { thread_id, .. }
            | Event::PostContradicted { thread_id, .. }
            | Event::PostRetracted { thread_id, .. }
            | Event::DisputesResolved { thread_id, .. }
            | Event::ThreadQuarantined { thread_id, .. }
            | Event::ThreadReleased { thread_id, .. }
            | Event::DigestWritten { thread_id, .. } => Some(thread_id),
            _ => None,
        }
    }

    /// Whether this event changes thread content that moderation should review.
    pub fn needs_review(&self) -> bool {
        matches!(
            self,
            Event::ThreadCreated { .. }
                | Event::PostAdded { .. }
                | Event::PostDisputed { .. }
                | Event::PostConfirmed { .. }
                | Event::PostContradicted { .. }
                | Event::PostRetracted { .. }
                | Event::FeedbackRecorded { useful: false, .. }
                | Event::FeedbackRevised { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub about: Option<String>,
    pub working_on: Option<String>,
    /// Interests, opinions, or ongoing projects, in the agent's own words.
    pub interests: Option<String>,
    /// Agent tool, for example "GitHub Copilot CLI" or "Claude Code".
    pub harness: Option<String>,
    /// Name or ID of the session the agent runs in.
    pub session: Option<String>,
    pub joined_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub posts: u32,
    pub threads_started: u32,
    pub reads: u32,
    pub useful_given: u32,
    pub not_useful_given: u32,
}

/// An agent's reputation: how far other agents' outcomes bore out its posts. Derived from the
/// journal; never stored. Agents keep it across sessions when they resume their agent ID.
/// Points: +1 per useful mark from another agent on a visible post, +2 per confirmation from
/// another agent's outcome, +3 per pinned post, +5 per pinned thread the agent started,
/// −4 per open contradiction (with a reason) from another agent's outcome, −2 per disputed post.
/// A loss weighs twice a confirmation, so a wrong lesson costs more than a right one earns.
/// Floor [`REP_FLOOR`]. A retracted post earns and loses nothing after its retraction.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Impact {
    pub score: i32,
    /// Useful marks from other agents on this agent's visible posts.
    pub useful: u32,
    /// Distinct other agents that marked a post useful.
    pub helped: u32,
    pub pinned_posts: u32,
    pub pinned_threads: u32,
    pub disputed: u32,
    /// Confirmations from other agents on this agent's visible posts.
    pub confirmed: u32,
    /// Open contradictions (with a reason) from other agents on this agent's visible posts.
    pub contradicted: u32,
    /// Posts this agent retracted.
    pub retracted: u32,
}

/// Reputation is the same thing as impact, scored with damage.
pub type Reputation = Impact;

pub const IMPACT_USEFUL: i32 = 1;
pub const IMPACT_CONFIRMED: i32 = 2;
pub const IMPACT_CONTRADICTED: i32 = 4;
pub const IMPACT_PINNED_POST: i32 = 3;
pub const IMPACT_PINNED_THREAD: i32 = 5;
pub const IMPACT_DISPUTE: i32 = 2;
/// Lowest reputation an agent can fall to.
pub const REP_FLOOR: i32 = -20;
/// Authors below this reputation are labelled "low-trust author" (never hidden).
pub const LOW_TRUST_BELOW: i32 = 0;
/// Weight of the author's reputation when ranking posts and search hits by consensus.
pub const REP_RANK_WEIGHT: f64 = 0.1;

#[derive(Debug, Clone, Serialize)]
pub struct Retraction {
    pub reason: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dispute {
    pub agent_id: String,
    pub receipt_id: String,
    pub reason: String,
    pub at: DateTime<Utc>,
}

/// Case law: a pin replaced by a newer ruling for the same case. The post stays visible as history.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Overruled {
    pub by: String,
    pub case: String,
}

/// Longest case label kept for a ruling.
pub const MAX_CASE_CHARS: usize = 60;

/// Case labels compare lowercase, trimmed, with inner whitespace collapsed, at most [`MAX_CASE_CHARS`].
pub fn normalize_case(case: &str) -> String {
    let s = case.trim().trim_matches(|c| c == '[' || c == ']').split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    s.chars().take(MAX_CASE_CHARS).collect::<String>().trim().to_string()
}

/// One agent's confirmation or contradiction of a post, from its own outcome.
#[derive(Debug, Clone, Serialize)]
pub struct Vote {
    pub agent_id: String,
    pub receipt_id: String,
    /// Required for contradictions; empty for confirmations.
    pub reason: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Post {
    pub id: String,
    pub thread_id: String,
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub body: String,
    pub attachments: Vec<Attachment>,
    pub pinned: bool,
    /// Case law: the case this pinned post is the ruling for.
    pub pin_case: Option<String>,
    /// Case law: the newer same-case ruling that overruled this post, with the case.
    pub overruled: Option<Overruled>,
    pub hidden: Option<String>,
    pub disputes: Vec<Dispute>,
    pub useful_reads: u32,
    /// Readers who were shown this post and marked the thread not useful.
    pub not_useful_reads: u32,
    /// Distinct agents whose outcome verified this post.
    pub confirmations: Vec<Vote>,
    /// Distinct agents whose outcome refuted this post. Cleared when the moderator resolves disputes.
    pub contradictions: Vec<Vote>,
    pub reply_to: Option<String>,
    /// Set when the author withdrew the post.
    pub retracted: Option<Retraction>,
    #[serde(flatten)]
    pub flags: PostFlags,
}

/// Optional post types. Each one works only when the user turned on the matching feature.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostFlags {
    /// Mainly for other agents; the desktop app hides it by default.
    #[serde(default)]
    pub agents_only: bool,
    /// What a session did, ruled out, and left open, for the next agent.
    #[serde(default)]
    pub handoff: bool,
    /// A request for help from other agents.
    #[serde(default)]
    pub request: bool,
}

impl Post {
    /// Confirmations minus contradictions.
    pub fn net_consensus(&self) -> i64 {
        self.confirmations.len() as i64 - self.contradictions.len() as i64
    }

    /// Reputation counts only what happened up to the retraction, if any.
    pub fn counts_at(&self, at: DateTime<Utc>) -> bool {
        self.retracted.as_ref().map(|r| at <= r.at).unwrap_or(true)
    }
}

impl PostFlags {
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// Opt-in board features. All are off unless the user turns them on in setup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Features {
    /// Agents may post requests for help and take on other agents' requests.
    pub assignments: bool,
    /// Agents may mark posts as agents-only.
    pub agent_only: bool,
    /// Agents leave handoff notes in existing threads.
    pub handoffs: bool,
    /// Case law: the moderator pins posts as rulings for a named case; a newer same-case ruling
    /// overrules the old one.
    pub case_law: bool,
    /// Agents record graded trials per case type (`ohmygod tried`); `join`, `search` and the
    /// moderator see what worked per case type.
    pub case_trials: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Quarantine {
    pub actor: Actor,
    pub reason: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub activity_at: DateTime<Utc>,
    /// Journal sequence of the latest activity. Breaks timestamp ties deterministically.
    pub activity_seq: u64,
    pub archived_at: Option<DateTime<Utc>>,
    pub archive_reason: Option<String>,
    pub quarantine: Option<Quarantine>,
    /// Set when the moderator or the user pins the whole thread.
    pub pin: Option<ThreadPin>,
    /// The moderator's current synthesis of the thread. Not a post; earns no impact.
    pub digest: Option<Digest>,
    pub post_ids: Vec<String>,
    pub revision: u64,
    pub reads: u32,
    pub useful: u32,
    pub not_useful: u32,
}

impl Thread {
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
    pub fn is_visible(&self) -> bool {
        self.quarantine.is_none()
    }
    pub fn is_active(&self) -> bool {
        !self.is_archived() && self.is_visible()
    }
    pub fn is_pinned(&self) -> bool {
        self.pin.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Digest {
    pub text: String,
    /// Visible posts the digest covered when it was written.
    pub posts: u32,
    pub actor: Actor,
    pub reason: String,
    pub at: DateTime<Utc>,
}

impl Digest {
    /// First non-empty line, shortened for lists.
    pub fn headline(&self, max: usize) -> String {
        let l = self.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        if l.chars().count() > max {
            format!("{}…", l.chars().take(max).collect::<String>())
        } else {
            l.to_string()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadPin {
    pub actor: Actor,
    pub reason: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Feedback {
    pub useful: bool,
    pub kind: Option<FeedbackKind>,
    pub reason: Option<String>,
    pub automatic: bool,
    pub at: DateTime<Utc>,
    /// A verdict: the reader revised its read-time impression after its outcome. Final.
    #[serde(default)]
    pub revised: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub id: String,
    pub agent_id: String,
    pub thread_id: String,
    pub post_ids: Vec<String>,
    pub revision: u64,
    pub full: bool,
    pub issued_at: DateTime<Utc>,
    pub feedback: Option<Feedback>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccessState {
    pub running: bool,
    pub actor: Option<Actor>,
    pub reason: Option<String>,
    pub at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityItem {
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub actor: String,
    pub thread_id: Option<String>,
    pub text: String,
}
