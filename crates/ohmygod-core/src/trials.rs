//! Graded trials per case type (`ohmygod tried`).
//!
//! After a graded task an agent records the KIND of case it handled as a few `name=value` fields
//! copied from the task's data, what it did (an action and an optional reason code) and the score
//! it got. The board groups the trials by case type and shows per type the option that WORKS (it
//! passed and has not failed since), a working option that later failed (`changed?`), or the
//! options that failed while nothing works yet. Types are never pooled.
//!
//! Case-type keys. Agents give every categorical field of the case record. Field names and values
//! are canonicalised to lowercase snake_case, bracketed asides dropped; a value that contains a
//! digit is dropped (amounts, dates and IDs would give every case its own type). All fields are
//! stored per trial and searchable. The key is computed from the board's own trials, so old trials
//! are re-keyed as evidence arrives: it starts as the case's KIND (its first field in the board's
//! field order that is not name-like) and grows only by the fields the OUTCOMES showed to matter
//! for that kind. When within one type an option passed for some trials and, for others, failed or
//! lost to another option that passed, the stored field whose values best separate the two becomes
//! a key field of the kind (see [`learn`]); the moment and the evidence are kept for the listing
//! (`level since 13:40 (fight passed for forest, not for cave)`). A NAME-LIKE field (many
//! distinct values, or more than a few mostly seen once: counterparty or item names) never joins
//! the key.
//!
//! Exceptions by name. When the same option both passed and failed within a type, the board looks
//! for one name-like field whose value(s) only failed while the option passed for other values. The type line then carries an exception
//! (`— except enemy=troll: failed fight 2x; WORKING there sneak (1 pass)`; after a single failure
//! `— possible exception ...`), and the rest of the
//! type is summarised without those trials. When no single field separates them, the line asks for
//! the missing field (`mixed: ...`).
//!
//! Shared vocabulary. The board lists the field names in use, with counts and an example, and the
//! actions in use, where agents look (`join`, the reply of `tried` and its errors), so that agents
//! reuse the same words and trials of one kind of case group together. Names and values match only
//! by their exact canonical form; a new name or value that looks like an established one gets a
//! hint in the reply, never a rewrite.
//!
//! Actions. Only the first word of an action counts (`approved per policy` -> `approved`; the full
//! text is kept), and words match by a language-generic stem (`denied` ~ `deny`, `approved` ~
//! `approve`), rendered in the board's most common spelling.
//!
//! Split hint. When the same option both passed and failed within one type and no stored field
//! separates the outcomes, the type's entry says so (`mixed: ...`) and names the fields other
//! agents used, so the next agent adds the field that separates them.
//!
//! Only what agents report from their own graded outcomes is used.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use chrono::{DateTime, Utc};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// A score at or above this counts as a pass.
pub const PASS_AT: u32 = 50;
/// Case-type lines in the join listing and the moderator prompt.
pub const BLOCK_LINES: usize = 12;
/// Case-type lines in search results.
pub const MATCH_LINES: usize = 6;
/// Case-type lines in a thread document (the moderator reviews many documents in one budget).
pub const DOC_LINES: usize = 3;
/// Failures in a type without a working option before the board says the default action failed.
pub const DEFAULT_FAILED_AT: u32 = 2;
/// Fields a case type keeps at most.
pub const MAX_FIELDS: usize = 8;
/// Characters a canonical field name or value keeps at most.
pub const MAX_PART_CHARS: usize = 40;
/// Field names the vocabulary line lists at most.
pub const VOCAB_NAMES: usize = 8;
/// Actions the action line lists at most.
pub const VOCAB_ACTIONS: usize = 8;
/// Field names a split hint suggests at most.
const SPLIT_FIELDS: usize = 5;
/// A field with more distinct values than this is name-like.
pub const NAME_LIKE_DISTINCT: usize = 10;
/// A field with more distinct values than this, most of them seen once, is name-like.
pub const NAME_LIKE_MIN: usize = 6;
/// Failures of an option for one value before an exception is called a rule.
pub const RULE_AT: u32 = 2;
/// Exceptions a type line shows at most.
pub const MAX_EXCEPTIONS: usize = 3;
/// Refinement rounds per type (one separating field per round).
const EXCEPTION_ROUNDS: usize = 3;
/// Established values a reuse hint lists at most.
const HINT_VALUES: usize = 5;

/// Values that say "no value": the field is left out instead.
const EMPTY_VALUES: &[&str] = &["none", "null", "nil", "na", "n_a", "unknown", "unspecified", "not_applicable"];

/// One graded trial as recorded by an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trial {
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub agent_id: String,
    /// Case-type key: the field values in vocabulary order, joined by `/`.
    pub case_type: String,
    #[serde(default)]
    pub fields: CaseFields,
    /// The action's first word, as recorded.
    pub action: String,
    /// The action as the agent wrote it, when it held more than one word.
    #[serde(default)]
    pub action_text: Option<String>,
    pub reason: Option<String>,
    pub score: u32,
    pub case_id: Option<String>,
}

impl Trial {
    pub fn passed(&self) -> bool {
        self.score >= PASS_AT
    }
    pub fn option(&self) -> String {
        option_label(&self.action, self.reason.as_deref())
    }
}

/// The canonical `name=value` fields of a case type, in key order. Serialised as a JSON object in
/// that order; read back in document order, so journals written by earlier versions (an object
/// with fixed field names) load unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaseFields(pub Vec<(String, String)>);

impl CaseFields {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(n, _)| n.as_str())
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// The values joined by `/`: the case-type key.
    pub fn key(&self) -> String {
        self.0.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>().join("/")
    }
    /// `name=value name=value`.
    pub fn pairs_text(&self) -> String {
        self.0.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join(" ")
    }
}

impl Serialize for CaseFields {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (n, v) in &self.0 {
            m.serialize_entry(n, v)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for CaseFields {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = CaseFields;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object of field names to values, or a list of [name, value] pairs")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<CaseFields, A::Error> {
                let mut out = vec![];
                while let Some((k, v)) = map.next_entry::<String, Option<String>>()? {
                    if let Some(v) = v {
                        out.push((k, v));
                    }
                }
                Ok(CaseFields(out))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<CaseFields, A::Error> {
                let mut out = vec![];
                while let Some((k, v)) = seq.next_element::<(String, String)>()? {
                    out.push((k, v));
                }
                Ok(CaseFields(out))
            }
            fn visit_unit<E: de::Error>(self) -> Result<CaseFields, E> {
                Ok(CaseFields::default())
            }
        }
        d.deserialize_any(V)
    }
}

// ------------------------------------------------------------------ canonical names and values

/// Lowercase snake_case: letters and digits kept, every other run of characters becomes one `_`.
/// With `drop_brackets`, text in (), [] or {} is left out.
fn snake(raw: &str, drop_brackets: bool) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut gap = false;
    for c in raw.chars() {
        if drop_brackets {
            match c {
                '(' | '[' | '{' => {
                    depth += 1;
                    gap = true;
                    continue;
                }
                ')' | ']' | '}' if depth > 0 => {
                    depth -= 1;
                    gap = true;
                    continue;
                }
                _ if depth > 0 => continue,
                _ => {}
            }
        }
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.extend(c.to_lowercase());
        } else {
            gap = true;
        }
    }
    let cut: String = out.chars().take(MAX_PART_CHARS).collect();
    cut.trim_end_matches('_').to_string()
}

/// Canonical form of a name or value: snake_case without bracketed asides (with them when that
/// would leave nothing, so `(none)` reads as `none`).
fn canonical(raw: &str) -> String {
    let s = snake(raw, true);
    if s.is_empty() { snake(raw, false) } else { s }
}

/// Canonical field name, `None` when nothing is left.
pub fn canonical_name(raw: &str) -> Option<String> {
    Some(canonical(raw)).filter(|s| !s.is_empty())
}

/// Why a field value was not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// It contains a digit: an amount, a date or an ID.
    Digits,
    /// Nothing is left, or it says "no value" (`none`, `n/a`, `unknown`).
    Empty,
}

/// Canonical field value: lowercase snake_case; dropped when it contains a digit or says "no value".
pub fn canonical_value(raw: &str) -> Result<String, Dropped> {
    let s = canonical(raw);
    if s.chars().any(char::is_numeric) {
        return Err(Dropped::Digits);
    }
    if s.is_empty() || EMPTY_VALUES.contains(&s.as_str()) {
        return Err(Dropped::Empty);
    }
    Ok(s)
}

/// `name=value` pairs from `--field` arguments. One argument may hold several pairs separated by
/// spaces, commas or semicolons (`a=x, b=y`); words without `=` continue the previous value
/// (`a=two words`). Returns the pairs and the arguments that held no `name=value`.
pub fn parse_field_args(items: &[String]) -> (Vec<(String, String)>, Vec<String>) {
    let mut pairs: Vec<(String, String)> = vec![];
    let mut malformed = vec![];
    for item in items {
        // `name = value` -> `name=value`.
        let mut tight = String::new();
        let mut skip_ws = false;
        for c in item.chars() {
            if c == '=' {
                while tight.ends_with(char::is_whitespace) {
                    tight.pop();
                }
                tight.push('=');
                skip_ws = true;
                continue;
            }
            if skip_ws && c.is_whitespace() {
                continue;
            }
            skip_ws = false;
            tight.push(c);
        }
        let mut current: Option<usize> = None;
        for tok in tight.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|t| !t.is_empty()) {
            match tok.split_once('=') {
                Some((n, v)) => {
                    pairs.push((n.to_string(), v.to_string()));
                    current = Some(pairs.len() - 1);
                }
                None => match current {
                    Some(i) => {
                        let v = &mut pairs[i].1;
                        if !v.is_empty() {
                            v.push(' ');
                        }
                        v.push_str(tok);
                    }
                    None => malformed.push(tok.to_string()),
                },
            }
        }
    }
    (pairs, malformed)
}

/// The case-type fields of a `tried` call's `fields` argument: a list of `name=value` strings
/// (what the CLI sends), one such string, an object of names to values, or a list of
/// `[name, value]` pairs.
pub fn field_args(v: Option<&Value>) -> (Vec<(String, String)>, Vec<String>) {
    let text = |x: &Value| match x {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    match v {
        Some(Value::Object(m)) => (m.iter().map(|(k, x)| (k.clone(), text(x))).collect(), vec![]),
        Some(Value::String(s)) => parse_field_args(std::slice::from_ref(s)),
        Some(Value::Array(items)) => {
            let mut strings = vec![];
            let mut pairs = vec![];
            for x in items {
                match x {
                    Value::Array(p) if p.len() == 2 => pairs.push((text(&p[0]), text(&p[1]))),
                    other => strings.push(text(other)),
                }
            }
            let (mut parsed, malformed) = parse_field_args(&strings);
            parsed.extend(pairs);
            (parsed, malformed)
        }
        _ => (vec![], vec![]),
    }
}

// ------------------------------------------------------------------ shared vocabulary

/// One field name in use.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldUse {
    pub name: String,
    /// Trials that gave this field.
    pub trials: usize,
    /// Values in use, most used first.
    pub values: Vec<(String, usize)>,
}

/// The field names agents use, in key order (order of first use), with counts and an example.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vocabulary {
    pub fields: Vec<FieldUse>,
    /// The newest fields of the most used set of field names.
    pub example: Option<CaseFields>,
    /// Per `name=value`: the other field names given with it, and how often.
    pub with: HashMap<(String, String), HashMap<String, usize>>,
}

impl Vocabulary {
    pub fn from_trials(trials: &[Trial]) -> Vocabulary {
        let mut sorted: Vec<&Trial> = trials.iter().filter(|t| !t.fields.is_empty()).collect();
        sorted.sort_by_key(|t| t.seq);
        let mut order: Vec<String> = vec![];
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut values: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        let mut sets: HashMap<Vec<&str>, (usize, u64)> = HashMap::new();
        let mut with: HashMap<(String, String), HashMap<String, usize>> = HashMap::new();
        for t in &sorted {
            for (n, v) in &t.fields.0 {
                let e = with.entry((n.clone(), v.clone())).or_default();
                for other in t.fields.names().filter(|o| o != n) {
                    *e.entry(other.to_string()).or_default() += 1;
                }
            }
            for (n, v) in &t.fields.0 {
                if !counts.contains_key(n) {
                    order.push(n.clone());
                }
                *counts.entry(n.clone()).or_default() += 1;
                let vs = values.entry(n.clone()).or_default();
                match vs.iter_mut().find(|(x, _)| x == v) {
                    Some(e) => e.1 += 1,
                    None => vs.push((v.clone(), 1)),
                }
            }
            let e = sets.entry(t.fields.names().collect()).or_default();
            e.0 += 1;
            e.1 = t.seq;
        }
        let fields = order
            .into_iter()
            .map(|name| {
                let mut vs = values.remove(&name).unwrap_or_default();
                vs.sort_by(|a, b| b.1.cmp(&a.1)); // stable: ties keep first use
                FieldUse { trials: counts[&name], values: vs, name }
            })
            .collect();
        let best = sets.iter().max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(a.1 .1.cmp(&b.1 .1))).map(|(names, _)| names.clone());
        let example = best.and_then(|names| sorted.iter().rev().find(|t| t.fields.names().eq(names.iter().copied())).map(|t| t.fields.clone()));
        Vocabulary { fields, example, with }
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&FieldUse> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Key position of a name; `None` for a name not in use yet.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }

    /// Trials that gave `name=value`.
    pub fn value_count(&self, name: &str, value: &str) -> usize {
        self.get(name).and_then(|f| f.values.iter().find(|v| v.0 == value)).map(|v| v.1).unwrap_or(0)
    }

    /// A field whose values look like names (counterparties, items): more than
    /// [`NAME_LIKE_DISTINCT`] distinct values, or more than [`NAME_LIKE_MIN`] with most of them seen
    /// once. Kept per trial, never in a key.
    pub fn name_like(&self, name: &str) -> bool {
        self.get(name).is_some_and(|f| {
            let distinct = f.values.len();
            let once = f.values.iter().filter(|v| v.1 == 1).count();
            distinct > NAME_LIKE_DISTINCT || (distinct > NAME_LIKE_MIN && once * 2 > distinct)
        })
    }

    /// The kind of a case: its first field (in key order) that is not name-like, `None` when it
    /// has none.
    pub fn anchor<'a>(&self, fields: &'a CaseFields) -> Option<&'a (String, String)> {
        fields.0.iter().filter(|(n, _)| !self.name_like(n)).min_by_key(|(n, _)| self.position(n).unwrap_or(usize::MAX))
    }

    /// The field names that hold the kind of case, in key order.
    pub fn anchor_names(&self, trials: &[Trial]) -> Vec<String> {
        let mut names: Vec<String> = vec![];
        for t in trials {
            if let Some((n, _)) = self.anchor(&t.fields) {
                if !names.contains(n) {
                    names.push(n.clone());
                }
            }
        }
        names.sort_by_key(|n| self.position(n).unwrap_or(usize::MAX));
        names
    }

    /// The names worth listing: the [`VOCAB_NAMES`] most used, in key order.
    pub fn listed(&self) -> Vec<&FieldUse> {
        let mut top: Vec<(usize, &FieldUse)> = self.fields.iter().enumerate().collect();
        top.sort_by(|a, b| b.1.trials.cmp(&a.1.trials).then(a.0.cmp(&b.0)));
        top.truncate(VOCAB_NAMES);
        top.sort_by_key(|x| x.0);
        top.into_iter().map(|x| x.1).collect()
    }

    /// `level (31), enemy (29), weather (8)`, plus `+2 rarely used` when more names are in use.
    pub fn names_text(&self) -> String {
        let listed = self.listed();
        let mut text = listed
            .iter()
            .map(|f| if self.name_like(&f.name) { format!("{} ({}, names)", f.name, f.trials) } else { format!("{} ({})", f.name, f.trials) })
            .collect::<Vec<_>>()
            .join(", ");
        if self.fields.len() > listed.len() {
            text.push_str(&format!(", +{} rarely used", self.fields.len() - listed.len()));
        }
        text
    }

    /// The name-like fields in use, in key order.
    pub fn name_fields(&self) -> Vec<&str> {
        self.fields.iter().filter(|f| self.name_like(&f.name)).map(|f| f.name.as_str()).collect()
    }

    /// `Case-type fields in use, reuse these names: a (3), b (2); e.g. a=x b=y`. Empty when no
    /// trial has fields.
    pub fn line(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let example = self.example.as_ref().map(|e| format!("; e.g. {}", e.pairs_text())).unwrap_or_default();
        format!("Case-type fields in use, reuse these names: {}{example}", self.names_text())
    }

    /// The established values of a field, `v (n)`, most used first.
    fn values_text(&self, name: &str) -> String {
        self.get(name)
            .map(|f| f.values.iter().take(HINT_VALUES).map(|(v, n)| format!("{v} ({n})")).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    }
}

/// Spellings of a word for hints only: without `_`, and without a plural ending.
fn looks(s: &str) -> Vec<String> {
    let flat: String = s.chars().filter(|c| *c != '_').collect();
    let mut out = vec![flat.clone()];
    if let Some(x) = flat.strip_suffix("ies") {
        out.push(format!("{x}y"));
    }
    if let Some(x) = flat.strip_suffix("es") {
        out.push(x.to_string());
    }
    if let Some(x) = flat.strip_suffix('s') {
        out.push(x.to_string());
    }
    out
}

/// `a` and `b` look like one word (`merge_conflict` / `mergeconflicts`), or one is a run of the other's
/// snake_case words (`basic_plan` / `basic`). Used for hints, never to merge.
fn looks_alike(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let (la, lb) = (looks(a), looks(b));
    if la.iter().any(|x| lb.contains(x)) {
        return true;
    }
    let contains = |long: &str, short: &str| {
        let (l, s): (Vec<&str>, Vec<&str>) = (long.split('_').collect(), short.split('_').collect());
        s.len() < l.len() && l.windows(s.len()).any(|w| w == s.as_slice())
    };
    contains(a, b) || contains(b, a)
}

// ------------------------------------------------------------------ recording

/// A new trial's case type and what the reply should tell the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub fields: CaseFields,
    /// Notes for the agent: dropped values, new names and values, names in use.
    pub notes: Vec<String>,
}

fn quoted(items: &[String]) -> String {
    items.iter().map(|x| format!("`{x}`")).collect::<Vec<_>>().join(", ")
}

/// The usage text shared by `tried`'s error and hints.
pub fn field_usage(vocab: &Vocabulary) -> String {
    let mut text = String::from(
        "Give EVERY categorical field of the case record (up to 8): its kinds, classes, statuses, places and counterparty names, as `--field <name>=<value>`, repeated, copied verbatim, each name the label the value has there; never numbers, dates or IDs.",
    );
    if !vocab.is_empty() {
        text.push(' ');
        text.push_str(&vocab.line());
        text.push('.');
    }
    text
}

/// Canonical fields for a new trial from the raw `name=value` pairs, given the trials so far:
/// names and values canonicalised, values with digits or without content dropped, the first use
/// of each name kept, ordered by the vocabulary (new names last, in the order given), at most
/// [`MAX_FIELDS`]. `Err` with the text for the agent when no field is left. The case-type key is
/// computed from these fields later, by the board's vocabulary ([`Vocabulary::key_of`]).
pub fn prepare(pairs: &[(String, String)], malformed: &[String], trials: &[Trial]) -> Result<Prepared, String> {
    let vocab = Vocabulary::from_trials(trials);
    let mut kept: Vec<(usize, String, String)> = vec![];
    let (mut digits, mut empty, mut twice, mut unreadable) = (vec![], vec![], vec![], malformed.to_vec());
    for (i, (n, v)) in pairs.iter().enumerate() {
        let Some(name) = canonical_name(n) else {
            unreadable.push(format!("{n}={v}"));
            continue;
        };
        match canonical_value(v) {
            Err(Dropped::Digits) => digits.push(format!("{name}={}", v.trim())),
            Err(Dropped::Empty) => empty.push(format!("{name}={}", v.trim())),
            Ok(value) if kept.iter().any(|k| k.1 == name) => twice.push(format!("{name}={value}")),
            Ok(value) => kept.push((i, name, value)),
        }
    }
    kept.sort_by_key(|(i, name, _)| (vocab.position(name).unwrap_or(usize::MAX), *i));
    let extra: Vec<String> = kept.iter().skip(MAX_FIELDS).map(|(_, n, v)| format!("{n}={v}")).collect();
    kept.truncate(MAX_FIELDS);
    let mut notes = vec![];
    if !digits.is_empty() {
        notes.push(format!(
            "Dropped {}: a value with digits (an amount, a date, an ID) would give every case its own type; give only fields that name the kind of case.",
            quoted(&digits)
        ));
    }
    if !unreadable.is_empty() {
        notes.push(format!("Could not read {}: write each field as `--field <name>=<value>`.", quoted(&unreadable)));
    }
    if kept.is_empty() {
        let mut text = String::from("No case-type field left to record.");
        for n in &notes {
            text.push(' ');
            text.push_str(n);
        }
        if !empty.is_empty() {
            text.push_str(&format!(" Left out {}: no value.", quoted(&empty)));
        }
        text.push(' ');
        text.push_str(&field_usage(&vocab));
        return Err(text);
    }
    if !empty.is_empty() {
        notes.push(format!("Left out {}: a field without a value is no field.", quoted(&empty)));
    }
    if !twice.is_empty() {
        notes.push(format!("Kept the first value of a field given twice; dropped {}.", quoted(&twice)));
    }
    if !extra.is_empty() {
        notes.push(format!("A trial keeps at most {MAX_FIELDS} fields; dropped {}.", quoted(&extra)));
    }
    if !vocab.is_empty() {
        let mut new_names = vec![];
        for (_, name, value) in &kept {
            match vocab.get(name) {
                None => match vocab.fields.iter().find(|f| looks_alike(name, &f.name)) {
                    Some(f) => notes.push(format!(
                        "`{name}` is a new field name; the board has `{}` ({} trials): if it means the same, use exactly that name next time.",
                        f.name, f.trials
                    )),
                    None => new_names.push(name.clone()),
                },
                Some(f) if !f.values.iter().any(|(v, _)| v == value) => {
                    if let Some((v, n)) = f.values.iter().find(|(v, _)| looks_alike(value, v)) {
                        notes.push(format!(
                            "`{name}={value}` is a new value; the board has `{name}={v}` ({n} trials): if it is the same kind of case, use exactly that value next time. Values in use: {}.",
                            vocab.values_text(name)
                        ));
                    }
                }
                Some(_) => {}
            }
        }
        if !new_names.is_empty() {
            notes.push(format!(
                "New field name(s) {}. Names in use: {}. Reuse them when your case has those fields, so that trials of one kind of case group together.",
                quoted(&new_names),
                vocab.names_text()
            ));
        }
    }
    if kept.len() == 1 && pairs.len() == 1 {
        notes.push("One field pools every case that shares its value: give every categorical field of the case record (up to 8).".into());
    }
    Ok(Prepared { fields: CaseFields(kept.into_iter().map(|(_, n, v)| (n, v)).collect()), notes })
}

/// Normalise an action or reason code. "none", "null", "-" and "" mean no reason.
pub fn normalize_code(raw: &str) -> Option<String> {
    let s = norm_part(raw);
    match s.as_str() {
        "" | "none" | "null" | "no_reason" | "n/a" | "na" | "-" => None,
        _ => Some(s),
    }
}

/// An action or reason code: lowercase, trimmed, inner runs of whitespace, `-` and `_` become one `_`.
fn norm_part(s: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in s.trim().chars() {
        if c.is_whitespace() || c == '-' || c == '_' {
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push('_');
            gap = false;
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// The first word of an action, snake-cased (`Approved; per policy` -> `approved`). `None` when it
/// has no word.
pub fn action_word(raw: &str) -> Option<String> {
    let s = snake(raw, false);
    s.split('_').next().filter(|w| !w.is_empty()).map(str::to_string)
}

/// A language-generic stem for matching action words: `...ied` -> `...y`, then a trailing `ed` or
/// `e` dropped (`denied`/`deny`, `approved`/`approve`, `escalated`/`escalate` match). Never cut
/// below three characters.
pub fn action_stem(word: &str) -> String {
    let w = word.to_lowercase();
    if let Some(x) = w.strip_suffix("ied").filter(|x| x.len() >= 2) {
        return format!("{x}y");
    }
    for end in ["ed", "e"] {
        if let Some(x) = w.strip_suffix(end).filter(|x| x.len() >= 3) {
            return x.to_string();
        }
    }
    w
}

/// A new trial's action: its first word and, when the agent wrote more, the full text and a note.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedAction {
    pub word: String,
    pub text: Option<String>,
    pub note: Option<String>,
}

/// The action of a new trial. `Err` with the text for the agent when it has no word.
pub fn prepare_action(raw: &str, trials: &[Trial]) -> Result<PreparedAction, String> {
    let Some(word) = action_word(raw) else {
        let actions = ActionVocab::from_trials(trials).line();
        let listed = if actions.is_empty() { String::new() } else { format!(" {actions}.") };
        return Err(format!("Give what you did as one word with --action, e.g. --action <the action you took>.{listed}"));
    };
    let full = raw.trim();
    let more = snake(full, false) != word;
    let text = more.then(|| full.chars().take(200).collect::<String>());
    let note = more.then(|| {
        let vocab = ActionVocab::from_trials(trials);
        let shown = vocab.surface(&word);
        format!(
            "Recorded the action as `{shown}`: an action is one word (your full text is kept). Put a reason code in --reason, not in the action."
        )
    });
    Ok(PreparedAction { word, text, note })
}

/// The actions agents use: per stem, its most common spelling and how often it was used.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActionVocab {
    /// stem -> (spelling shown, trials), most used first.
    pub actions: Vec<(String, String, usize)>,
}

impl ActionVocab {
    pub fn from_trials(trials: &[Trial]) -> ActionVocab {
        let mut sorted: Vec<&Trial> = trials.iter().collect();
        sorted.sort_by_key(|t| t.seq);
        // stem -> (trials, spellings in first-use order with counts)
        let mut by_stem: Vec<(String, usize, Vec<(String, usize)>)> = vec![];
        for t in sorted {
            let word = action_word(&t.action).unwrap_or_else(|| t.action.clone());
            let stem = action_stem(&word);
            let i = match by_stem.iter().position(|x| x.0 == stem) {
                Some(i) => i,
                None => {
                    by_stem.push((stem, 0, vec![]));
                    by_stem.len() - 1
                }
            };
            let e = &mut by_stem[i];
            e.1 += 1;
            match e.2.iter_mut().find(|(w, _)| *w == word) {
                Some(x) => x.1 += 1,
                None => e.2.push((word, 1)),
            }
        }
        let mut actions: Vec<(String, String, usize)> = by_stem
            .into_iter()
            .map(|(stem, n, spellings)| {
                // Most common spelling; ties keep the first used.
                let best = spellings.iter().fold(None::<&(String, usize)>, |b, x| if b.is_none_or(|b| x.1 > b.1) { Some(x) } else { b });
                (stem, best.map(|b| b.0.clone()).unwrap_or_default(), n)
            })
            .collect();
        actions.sort_by(|a, b| b.2.cmp(&a.2)); // stable: ties keep first use
        ActionVocab { actions }
    }

    /// How a word is shown: the board's most common spelling of its stem (the word itself when new).
    pub fn surface(&self, word: &str) -> String {
        let stem = action_stem(word);
        self.actions.iter().find(|a| a.0 == stem).map(|a| a.1.clone()).unwrap_or_else(|| word.to_string())
    }

    /// `Actions in use: approve (40), deny (9)`. Empty when no trial is recorded.
    pub fn line(&self) -> String {
        if self.actions.is_empty() {
            return String::new();
        }
        let mut text = self.actions.iter().take(VOCAB_ACTIONS).map(|a| format!("{} ({})", a.1, a.2)).collect::<Vec<_>>().join(", ");
        if self.actions.len() > VOCAB_ACTIONS {
            text.push_str(&format!(", +{} rarely used", self.actions.len() - VOCAB_ACTIONS));
        }
        format!("Actions in use: {text}")
    }
}

pub fn option_label(action: &str, reason: Option<&str>) -> String {
    match reason {
        Some(r) => format!("{action}/{r}"),
        None => action.to_string(),
    }
}

// ------------------------------------------------------------------ per-type summaries

/// What the trials say about one option within one case type.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OptionStats {
    pub option: String,
    pub passes: u32,
    pub fails: u32,
    pub last_pass: Option<u64>,
    pub last_fail: Option<u64>,
    pub last_pass_at: Option<DateTime<Utc>>,
    pub last_fail_at: Option<DateTime<Utc>>,
    /// Passes after the option's last fail.
    pub passes_since_fail: u32,
    /// Fails after the option's last pass.
    pub fails_since_pass: u32,
    /// The recorded values of the cases it passed for.
    pub pass_ev: Evidence,
    /// The recorded values of the cases it failed for.
    pub fail_ev: Evidence,
}

/// What the cases behind an option's passes (or failures) had recorded: the values of their
/// low-variety fields (not name-like) that are not in the type's key.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Evidence {
    /// `time=night, enemy=bat 2x|wolf 1x`; empty when no such field.
    pub text: String,
    /// Every such field has one value across the cases: a claim holds for exactly these values.
    pub homogeneous: bool,
}

/// Fields an evidence text names at most (homogeneity covers all of them).
const EVIDENCE_FIELDS: usize = MAX_FIELDS;
/// Values an evidence text names per field at most.
const EVIDENCE_VALUES: usize = 3;

/// The evidence of a set of cases, leaving out the key fields. Low-variety fields decide whether
/// the cases share their values; name fields are listed too (a reader with another name sees the
/// claim was not tested on it). For a claim (`names` = the name fields the kind's trials use),
/// a name field its cases did not record reads `none recorded`, and a note says the claim is
/// untested for other names.
fn evidence(cases: &[&CaseFields], vocab: &Vocabulary, key: &[String], names: Option<&[String]>) -> Evidence {
    let mut fields: Vec<&str> = vec![];
    for c in cases {
        for n in c.names() {
            if !fields.contains(&n) && !key.iter().any(|k| k == n) {
                fields.push(n);
            }
        }
    }
    fields.sort_by_key(|n| (vocab.name_like(n), vocab.position(n).unwrap_or(usize::MAX)));
    let mut homogeneous = true;
    let mut parts = vec![];
    for (i, n) in fields.iter().enumerate() {
        let mut vals: Vec<(&str, usize)> = vec![];
        for v in cases.iter().filter_map(|c| c.get(n)) {
            match vals.iter_mut().find(|x| x.0 == v) {
                Some(x) => x.1 += 1,
                None => vals.push((v, 1)),
            }
        }
        vals.sort_by(|a, b| b.1.cmp(&a.1));
        if vals.len() > 1 && !vocab.name_like(n) {
            homogeneous = false;
        }
        if i >= EVIDENCE_FIELDS {
            continue;
        }
        if vals.len() == 1 {
            parts.push(format!("{n}={}", vals[0].0));
        } else {
            let mut shown: Vec<String> = vals.iter().take(EVIDENCE_VALUES).map(|(v, k)| format!("{v} {k}x")).collect();
            if vals.len() > EVIDENCE_VALUES {
                shown.push(format!("+{}", vals.len() - EVIDENCE_VALUES));
            }
            parts.push(format!("{n}={}", shown.join("|")));
        }
    }
    if fields.len() > EVIDENCE_FIELDS {
        parts.push(format!("+{} fields", fields.len() - EVIDENCE_FIELDS));
    }
    if let Some(names) = names {
        for n in names.iter().filter(|n| !key.iter().any(|k| k == *n)) {
            if !fields.contains(&n.as_str()) {
                parts.push(format!("{n}=none recorded"));
            }
            parts.push(format!("untested for other {n} values"));
        }
    }
    Evidence { text: parts.join(", "), homogeneous }
}

impl OptionStats {
    /// A recent pass and no later fail.
    pub fn working(&self) -> bool {
        matches!((self.last_pass, self.last_fail), (Some(p), f) if f.is_none_or(|f| p > f))
    }
    /// Its newest outcome is a fail.
    pub fn failing(&self) -> bool {
        matches!((self.last_fail, self.last_pass), (Some(f), p) if p.is_none_or(|p| f > p))
    }
    /// It passed before and fails now: the rule may have changed.
    pub fn changed(&self) -> bool {
        self.failing() && self.passes > 0
    }
}

/// Everything the board knows about one case type.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeSummary {
    pub case_type: String,
    /// The fields of the type's newest trial (empty for the earliest journals).
    pub fields: CaseFields,
    /// Sequence of the newest trial for this type.
    pub last_seq: u64,
    pub trials: usize,
    /// Failed trials of this type (outside its exceptions).
    pub fails: u32,
    /// Options that pass with no later fail, newest pass first.
    pub working: Vec<OptionStats>,
    /// Options whose newest outcome is a fail, newest fail first.
    pub failed: Vec<OptionStats>,
    /// Options that both passed and failed here (any order) and that no stored field separates,
    /// most trials first.
    pub mixed: Vec<OptionStats>,
    /// Values of one stored field for which an option only failed while it passed for others.
    pub exceptions: Vec<Exception>,
    /// Stored values of the type's trials that are not in its key (names, rarely given fields).
    pub values: Vec<String>,
}

impl TypeSummary {
    pub fn changed(&self) -> bool {
        self.working.is_empty() && self.failed.iter().any(|o| o.changed())
    }
}

/// An exception within a type: for `field=value` the option only failed while it passed for
/// other values. `summary` covers the type's trials with that value.
#[derive(Debug, Clone, PartialEq)]
pub struct Exception {
    pub field: String,
    pub value: String,
    pub option: String,
    /// Failures of the option for this value.
    pub fails: u32,
    pub summary: Box<TypeSummary>,
}

impl Exception {
    /// Two or more failures: a rule, not a single case.
    pub fn is_rule(&self) -> bool {
        self.fails >= RULE_AT
    }
}

/// A field the outcomes showed to matter for a kind of case: within one type an option passed
/// for some of its values and, for others, failed or lost to another option that passed.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyField {
    pub field: String,
    /// The trial whose outcome made it a key field.
    pub seq: u64,
    pub at: DateTime<Utc>,
    /// The option whose outcomes it separated.
    pub option: String,
    /// Values for which the option passed, most passes first.
    pub passed_for: Vec<String>,
    /// Values for which it failed or another option passed, most first.
    pub not_for: Vec<String>,
}

impl KeyField {
    /// `level since 13:40 (fight passed for forest, not for cave)`.
    pub fn text(&self) -> String {
        format!(
            "{} since {} ({} passed for {}, not for {})",
            self.field,
            hm(Some(self.at)),
            self.option,
            self.passed_for.join(", "),
            self.not_for.join(", ")
        )
    }
}

/// The key fields learned per kind of case (anchor `name=value`), in the order learned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Learned {
    pub kinds: Vec<((String, String), Vec<KeyField>)>,
}

impl Learned {
    pub fn fields_of(&self, kind: &(String, String)) -> &[KeyField] {
        self.kinds.iter().find(|(k, _)| k == kind).map(|(_, f)| f.as_slice()).unwrap_or(&[])
    }
    fn push(&mut self, kind: &(String, String), f: KeyField) {
        match self.kinds.iter_mut().find(|(k, _)| k == kind) {
            Some((_, fs)) => fs.push(f),
            None => self.kinds.push((kind.clone(), vec![f])),
        }
    }
}

/// Everything the board derives from its trials: the vocabularies, the key fields learned per
/// kind, and the case types with their lines' data, most recent first.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub vocab: Vocabulary,
    pub actions: ActionVocab,
    pub learned: Learned,
    pub types: Vec<TypeSummary>,
}

impl Analysis {
    /// The case-type key of a case's fields: its kind, then the kind's learned key fields in the
    /// order learned (`?` where the case did not give one). A case with name fields only keeps
    /// their values.
    pub fn key_of(&self, fields: &CaseFields) -> String {
        key_with(fields, &self.vocab, &self.learned)
    }

    pub fn type_of(&self, key: &str) -> Option<&TypeSummary> {
        self.types.iter().find(|s| s.case_type == key)
    }

    /// `quest: level since 13:39 (...); also recorded: weather, enemy (names)`.
    pub fn kind_line(&self, trials: &[Trial], kind: &(String, String)) -> String {
        let fields = self.learned.fields_of(kind);
        let key = if fields.is_empty() { "no key field yet".to_string() } else { fields.iter().map(KeyField::text).collect::<Vec<_>>().join(", ") };
        let mut others: Vec<&str> = vec![];
        for t in trials.iter().filter(|t| self.vocab.anchor(&t.fields) == Some(kind)) {
            for n in t.fields.names() {
                if n != kind.0 && !fields.iter().any(|f| f.field == n) && !others.contains(&n) {
                    others.push(n);
                }
            }
        }
        others.sort_by_key(|n| self.vocab.position(n).unwrap_or(usize::MAX));
        let others: Vec<String> = others.iter().map(|n| if self.vocab.name_like(n) { format!("{n} (names)") } else { n.to_string() }).collect();
        let also = if others.is_empty() { String::new() } else { format!("; also recorded: {}", others.join(", ")) };
        format!("{}: {key}{also}", kind.1)
    }

    /// The key fields learned per kind, most recently active kind first, at most `limit` kinds.
    pub fn learned_block(&self, trials: &[Trial], limit: usize) -> String {
        let mut kinds: Vec<(&(String, String), u64)> = vec![];
        for t in trials {
            if let Some(k) = self.vocab.anchor(&t.fields) {
                match kinds.iter_mut().find(|x| x.0 == k) {
                    Some(x) => x.1 = x.1.max(t.seq),
                    None => kinds.push((k, t.seq)),
                }
            }
        }
        if kinds.is_empty() {
            return String::new();
        }
        kinds.sort_by(|a, b| b.1.cmp(&a.1));
        let (keyed, plain): (Vec<_>, Vec<_>) = kinds.iter().partition(|(k, _)| !self.learned.fields_of(k).is_empty());
        let mut out = String::from("Key fields per kind, learned from outcomes (a field joins the key when an option passed for some of its values and not for others; other fields are recorded and searchable):\n");
        for (k, _) in keyed.iter().take(limit) {
            out.push_str(&format!("- {}\n", self.kind_line(trials, k)));
        }
        if keyed.len() > limit {
            out.push_str(&format!("- … and {} more kinds with key fields\n", keyed.len() - limit));
        }
        if !plain.is_empty() {
            let names: Vec<&str> = plain.iter().take(limit).map(|(k, _)| k.1.as_str()).collect();
            let more = if plain.len() > limit { format!(", +{} more", plain.len() - limit) } else { String::new() };
            out.push_str(&format!("- no key field yet (key = the kind alone): {}{more}\n", names.join(", ")));
        }
        out
    }
}

fn key_with(fields: &CaseFields, vocab: &Vocabulary, learned: &Learned) -> String {
    let Some(kind) = vocab.anchor(fields) else { return fields.key() };
    let mut parts = vec![kind.1.clone()];
    for f in learned.fields_of(kind) {
        parts.push(fields.get(&f.field).unwrap_or("?").to_string());
    }
    parts.join("/")
}

/// Key fields a kind learns at most.
const MAX_KEY_FIELDS: usize = 5;
/// How much more often an option must lose for a value than for the field's other values.
const CONTRAST: f64 = 0.5;

/// Learn the key fields per kind from the trials in journal order. After each trial the type it
/// falls in is checked (after a split, every type of its kind): when an option passed there and,
/// for other trials, failed or lost to another option that passed, the stored field (not the
/// kind, not name-like, not yet a key field) whose values best separate the two becomes a key field
/// of the kind. A value counts against the option when it holds at least `1 + log2(values)` more
/// such trials than passes, so that a field with many values (where some value always looks
/// special by chance) needs more evidence. Ranked by fewest trials left unexplained, then fewest
/// distinct values, then most trials carrying the field.
fn learn(trials: &[&Trial], vocab: &Vocabulary, option: &dyn Fn(&Trial) -> String) -> Learned {
    let mut learned = Learned::default();
    let mut members: Vec<((String, String), Vec<usize>)> = vec![];
    for (i, t) in trials.iter().enumerate() {
        let Some(kind) = vocab.anchor(&t.fields).cloned() else { continue };
        let idx = match members.iter().position(|(k, _)| *k == kind) {
            Some(j) => j,
            None => {
                members.push((kind.clone(), vec![]));
                members.len() - 1
            }
        };
        members[idx].1.push(i);
        let ids = members[idx].1.clone();
        let mut only_own = true;
        while learned.fields_of(&kind).len() < MAX_KEY_FIELDS {
            let mut groups: BTreeMap<String, Vec<&Trial>> = BTreeMap::new();
            for &j in &ids {
                groups.entry(key_with(&trials[j].fields, vocab, &learned)).or_default().push(trials[j]);
            }
            let own = key_with(&t.fields, vocab, &learned);
            let found = groups
                .iter()
                .filter(|(k, _)| !only_own || **k == own)
                .find_map(|(_, g)| find_split(g, &kind, learned.fields_of(&kind), vocab, option));
            only_own = false;
            match found {
                Some((field, opt, passed_for, not_for)) => {
                    learned.push(&kind, KeyField { field, seq: t.seq, at: t.at, option: opt, passed_for, not_for })
                }
                None => break,
            }
        }
    }
    learned
}

/// The best key-field split of one type, see [`learn`].
fn find_split(
    g: &[&Trial],
    kind: &(String, String),
    known: &[KeyField],
    vocab: &Vocabulary,
    option: &dyn Fn(&Trial) -> String,
) -> Option<(String, String, Vec<String>, Vec<String>)> {
    // Options that passed here, most passes first.
    let mut passing: Vec<(String, usize)> = vec![];
    for t in g.iter().filter(|t| t.passed()) {
        let o = option(t);
        match passing.iter_mut().find(|x| x.0 == o) {
            Some(x) => x.1 += 1,
            None => passing.push((o, 1)),
        }
    }
    passing.sort_by(|a, b| b.1.cmp(&a.1));
    let mut names: Vec<&str> = vec![];
    for t in g {
        for n in t.fields.names() {
            if !names.contains(&n) && n != kind.0 && !vocab.name_like(n) && !known.iter().any(|f| f.field == n) {
                names.push(n);
            }
        }
    }
    for (opt, _) in passing {
        let pos: Vec<&Trial> = g.iter().copied().filter(|t| t.passed() && option(t) == opt).collect();
        let neg: Vec<&Trial> = g.iter().copied().filter(|t| (option(t) == opt) != t.passed()).collect();
        if pos.is_empty() || neg.is_empty() {
            continue;
        }
        let mut best: Option<((usize, usize, std::cmp::Reverse<usize>, usize), String, Vec<String>, Vec<String>)> = None;
        for &name in &names {
            let count = |ts: &[&Trial]| -> Vec<(String, usize)> {
                let mut c: Vec<(String, usize)> = vec![];
                for v in ts.iter().filter_map(|t| t.fields.get(name)) {
                    match c.iter_mut().find(|x| x.0 == v) {
                        Some(x) => x.1 += 1,
                        None => c.push((v.to_string(), 1)),
                    }
                }
                c.sort_by(|a, b| b.1.cmp(&a.1));
                c
            };
            let (pv, nv) = (count(&pos), count(&neg));
            let of = |c: &[(String, usize)], v: &str| c.iter().find(|x| x.0 == v).map(|x| x.1).unwrap_or(0);
            let mut distinct: Vec<&str> = g.iter().filter_map(|t| t.fields.get(name)).collect();
            distinct.sort();
            distinct.dedup();
            let need = 1 + (distinct.len().max(2) as f64).log2().ceil() as usize;
            // ... and its rate of such trials must stand out from the other values' by at least
            // CONTRAST: a value that only reflects how often the option loses anyway (a large type
            // where one value of a many-valued field gathered a few more losses by chance) never
            // makes its field a key field.
            let total = neg.len() + pos.len();
            let against: Vec<String> = nv
                .iter()
                .filter(|(v, n)| {
                    let p = of(&pv, v);
                    let rate = *n as f64 / (n + p) as f64;
                    let rest = total - n - p;
                    let rest_rate = if rest == 0 { 0.0 } else { (neg.len() - n) as f64 / rest as f64 };
                    *n >= p + need && rate - rest_rate >= CONTRAST
                })
                .map(|(v, _)| v.clone())
                .collect();
            if against.is_empty() {
                continue;
            }
            let passed_for: Vec<String> = pv.iter().filter(|(v, _)| !against.contains(v)).map(|(v, _)| v.clone()).take(3).collect();
            if passed_for.is_empty() {
                continue;
            }
            let gain: usize = against.iter().map(|v| of(&nv, v) - of(&pv, v)).sum();
            let carry = g.iter().filter(|t| t.fields.get(name).is_some()).count();
            let rank = (neg.len() - gain, distinct.len(), std::cmp::Reverse(carry), vocab.position(name).unwrap_or(usize::MAX));
            if best.as_ref().is_none_or(|b| rank < b.0) {
                best = Some((rank, name.to_string(), passed_for, against.into_iter().take(3).collect()));
            }
        }
        if let Some((_, field, passed_for, not_for)) = best {
            return Some((field, opt, passed_for, not_for));
        }
    }
    None
}

/// The key a new trial with these fields gets once recorded on top of `trials`.
pub fn key_for_new(trials: &[Trial], fields: &CaseFields) -> String {
    let mut all = trials.to_vec();
    all.push(Trial {
        seq: u64::MAX,
        at: Utc::now(),
        agent_id: String::new(),
        case_type: fields.key(),
        fields: fields.clone(),
        action: String::new(),
        action_text: None,
        reason: None,
        score: 0,
        case_id: None,
    });
    analyze(&all).key_of(fields)
}

/// Analyse the trials: deduplicate (a later trial by the same agent for the same case replaces the
/// earlier one), learn the key fields per kind, group by key and summarise each type with its
/// exceptions. Options are the action's first word (matched by stem, shown in the board's most
/// common spelling) with the reason code.
pub fn analyze(trials: &[Trial]) -> Analysis {
    let vocab = Vocabulary::from_trials(trials);
    let actions = ActionVocab::from_trials(trials);
    let option = |t: &Trial| option_label(&actions.surface(&action_word(&t.action).unwrap_or_else(|| t.action.clone())), t.reason.as_deref());
    let kept = dedupe(trials);
    let learned = learn(&kept, &vocab, &option);
    let mut by_type: BTreeMap<String, Vec<&Trial>> = BTreeMap::new();
    for t in kept {
        let key = if t.fields.is_empty() { t.case_type.clone() } else { key_with(&t.fields, &vocab, &learned) };
        by_type.entry(key).or_default().push(t);
    }
    // The name fields each kind's trials use.
    let mut kind_names: HashMap<(String, String), Vec<String>> = HashMap::new();
    for ts in by_type.values() {
        for t in ts {
            if let Some(k) = vocab.anchor(&t.fields) {
                let e = kind_names.entry(k.clone()).or_default();
                for n in t.fields.names().filter(|n| vocab.name_like(n)) {
                    if !e.iter().any(|x| x == n) {
                        e.push(n.to_string());
                    }
                }
            }
        }
    }
    for v in kind_names.values_mut() {
        v.sort_by_key(|n| vocab.position(n).unwrap_or(usize::MAX));
    }
    let mut types: Vec<TypeSummary> = by_type.iter().map(|(ty, ts)| summarize_type(ty, ts, &option, &vocab, &learned, &kind_names)).collect();
    types.sort_by(|a, b| b.last_seq.cmp(&a.last_seq));
    Analysis { vocab, actions, learned, types }
}

/// The trials in journal order, a later trial by the same agent for the same case replacing the
/// earlier one.
fn dedupe(trials: &[Trial]) -> Vec<&Trial> {
    let mut sorted: Vec<&Trial> = trials.iter().collect();
    sorted.sort_by_key(|t| t.seq);
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut kept: Vec<&Trial> = vec![];
    for t in sorted.iter().rev() {
        if let Some(c) = t.case_id.as_deref() {
            if !seen.insert((t.agent_id.as_str(), c)) {
                continue;
            }
        }
        kept.push(t);
    }
    kept.reverse();
    kept
}

/// Groups the closest-cases section shows at most.
pub const CLOSEST_LINES: usize = 6;

/// The recorded cases closest to a search: trials whose recorded field values (any field, names
/// included) match the query's words, a value matching when all its words are in the query; grouped
/// by the values they match, most matching values first, then most trials. With a query made of a
/// case's own values, the first lines are the recorded cases most like it:
/// `[a=b c=d] x PASS 2x, y FAIL 1x`. When some group matches two or more values, groups matching one
/// are left out. An option that both passed and failed says which came last.
pub fn closest_cases(trials: &[Trial], query: &str, limit: usize) -> Vec<String> {
    let q = words(query);
    if q.is_empty() || trials.is_empty() {
        return vec![];
    }
    let vocab = Vocabulary::from_trials(trials);
    let actions = ActionVocab::from_trials(trials);
    let option = |t: &Trial| option_label(&actions.surface(&action_word(&t.action).unwrap_or_else(|| t.action.clone())), t.reason.as_deref());
    let hit = |v: &str| {
        let w = words(v);
        !w.is_empty() && w.iter().all(|x| q.contains(x))
    };
    // signature -> (options with pass/fail counts and newest outcome, trials, newest seq)
    let mut groups: Vec<(Vec<(String, String)>, Vec<(String, u32, u32, bool)>, usize, u64)> = vec![];
    for t in dedupe(trials) {
        let mut sig: Vec<(String, String)> = t.fields.0.iter().filter(|(_, v)| hit(v)).cloned().collect();
        if sig.is_empty() {
            continue;
        }
        sig.sort_by_key(|(n, _)| vocab.position(n).unwrap_or(usize::MAX));
        let i = match groups.iter().position(|g| g.0 == sig) {
            Some(i) => i,
            None => {
                groups.push((sig, vec![], 0, 0));
                groups.len() - 1
            }
        };
        let g = &mut groups[i];
        g.2 += 1;
        g.3 = g.3.max(t.seq);
        let o = option(t);
        let e = match g.1.iter().position(|x| x.0 == o) {
            Some(j) => &mut g.1[j],
            None => {
                g.1.push((o, 0, 0, false));
                g.1.last_mut().unwrap()
            }
        };
        if t.passed() {
            e.1 += 1;
        } else {
            e.2 += 1;
        }
        e.3 = t.passed();
    }
    let best = groups.iter().map(|g| g.0.len()).max().unwrap_or(0);
    if best >= 2 {
        groups.retain(|g| g.0.len() >= 2);
    }
    groups.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(b.2.cmp(&a.2)).then(b.3.cmp(&a.3)));
    groups
        .into_iter()
        .take(limit)
        .map(|(sig, mut opts, _, _)| {
            opts.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)).then(b.1.cmp(&a.1)));
            let vals = sig.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join(" ");
            let outcomes: Vec<String> = opts
                .iter()
                .map(|(o, p, f, newest)| match (p, f) {
                    (0, f) => format!("{o} FAIL {f}x"),
                    (p, 0) => format!("{o} PASS {p}x"),
                    (p, f) => format!("{o} PASS {p}x FAIL {f}x (newest {})", if *newest { "PASS" } else { "FAIL" }),
                })
                .collect();
            format!("[{vals}] {}", outcomes.join(", "))
        })
        .collect()
}

/// The case types of the trials, most recent first (see [`analyze`]).
pub fn summarize(trials: &[Trial]) -> Vec<TypeSummary> {
    analyze(trials).types
}

/// One type's summary with its exceptions: each round finds the option that passed and failed
/// and the one stored field that separates its outcomes, sets those trials apart as exceptions
/// and summarises the rest.
fn summarize_type(
    ty: &str,
    ts: &[&Trial],
    option: &dyn Fn(&Trial) -> String,
    vocab: &Vocabulary,
    learned: &Learned,
    kind_names: &HashMap<(String, String), Vec<String>>,
) -> TypeSummary {
    let kind = ts.first().and_then(|t| vocab.anchor(&t.fields));
    let key: Vec<String> = match kind {
        Some(kind) => std::iter::once(kind.0.clone()).chain(learned.fields_of(kind).iter().map(|f| f.field.clone())).collect(),
        None => vec![],
    };
    let names: Vec<String> = kind.and_then(|k| kind_names.get(k)).cloned().unwrap_or_default();
    let tally = |ty: &str, ts: &[&Trial], option: &dyn Fn(&Trial) -> String| tally(ty, ts, option, vocab, &key, &names);
    let mut rest: Vec<&Trial> = ts.to_vec();
    let mut exceptions: Vec<Exception> = vec![];
    for _ in 0..EXCEPTION_ROUNDS {
        let base = tally(ty, &rest, option);
        let found = base.mixed.iter().find_map(|o| separating_field(&rest, &o.option, option, vocab).map(|(f, vals)| (o.option.clone(), f, vals)));
        let Some((opt, field, values)) = found else { break };
        for v in &values {
            let subset: Vec<&Trial> = rest.iter().copied().filter(|t| t.fields.get(&field) == Some(v.as_str())).collect();
            let fails = subset.iter().filter(|t| !t.passed() && option(t) == opt).count() as u32;
            exceptions.push(Exception { field: field.clone(), value: v.clone(), option: opt.clone(), fails, summary: Box::new(tally(ty, &subset, option)) });
        }
        rest.retain(|t| !t.fields.get(&field).is_some_and(|x| values.iter().any(|v| v == x)));
    }
    exceptions.sort_by(|a, b| b.fails.cmp(&a.fails).then(b.summary.last_seq.cmp(&a.summary.last_seq)));
    let mut s = tally(ty, &rest, option);
    s.exceptions = exceptions;
    s.trials = ts.len();
    s.last_seq = ts.iter().map(|t| t.seq).max().unwrap_or(0);
    s.fields = ts.last().map(|t| t.fields.clone()).unwrap_or_default();
    let parts: HashSet<&str> = ty.split('/').collect();
    let mut values: Vec<String> = ts.iter().flat_map(|t| t.fields.0.iter().map(|(_, v)| v.clone())).filter(|v| !parts.contains(v.as_str())).collect();
    values.sort();
    values.dedup();
    s.values = values;
    s
}

/// The name-like field whose values separate the outcomes of `opt` within `ts`: every failure has
/// a value, those values never passed, and the option passed for another value. The fewest failing
/// values win, then key order. `None` when no name field does (other fields that matter become key
/// fields instead, see [`learn`]).
fn separating_field(ts: &[&Trial], opt: &str, option: &dyn Fn(&Trial) -> String, vocab: &Vocabulary) -> Option<(String, Vec<String>)> {
    let of: Vec<&Trial> = ts.iter().copied().filter(|t| option(t) == opt).collect();
    let (fails, passes): (Vec<&Trial>, Vec<&Trial>) = of.iter().copied().partition(|t| !t.passed());
    if fails.is_empty() || passes.is_empty() {
        return None;
    }
    let mut names: Vec<&str> = vec![];
    for t in &of {
        for n in t.fields.names() {
            if !names.contains(&n) && vocab.name_like(n) {
                names.push(n);
            }
        }
    }
    let mut best: Option<(usize, bool, usize, String, Vec<String>)> = None;
    for name in names {
        if fails.iter().any(|t| t.fields.get(name).is_none()) {
            continue;
        }
        let mut failing: Vec<String> = fails.iter().filter_map(|t| t.fields.get(name)).map(str::to_string).collect();
        failing.sort();
        failing.dedup();
        let pass_values: Vec<&str> = passes.iter().filter_map(|t| t.fields.get(name)).collect();
        if pass_values.is_empty() || pass_values.iter().any(|v| failing.iter().any(|f| f == v)) {
            continue;
        }
        let rank = (failing.len(), vocab.name_like(name), vocab.position(name).unwrap_or(usize::MAX));
        if best.as_ref().is_none_or(|b| rank < (b.0, b.1, b.2)) {
            best = Some((rank.0, rank.1, rank.2, name.to_string(), failing));
        }
    }
    best.map(|b| (b.3, b.4))
}

/// Working, failed and mixed options of a set of trials of one type.
fn tally(ty: &str, ts: &[&Trial], option: &dyn Fn(&Trial) -> String, vocab: &Vocabulary, key: &[String], names: &[String]) -> TypeSummary {
    let mut opts: BTreeMap<String, OptionStats> = BTreeMap::new();
    for t in ts {
        let label = option(t);
        let o = opts.entry(label.clone()).or_insert_with(|| OptionStats { option: label, ..Default::default() });
        if t.passed() {
            o.passes += 1;
            o.last_pass = Some(t.seq);
            o.last_pass_at = Some(t.at);
            o.passes_since_fail += 1;
            o.fails_since_pass = 0;
        } else {
            o.fails += 1;
            o.last_fail = Some(t.seq);
            o.last_fail_at = Some(t.at);
            o.fails_since_pass += 1;
            o.passes_since_fail = 0;
        }
    }
    for o in opts.values_mut() {
        let of = |pass: bool| -> Vec<&CaseFields> { ts.iter().filter(|t| t.passed() == pass && option(t) == o.option).map(|t| &t.fields).collect() };
        o.pass_ev = evidence(&of(true), vocab, key, Some(names));
        o.fail_ev = evidence(&of(false), vocab, key, None);
    }
    let mut working: Vec<OptionStats> = opts.values().filter(|o| o.working()).cloned().collect();
    working.sort_by(|a, b| b.last_pass.cmp(&a.last_pass));
    let mut failed: Vec<OptionStats> = opts.values().filter(|o| o.failing()).cloned().collect();
    failed.sort_by(|a, b| b.last_fail.cmp(&a.last_fail));
    let mut mixed: Vec<OptionStats> = opts.values().filter(|o| o.passes > 0 && o.fails > 0).cloned().collect();
    mixed.sort_by(|a, b| (b.passes + b.fails).cmp(&(a.passes + a.fails)).then(b.last_pass.max(b.last_fail).cmp(&a.last_pass.max(a.last_fail))));
    TypeSummary {
        case_type: ty.to_string(),
        fields: ts.last().map(|t| t.fields.clone()).unwrap_or_default(),
        last_seq: ts.iter().map(|t| t.seq).max().unwrap_or(0),
        trials: ts.len(),
        fails: ts.iter().filter(|t| !t.passed()).count() as u32,
        working,
        failed,
        mixed,
        exceptions: vec![],
        values: vec![],
    }
}

fn hm(t: Option<DateTime<Utc>>) -> String {
    t.map(|t| t.format("%H:%M UTC").to_string()).unwrap_or_else(|| "?".into())
}

/// One option's claim at the granularity of its evidence: `WORKING x (2 pass, last at T; a=b)` when
/// every case it passed for had the same values, else `x passed 3x (last at T; a=b 2x|c 1x)`; its
/// failures follow with their values: `, failed 1x (a=d)`.
fn option_claim(o: &OptionStats) -> String {
    let ev = |e: &Evidence| if e.text.is_empty() { String::new() } else { format!("; {}", e.text) };
    let mut c = if o.pass_ev.homogeneous {
        format!("WORKING {} ({} pass, last at {}{})", o.option, o.passes_since_fail, hm(o.last_pass_at), ev(&o.pass_ev))
    } else {
        format!("{} passed {}x (last at {}{})", o.option, o.passes, hm(o.last_pass_at), ev(&o.pass_ev))
    };
    if o.fails > 0 {
        let fe = if o.fail_ev.text.is_empty() { String::new() } else { format!(" ({})", o.fail_ev.text) };
        c.push_str(&format!(", failed {}x{fe}", o.fails));
    }
    c
}

/// The line for one case type: its own line (below) and its exceptions,
/// `— except enemy=troll: failed fight 2x; WORKING there sneak (1 pass)` (the option failed there
/// once: `— possible exception enemy=troll: failed fight 1x`).
pub fn type_line(s: &TypeSummary) -> String {
    let mut line = base_line(s);
    for e in s.exceptions.iter().take(MAX_EXCEPTIONS) {
        line.push_str(" — ");
        line.push_str(&exception_text(e));
    }
    if s.exceptions.len() > MAX_EXCEPTIONS {
        line.push_str(&format!(" — +{} more exceptions", s.exceptions.len() - MAX_EXCEPTIONS));
    }
    line
}

/// `except enemy=troll: failed fight 2x; WORKING there sneak (1 pass)`.
pub fn exception_text(e: &Exception) -> String {
    let head = if e.is_rule() { format!("except {}={}", e.field, e.value) } else { format!("possible exception {}={}", e.field, e.value) };
    let failed: Vec<String> = e.summary.failed.iter().take(3).map(|o| format!("{} {}x", o.option, o.fails)).collect();
    let mut text = format!("{head}: failed {}", failed.join(", "));
    if let Some(w) = e.summary.working.first() {
        text.push_str(&format!("; WORKING there {} ({} pass)", w.option, w.passes_since_fail));
    }
    text
}

/// A type's own line, without exceptions. Every claim names the values of the cases behind it
/// (fields outside the key, see [`option_claim`]), so a reader checks whether its own case was
/// among them:
/// - an option whose last outcome passed: `a/b: WORKING x (2 pass, last at 10:42 UTC; c=d)` when the
///   cases it passed for share their values, else `a/b: x passed 3x (last at ...; c=d 2x|e 1x)`, with
///   its own failures after it; no other options' failures, no advice to try other options;
/// - a working option that later failed: `...: changed? approve (3 pass, then failed 1x, last at ...) | failed: ... — no working rule yet`;
/// - failures and no working option: `...: failed: approve, escalate/x — no working rule yet`, plus after
///   [`DEFAULT_FAILED_AT`] failures `; the default action failed for this type: check the reason codes`.
fn base_line(s: &TypeSummary) -> String {
    if !s.working.is_empty() {
        return working_line(s);
    }
    let failed_text = failed_text(s);
    let mut line = match s.failed.iter().find(|o| o.changed()) {
        Some(c) => format!(
            "{}: changed? {} ({} pass{}, then failed {}x, last at {}) | failed: {} — no working rule yet",
            s.case_type,
            c.option,
            c.passes,
            if c.pass_ev.text.is_empty() { String::new() } else { format!(" [{}]", c.pass_ev.text) },
            c.fails_since_pass,
            hm(c.last_fail_at),
            failed_text
        ),
        None => format!("{}: failed: {} — no working rule yet", s.case_type, failed_text),
    };
    if s.fails >= DEFAULT_FAILED_AT {
        line.push_str("; the default action failed for this type: check the reason codes");
    }
    line
}

fn working_line(s: &TypeSummary) -> String {
    let mut line = format!("{}: {}", s.case_type, option_claim(&s.working[0]));
    if s.working.len() > 1 {
        let also: Vec<String> = s.working.iter().skip(1).take(2).map(option_claim).collect();
        line.push_str(&format!(
            "; also {} (more than one option passes: compare the values each passed for with your case's; an attribute outside the type, such as an amount, may decide)",
            also.join("; also ")
        ));
    }
    line
}

fn failed_text(s: &TypeSummary) -> String {
    let failed: Vec<String> = s
        .failed
        .iter()
        .take(5)
        .map(|o| {
            let n = if o.fails > 1 { format!(" ({}x)", o.fails) } else { String::new() };
            let e = if o.fail_ev.text.is_empty() { String::new() } else { format!(" [{}]", o.fail_ev.text) };
            format!("{}{n}{e}", o.option)
        })
        .collect();
    let more = s.failed.len().saturating_sub(5);
    let mut text = failed.join(", ");
    if more > 0 {
        text.push_str(&format!(", +{more} more"));
    }
    text
}

/// The split hint of a type whose same option both passed and failed: `mixed: approve passed 4x
/// and failed 2x — cases of this type may differ in a field not in the key; add the field that
/// separates them (fields others used: a, b)`. `None` when no option is mixed.
pub fn split_hint(s: &TypeSummary, vocab: &Vocabulary) -> Option<String> {
    if s.mixed.is_empty() {
        return None;
    }
    let what: Vec<String> = s.mixed.iter().take(2).map(|o| format!("{} passed {}x and failed {}x", o.option, o.passes, o.fails)).collect();
    // Names not in the type. Prefer those given alongside the type's first field value (the most
    // similar cases), else those given alongside the type's field names, else any; most used first.
    let missing: Vec<&FieldUse> = vocab.fields.iter().filter(|f| s.fields.get(&f.name).is_none()).collect();
    let near = s.fields.0.first().and_then(|p| vocab.with.get(p));
    let by_value = |f: &FieldUse| near.and_then(|m| m.get(&f.name)).copied().unwrap_or(0);
    let by_name = |f: &FieldUse| -> usize {
        vocab.with.iter().filter(|((n, _), _)| s.fields.get(n).is_some()).filter_map(|(_, m)| m.get(&f.name)).sum()
    };
    let mut others: Vec<&FieldUse> = if missing.iter().any(|f| by_value(f) > 0) {
        missing.into_iter().filter(|f| by_value(f) > 0).collect()
    } else if missing.iter().any(|f| by_name(f) > 0) {
        missing.into_iter().filter(|f| by_name(f) > 0).collect()
    } else {
        missing
    };
    others.sort_by(|a, b| by_value(b).cmp(&by_value(a)).then(by_name(b).cmp(&by_name(a))).then(b.trials.cmp(&a.trials))); // stable: ties keep key order
    let others: Vec<&str> = others.iter().take(SPLIT_FIELDS).map(|f| f.name.as_str()).collect();
    let used = if others.is_empty() { String::new() } else { format!(" (fields others used: {})", others.join(", ")) };
    Some(format!(
        "mixed: {} — cases of this type may differ in a field not in the key; add the field that separates them{used}",
        what.join(", ")
    ))
}

/// A type's entry in a listing: its line and, below it, the split hint when it has one.
pub fn type_entry(s: &TypeSummary, vocab: &Vocabulary) -> String {
    match split_hint(s, vocab) {
        Some(h) => format!("{}\n  - {h}", type_line(s)),
        None => type_line(s),
    }
}

/// The entry of one case type, given all trials (`ohmygod tried` output; `case_type` is the key
/// under the current trials, see [`Analysis::key_of`]).
pub fn line_of(trials: &[Trial], case_type: &str) -> Option<String> {
    let a = analyze(trials);
    a.type_of(case_type).map(|s| type_entry(s, &a.vocab))
}

/// Kinds the learned-key block lists at most.
pub const KIND_LINES: usize = 8;

/// "Key = ..." sentence for a listing.
fn key_sentence(a: &Analysis, trials: &[Trial]) -> String {
    if a.vocab.is_empty() {
        return "Key = the case's field values joined by /.".into();
    }
    let kinds = a.vocab.anchor_names(trials);
    let names = a.vocab.name_fields();
    let kept = if names.is_empty() {
        String::new()
    } else {
        format!(" Name fields ({}) never join the key: when outcomes differ by a name, the type line shows an exception.", names.join(", "))
    };
    format!(
        "Key = the case's kind ({}), then the key fields learned for that kind, in the order learned (`?` = not given).{kept}",
        kinds.join(", ")
    )
}

/// The "Case types" block agents see in `join`: the field and action vocabularies, the key fields
/// learned per kind, then one line per type, most recent first. Empty when nobody recorded a trial.
pub fn case_types_block(trials: &[Trial], limit: usize) -> String {
    block(trials, limit, true)
}

/// The "Case types" block for the moderator's prompt: the learned key fields and the same lines,
/// without the vocabularies.
pub fn case_types_block_plain(trials: &[Trial], limit: usize) -> String {
    block(trials, limit, false)
}

fn block(trials: &[Trial], limit: usize, agents: bool) -> String {
    if trials.is_empty() {
        return String::new();
    }
    let a = analyze(trials);
    let mut out = String::from("\n## Case types (graded trials agents recorded with `ohmygod tried`, most recent first)\n\n");
    if agents && !a.vocab.is_empty() {
        out.push_str(&a.vocab.line());
        out.push('\n');
    }
    if agents {
        out.push_str(&a.actions.line());
        out.push('\n');
    }
    out.push_str(&a.learned_block(trials, KIND_LINES));
    out.push_str(&format!(
        "{} Find your case's type and compare the values each option passed or failed for with your case's: a WORKING option passed for exactly the values shown and has not failed since; one that passed for other values is no rule for yours. Types are separate: never carry a result to another type.\n\n",
        key_sentence(&a, trials)
    ));
    for s in a.types.iter().take(limit) {
        out.push_str(&format!("- {}\n", type_entry(s, &a.vocab)));
    }
    if a.types.len() > limit {
        out.push_str(&format!("- … and {} more case types: `ohmygod search <type words>` finds them.\n", a.types.len() - limit));
    }
    out
}

/// The header of matching type lines in search results.
pub fn matching_header(trials: &[Trial]) -> String {
    let a = analyze(trials);
    let kinds = a.vocab.anchor_names(trials);
    let order = if a.vocab.is_empty() {
        "the case's field values".to_string()
    } else {
        format!("the kind ({}), then the key fields learned for it; exceptions by name follow a type's line", kinds.join(", "))
    };
    format!("Case types matching your search (graded trials agents recorded; key = {order}; a WORKING option passed for that type and has not failed since):\n")
}

/// Lowercase alphanumeric words, a plural `s` dropped (`cards` matches `card`).
fn words(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| if w.len() > 3 && w.ends_with('s') { w[..w.len() - 1].to_string() } else { w.to_string() })
        .collect()
}

/// Type lines that match the words of `query`, best match first. A type part, or a stored value of
/// the type's trials (a name), matches when all its words appear in the query. When some type matches on two or more parts, types that match
/// on only one part are left out.
pub fn matching_lines(trials: &[Trial], query: &str, limit: usize) -> Vec<String> {
    matching(trials, query, limit, true)
}

/// Type lines whose key matches the words of a thread's title, for the thread document the
/// moderator reviews: matched by key parts only and without the split hint, so documents stay
/// short and more threads fit in one review.
pub fn matching_lines_brief(trials: &[Trial], query: &str, limit: usize) -> Vec<String> {
    matching(trials, query, limit, false)
}

fn matching(trials: &[Trial], query: &str, limit: usize, full: bool) -> Vec<String> {
    let q = words(query);
    if q.is_empty() || trials.is_empty() {
        return vec![];
    }
    let a = analyze(trials);
    let mut scored: Vec<(usize, bool, &TypeSummary)> = a
        .types
        .iter()
        .filter_map(|s| {
            let parts: Vec<&str> = s.case_type.split('/').collect();
            let hit = |p: &str| {
                let w = words(p);
                !w.is_empty() && w.iter().all(|x| q.contains(x))
            };
            let key_hits = parts.iter().filter(|p| hit(p)).count();
            let hits = key_hits + if full { s.values.iter().filter(|v| hit(v)).count() } else { 0 };
            (hits > 0).then_some((hits, key_hits == parts.len(), s))
        })
        .collect();
    let best = scored.iter().map(|x| x.0).max().unwrap_or(0);
    if best >= 2 {
        scored.retain(|x| x.0 >= 2);
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.last_seq.cmp(&a.2.last_seq)));
    scored.truncate(limit);
    scored.into_iter().map(|x| if full { type_entry(x.2, &a.vocab) } else { type_line(x.2) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn s(x: &str) -> String {
        x.to_string()
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items.iter().map(|(n, v)| (s(n), s(v))).collect()
    }

    /// A trial recorded through [`prepare`] on top of `prior`.
    fn record(prior: &mut Vec<Trial>, agent: &str, fields: &[(&str, &str)], action: &str, reason: Option<&str>, score: u32, case: Option<&str>) -> Prepared {
        let p = prepare(&pairs(fields), &[], prior).expect("fields");
        let seq = prior.len() as u64 + 1;
        prior.push(Trial {
            seq,
            at: Utc.with_ymd_and_hms(2026, 11, 9, 10, seq as u32 % 60, 0).unwrap(),
            agent_id: agent.into(),
            case_type: p.fields.key(),
            fields: p.fields.clone(),
            action: action_word(action).unwrap(),
            action_text: None,
            reason: reason.and_then(normalize_code),
            score,
            case_id: case.map(str::to_string),
        });
        p
    }

    #[test]
    fn names_and_values_are_canonical_snake_case() {
        assert_eq!(canonical_name("Enemy Type"), Some(s("enemy_type")));
        assert_eq!(canonical_name("  LEVEL-name "), Some(s("level_name")));
        assert_eq!(canonical_name(" -- "), None);
        assert_eq!(canonical_value("Dark Forest"), Ok(s("dark_forest")));
        assert_eq!(canonical_value("dark-forest"), Ok(s("dark_forest")));
        assert_eq!(canonical_value("TROLL (boss, armoured)"), Ok(s("troll")));
        assert_eq!(canonical_value("Merge conflicts"), Ok(s("merge_conflicts")));
        // Digits: amounts, dates and IDs are dropped, a bracketed aside with digits is not the value.
        assert_eq!(canonical_value("598.72"), Err(Dropped::Digits));
        assert_eq!(canonical_value("2026-11-09"), Err(Dropped::Digits));
        assert_eq!(canonical_value("PAY-106"), Err(Dropped::Digits));
        assert_eq!(canonical_value("level 3"), Err(Dropped::Digits));
        assert_eq!(canonical_value("basic (7 years)"), Ok(s("basic")));
        // No value at all.
        assert_eq!(canonical_value("n/a"), Err(Dropped::Empty));
        assert_eq!(canonical_value("(none)"), Err(Dropped::Empty));
        assert_eq!(canonical_value("  "), Err(Dropped::Empty));
        // Long values are cut.
        assert_eq!(canonical_value(&"x".repeat(80)).unwrap().len(), MAX_PART_CHARS);
    }

    #[test]
    fn field_arguments_parse_pairs_and_continuations() {
        let (p, bad) = parse_field_args(&[s("level=forest"), s("enemy = cave troll"), s("a=x, b=y;c=z"), s("orphan")]);
        assert_eq!(p, pairs(&[("level", "forest"), ("enemy", "cave troll"), ("a", "x"), ("b", "y"), ("c", "z")]));
        assert_eq!(bad, vec![s("orphan")]);
        let (p, _) = field_args(Some(&serde_json::json!({"level": "forest", "enemy": "troll"})));
        assert_eq!(p, pairs(&[("level", "forest"), ("enemy", "troll")]));
        let (p, _) = field_args(Some(&serde_json::json!(["level=forest", ["enemy", "troll"]])));
        assert_eq!(p, pairs(&[("level", "forest"), ("enemy", "troll")]));
        let (p, _) = field_args(Some(&serde_json::json!("level=forest enemy=troll")));
        assert_eq!(p.len(), 2);
        assert_eq!(field_args(None), (vec![], vec![]));
    }

    #[test]
    fn digits_are_dropped_and_the_key_follows_the_established_order() {
        let mut t = vec![];
        let p = record(&mut t, "A1", &[("Level", "Forest"), ("Enemy", "Troll"), ("gold", "120"), ("seen at", "2026-11-09")], "retreat", None, 0, Some("G1"));
        assert_eq!(p.fields.key(), "forest/troll");
        assert!(p.notes[0].starts_with("Dropped `gold=120`, `seen_at=2026-11-09`: a value with digits"), "{:?}", p.notes);
        // Another agent gives the same fields in another order: same key.
        let p = record(&mut t, "A2", &[("enemy", "troll"), ("level", "forest")], "fight", Some("use fire"), 100, Some("G2"));
        assert_eq!(p.fields.key(), "forest/troll");
        assert_eq!(p.fields.pairs_text(), "level=forest enemy=troll");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        // A new name goes after the established ones.
        let p = record(&mut t, "A3", &[("weather", "rain"), ("enemy", "troll"), ("level", "swamp")], "fight", Some("use_fire"), 100, None);
        assert_eq!(p.fields.key(), "swamp/troll/rain");
        // Nothing usable: the error lists the names in use.
        let e = prepare(&pairs(&[("gold", "120"), ("hp", "none")]), &[], &t).unwrap_err();
        assert!(e.starts_with("No case-type field left to record. Dropped `gold=120`"), "{e}");
        assert!(e.contains("Case-type fields in use, reuse these names: level (3), enemy (3), weather (1); e.g. level=forest enemy=troll."), "{e}");
        let e = prepare(&[], &[s("forest")], &[]).unwrap_err();
        assert!(e.contains("Could not read `forest`") && e.contains("--field <name>=<value>") && !e.contains("in use"), "{e}");
        // At most eight fields, established names first.
        let many = [("a", "1x"), ("b", "q"), ("c", "r"), ("d", "s"), ("e", "t"), ("f", "u"), ("g", "v"), ("h", "w"), ("i", "x"), ("level", "cave")];
        let p = prepare(&pairs(&many), &[], &t).unwrap();
        assert_eq!(p.fields.pairs_text(), "level=cave b=q c=r d=s e=t f=u g=v h=w");
        assert_eq!(p.fields.len(), MAX_FIELDS);
        assert!(p.notes.iter().any(|n| n == "A trial keeps at most 8 fields; dropped `i=x`."), "{:?}", p.notes);
        let eight = prepare(&pairs(&many[1..9]), &[], &[]).unwrap();
        assert_eq!(eight.fields.key(), "q/r/s/t/u/v/w/x");
        assert!(eight.notes.is_empty(), "{:?}", eight.notes);
    }

    #[test]
    fn names_match_only_by_exact_canonical_form_and_replies_point_at_the_names_in_use() {
        let mut t = vec![];
        for (i, v) in ["merge_conflict", "merge_conflict", "detached_head"].iter().enumerate() {
            record(&mut t, "A1", &[("tool", "git"), ("symptom", v), ("os", "linux")], "rebase", None, 100, Some(&format!("C{i}")));
        }
        // `Symptom ` and `TOOL` are the established names; no synonym tables.
        let p = prepare(&pairs(&[("TOOL", "Git"), ("Symptom ", "Merge Conflict"), ("os", "linux")]), &[], &t).unwrap();
        assert_eq!(p.fields.key(), "git/merge_conflict/linux");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        // A different name is a different field: recorded, with the names in use in the reply.
        let p = prepare(&pairs(&[("problem", "merge_conflict"), ("tool", "git")]), &[], &t).unwrap();
        assert_eq!(p.fields.pairs_text(), "tool=git problem=merge_conflict");
        assert!(
            p.notes.iter().any(|n| n == "New field name(s) `problem`. Names in use: tool (3), symptom (3), os (3). Reuse them when your case has those fields, so that trials of one kind of case group together."),
            "{:?}",
            p.notes
        );
        // A look-alike name or value gets a pointer to the established one, never a rewrite.
        let p = prepare(&pairs(&[("symptoms", "merge_conflict"), ("tool", "git")]), &[], &t).unwrap();
        assert_eq!(p.fields.pairs_text(), "tool=git symptoms=merge_conflict");
        assert!(p.notes.iter().any(|n| n.starts_with("`symptoms` is a new field name; the board has `symptom` (3 trials)")), "{:?}", p.notes);
        let p = prepare(&pairs(&[("tool", "git"), ("symptom", "mergeconflicts")]), &[], &t).unwrap();
        assert_eq!(p.fields.key(), "git/mergeconflicts");
        assert!(
            p.notes.iter().any(|n| n.starts_with("`symptom=mergeconflicts` is a new value; the board has `symptom=merge_conflict` (2 trials)")
                && n.contains("Values in use: merge_conflict (2), detached_head (1).")),
            "{:?}",
            p.notes
        );
        let p = prepare(&pairs(&[("tool", "git"), ("symptom", "detached")]), &[], &t).unwrap();
        assert!(p.notes.iter().any(|n| n.contains("the board has `symptom=detached_head`")), "{:?}", p.notes);
        // A plainly new value needs no note.
        let p = prepare(&pairs(&[("tool", "git"), ("symptom", "lost_stash")]), &[], &t).unwrap();
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        // One field alone gets a nudge.
        let p = prepare(&pairs(&[("tool", "git")]), &[], &t).unwrap();
        assert!(p.notes.iter().any(|n| n.starts_with("One field pools")), "{:?}", p.notes);
    }

    #[test]
    fn vocabulary_lists_names_in_key_order_with_counts_and_an_example() {
        let mut t = vec![];
        record(&mut t, "A1", &[("level", "forest"), ("enemy", "troll")], "fight", None, 100, None);
        record(&mut t, "A1", &[("level", "cave"), ("enemy", "bat"), ("weather", "rain")], "fight", None, 100, None);
        record(&mut t, "A1", &[("level", "cave"), ("enemy", "bat")], "fight", None, 0, None);
        let v = Vocabulary::from_trials(&t);
        assert_eq!(v.names_text(), "level (3), enemy (3), weather (1)");
        assert_eq!(v.line(), "Case-type fields in use, reuse these names: level (3), enemy (3), weather (1); e.g. level=cave enemy=bat");
        assert_eq!(v.get("enemy").unwrap().values, vec![(s("bat"), 2), (s("troll"), 1)]);
        // Many rarely used names: the most used are listed, in key order.
        for i in 0..6 {
            let name = format!("extra{}", ["a", "b", "c", "d", "e", "f"][i]);
            record(&mut t, "A1", &[("level", "cave"), (&name, "x")], "fight", None, 100, None);
        }
        let v = Vocabulary::from_trials(&t);
        assert_eq!(v.names_text(), "level (9), enemy (3), weather (1), extraa (1), extrab (1), extrac (1), extrad (1), extrae (1), +1 rarely used");
        assert!(Vocabulary::from_trials(&[]).line().is_empty());
    }

    #[test]
    fn working_type_shows_only_the_working_option() {
        let ty: &[(&str, &str)] = &[("tool", "git"), ("symptom", "merge_conflict"), ("os", "linux")];
        let mut t = vec![];
        record(&mut t, "A1", ty, "reset", None, 0, Some("C1"));
        record(&mut t, "A2", ty, "rebase", Some("onto_main"), 0, Some("C2"));
        let two = t.clone();
        record(&mut t, "A3", &[("tool", "npm"), ("symptom", "peer_dependency"), ("os", "linux")], "install", None, 100, Some("C3"));
        record(&mut t, "A1", ty, "merge", Some("ours"), 100, Some("C4"));
        record(&mut t, "A2", ty, "merge", Some("ours"), 100, Some("C5"));
        let all = summarize(&t);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].case_type, "git");
        assert_eq!(type_line(&all[0]), "git: WORKING merge/ours (2 pass, last at 10:05 UTC; symptom=merge_conflict, os=linux)");
        // Failures and no working option: list them; after two failures point at the reason codes.
        assert_eq!(type_line(&summarize(&two[..1])[0]), "git: failed: reset [symptom=merge_conflict, os=linux] — no working rule yet");
        assert_eq!(
            type_line(&summarize(&two)[0]),
            "git: failed: rebase/onto_main [symptom=merge_conflict, os=linux], reset [symptom=merge_conflict, os=linux] — no working rule yet; the default action failed for this type: check the reason codes"
        );
        // Two options both pass: an attribute outside the type decides.
        let mut mixed = vec![];
        record(&mut mixed, "A1", &[("level", "forest"), ("enemy", "troll")], "fight", None, 100, None);
        record(&mut mixed, "A2", &[("level", "forest"), ("enemy", "troll")], "sneak", Some("night"), 100, None);
        let line = type_line(&summarize(&mixed)[0]);
        assert!(line.starts_with("forest: WORKING sneak/night (1 pass") && line.contains("also WORKING fight (1 pass"), "{line}");
        assert!(line.contains("an attribute outside the type"), "{line}");
    }

    #[test]
    fn a_working_option_that_later_fails_is_marked_changed() {
        let ty: &[(&str, &str)] = &[("level", "forest"), ("enemy", "troll")];
        let mut t = vec![];
        record(&mut t, "A1", ty, "fight", Some("fire"), 100, None);
        record(&mut t, "A2", ty, "fight", Some("fire"), 100, None);
        record(&mut t, "A3", ty, "fight", Some("fire"), 0, None);
        let s = &summarize(&t)[0];
        assert!(s.working.is_empty() && s.changed());
        assert_eq!(type_line(s), "forest: changed? fight/fire (2 pass [enemy=troll], then failed 1x, last at 10:03 UTC) | failed: fight/fire [enemy=troll] — no working rule yet");
        // Passed and failed: the entry also carries the split hint (no other fields in use yet).
        assert_eq!(
            type_entry(s, &Vocabulary::from_trials(&t)),
            format!("{}\n  - mixed: fight/fire passed 2x and failed 1x — cases of this type may differ in a field not in the key; add the field that separates them", type_line(s))
        );
        // A newer pass of another option is the working one; only it is shown.
        record(&mut t, "A4", ty, "sneak", None, 100, None);
        let line = type_line(&summarize(&t)[0]);
        assert_eq!(line, "forest: WORKING sneak (1 pass, last at 10:04 UTC; enemy=troll)");
    }

    #[test]
    fn same_agent_same_case_keeps_the_newest_record() {
        let ty: &[(&str, &str)] = &[("level", "forest"), ("enemy", "troll")];
        let mut t = vec![];
        record(&mut t, "A1", ty, "fight", None, 0, Some("G9"));
        record(&mut t, "A1", ty, "fight", None, 100, Some("G9"));
        let s = &summarize(&t)[0];
        assert_eq!(s.trials, 1);
        assert!(s.failed.is_empty() && !s.working.is_empty());
    }

    #[test]
    fn listing_and_search_show_types_with_the_vocabulary() {
        let mut t = vec![];
        record(&mut t, "A1", &[("level", "forest"), ("enemy", "troll")], "fight", None, 0, None);
        record(&mut t, "A1", &[("level", "cave"), ("enemy", "bat")], "fight", None, 100, None);
        record(&mut t, "A1", &[("level", "swamp"), ("enemy", "troll")], "sneak", None, 100, None);
        let hits = matching_lines(&t, "forest trolls", 6);
        assert_eq!(hits, vec![s("forest: failed: fight [enemy=troll] — no working rule yet")]);
        assert_eq!(matching_lines(&t, "troll", 6).len(), 2);
        assert!(matching_lines(&t, "refund timeout", 6).is_empty());
        let block = case_types_block(&t, 2);
        assert!(block.contains("## Case types"), "{block}");
        assert!(
            block.contains("Case-type fields in use, reuse these names: level (3), enemy (3); e.g. level=swamp enemy=troll\nActions in use: fight (2), sneak (1)\nKey fields per kind, learned from outcomes"),
            "{block}"
        );
        assert!(block.contains("… and 1 more case types"), "{block}");
        assert!(block.contains("- no key field yet (key = the kind alone): swamp, cave, forest\n"), "{block}");
        assert!(block.contains("Key = the case's kind (level), then the key fields learned for that kind"), "{block}");
        assert!(block.find("- swamp: WORKING").unwrap() < block.find("- cave: WORKING").unwrap(), "most recent first: {block}");
        let plain = case_types_block_plain(&t, 12);
        assert!(!plain.contains("reuse these names") && !plain.contains("Actions in use") && plain.contains("- forest: failed: fight"), "{plain}");
        assert_eq!(case_types_block(&[], 12), "");
        assert!(matching_header(&t).contains("key = the kind (level), then the key fields learned for it"));
    }

    #[test]
    fn actions_count_by_their_first_word_and_a_generic_stem() {
        let words: Vec<Option<String>> = [
            "approved_per_written_policy",
            "approved",
            "approve",
            "approved_slightly_over_written_500_limit",
            "denied",
            "escalated",
            "deny",
            "escalate",
            "Approved; under threshold, no flag",
            "Escalated (over limit)",
            " -- ",
        ]
        .iter()
        .map(|a| action_word(a))
        .collect();
        let w = |x: &str| Some(s(x));
        assert_eq!(words, vec![w("approved"), w("approved"), w("approve"), w("approved"), w("denied"), w("escalated"), w("deny"), w("escalate"), w("approved"), w("escalated"), None]);
        for (a, b) in [("approve", "approved"), ("deny", "denied"), ("escalate", "escalated"), ("retry", "retried"), ("fight", "fight")] {
            assert_eq!(action_stem(a), action_stem(b), "{a} {b}");
        }
        assert_ne!(action_stem("approve"), action_stem("deny"));
        assert_eq!(action_stem("bed"), "bed"); // never cut below three characters
        // Options group by stem and show the most common spelling; the reason stays separate.
        let ty: &[(&str, &str)] = &[("level", "forest"), ("enemy", "troll")];
        let mut t = vec![];
        for (i, a) in ["approved per written policy", "approved", "approve", "approved slightly over 500"].iter().enumerate() {
            record(&mut t, "A1", ty, a, None, 100, Some(&format!("C{i}")));
        }
        record(&mut t, "A1", ty, "denied", Some("too_far"), 0, Some("C8"));
        record(&mut t, "A1", ty, "deny", Some("too_far"), 0, Some("C9"));
        let all = summarize(&t);
        assert_eq!(type_line(&all[0]), "forest: WORKING approved (4 pass, last at 10:04 UTC; enemy=troll)");
        assert_eq!(all[0].failed[0].option, "denied/too_far");
        assert_eq!(all[0].failed[0].fails, 2);
        assert_eq!(ActionVocab::from_trials(&t).line(), "Actions in use: approved (4), denied (2)");
        // A multi-word action keeps its first word, the full text and a note; no word is an error.
        let a = prepare_action("Approve: per written policy", &t).unwrap();
        assert_eq!(a.word, "approve");
        assert_eq!(a.text.as_deref(), Some("Approve: per written policy"));
        assert_eq!(
            a.note.as_deref(),
            Some("Recorded the action as `approved`: an action is one word (your full text is kept). Put a reason code in --reason, not in the action.")
        );
        let a = prepare_action("deny", &t).unwrap();
        assert!(a.text.is_none() && a.note.is_none());
        let e = prepare_action(" ; ", &t).unwrap_err();
        assert!(e.contains("one word with --action") && e.contains("Actions in use: approved (4), denied (2)."), "{e}");
    }

    #[test]
    fn an_option_that_passed_and_failed_in_a_type_asks_for_the_separating_field() {
        let mut t = vec![];
        // Other agents also record `weather` and `time` for this game, and `deck` for another one.
        record(&mut t, "A1", &[("level", "cave"), ("enemy", "bat"), ("weather", "rain"), ("time", "night")], "fight", None, 100, None);
        record(&mut t, "A1", &[("game", "cards"), ("deck", "red"), ("rank", "high")], "fold", None, 100, None);
        record(&mut t, "A1", &[("game", "cards"), ("deck", "blue")], "fold", None, 100, None);
        record(&mut t, "A1", &[("level", "cave"), ("enemy", "bat"), ("weather", "dry")], "fight", None, 100, None);
        let ty: &[(&str, &str)] = &[("level", "forest"), ("enemy", "troll")];
        // Fail then pass then pass then fail: mixed either way round; the newest outcome decides WORKING/changed?.
        record(&mut t, "A2", ty, "fight", None, 0, None);
        record(&mut t, "A2", ty, "fight", None, 100, None);
        record(&mut t, "A3", ty, "fight", None, 100, None);
        let all = summarize(&t);
        let s = all.iter().find(|s| s.case_type == "forest").unwrap();
        let v = Vocabulary::from_trials(&t);
        assert_eq!(type_line(s), "forest: WORKING fight (2 pass, last at 10:07 UTC; enemy=troll), failed 1x (enemy=troll)");
        assert_eq!(
            split_hint(s, &v).unwrap(),
            "mixed: fight passed 2x and failed 1x — cases of this type may differ in a field not in the key; add the field that separates them (fields others used: weather, time)"
        );
        record(&mut t, "A3", ty, "fight", None, 0, None);
        record(&mut t, "A3", ty, "sneak", None, 0, None);
        let entry = line_of(&t, "forest").unwrap();
        assert!(
            entry.starts_with("forest: changed? fight (2 pass [enemy=troll], then failed 1x, last at 10:08 UTC) | failed: sneak [enemy=troll], fight (2x) [enemy=troll] — no working rule yet; the default action failed for this type: check the reason codes\n  - mixed: fight passed 2x and failed 2x — "),
            "{entry}"
        );
        // Similar cases share the first field value: their extra names come first.
        let mut sim = vec![];
        record(&mut sim, "A1", &[("game", "cards"), ("deck", "red"), ("rank", "high")], "fold", None, 100, None);
        record(&mut sim, "A1", &[("game", "chess"), ("opening", "sicilian"), ("clock", "blitz")], "resign", None, 100, None);
        record(&mut sim, "A1", &[("game", "chess"), ("opening", "french")], "castle", None, 100, None);
        record(&mut sim, "A1", &[("game", "chess"), ("opening", "french")], "castle", None, 0, None);
        let ss = summarize(&sim);
        assert!(split_hint(&ss[0], &Vocabulary::from_trials(&sim)).unwrap().ends_with("(fields others used: clock)"), "{:?}", split_hint(&ss[0], &Vocabulary::from_trials(&sim)));
        // Without similar cases (no shared first field value), the most used missing names.
        let mut solo = vec![];
        record(&mut solo, "A1", &[("game", "cards"), ("deck", "red"), ("rank", "high")], "fold", None, 100, None);
        record(&mut solo, "A1", &[("mode", "duel")], "fold", None, 100, None);
        record(&mut solo, "A1", &[("mode", "duel")], "fold", None, 0, None);
        let sm = summarize(&solo);
        assert!(split_hint(&sm[0], &Vocabulary::from_trials(&solo)).unwrap().ends_with("(fields others used: game, deck, rank)"));
        // Types whose options never both passed and failed carry no hint.
        assert!(split_hint(all.iter().find(|s| s.case_type == "cave").unwrap(), &v).is_none());
        // The listing shows the hint under the type's line.
        let block = case_types_block(&t, 12);
        assert!(block.contains("- forest: changed? fight") && block.contains("\n  - mixed: fight passed 2x and failed 2x"), "{block}");
        assert!(matching_lines(&t, "forest troll", 6)[0].contains("\n  - mixed: "));
    }

    const ENEMIES: [&str; 11] = ["wolf", "bat", "spider", "boar", "goblin", "imp", "slime", "crow", "snake", "rat", "bear"];

    /// The forest: `fight` wins against every enemy but the troll; enemies are names.
    fn forest() -> Vec<Trial> {
        let mut t = vec![];
        for (i, e) in ENEMIES.iter().enumerate() {
            record(&mut t, "A1", &[("level", "forest"), ("enemy", e)], "fight", None, 100, Some(&format!("F{i}")));
        }
        record(&mut t, "A2", &[("level", "forest"), ("enemy", "troll")], "fight", None, 0, Some("T1"));
        record(&mut t, "A3", &[("level", "forest"), ("enemy", "troll")], "fight", None, 0, Some("T2"));
        record(&mut t, "A3", &[("level", "forest"), ("enemy", "troll")], "sneak", None, 100, Some("T3"));
        t
    }

    #[test]
    fn name_like_fields_are_detected_from_their_values_and_stay_out_of_the_key() {
        let t = forest();
        let v = Vocabulary::from_trials(&t);
        assert!(v.name_like("enemy") && !v.name_like("level"));
        assert_eq!(analyze(&t).key_of(&CaseFields(pairs(&[("level", "forest"), ("enemy", "wolf")]))), "forest");
        assert_eq!(v.names_text(), "level (14), enemy (14, names)");
        assert_eq!(v.name_fields(), vec!["enemy"]);
        assert_eq!(v.anchor(&t[0].fields), Some(&(s("level"), s("forest"))));
        // A name field is never a key field, even where it separates outcomes (the troll).
        assert!(analyze(&t).learned.kinds.is_empty());
        // Few enemies yet: not name-like, but no outcome made it a key field either.
        let few = forest()[..3].to_vec();
        assert!(!Vocabulary::from_trials(&few).name_like("enemy"));
        assert_eq!(analyze(&few).key_of(&few[0].fields), "forest");
        // More than six values, mostly seen once: name-like; the same count of values reused: not.
        let mut once = vec![];
        let mut twice = vec![];
        for (i, e) in ENEMIES.iter().take(7).enumerate() {
            record(&mut once, "A1", &[("level", "cave"), ("enemy", e)], "fight", None, 100, Some(&format!("O{i}")));
            for k in 0..2 {
                record(&mut twice, "A1", &[("level", "cave"), ("enemy", e)], "fight", None, 100, Some(&format!("W{i}{k}")));
            }
        }
        assert!(Vocabulary::from_trials(&once).name_like("enemy"));
        assert!(!Vocabulary::from_trials(&twice).name_like("enemy"));
    }

    #[test]
    fn a_name_whose_cases_only_failed_becomes_an_exception_of_its_type() {
        let mut t = forest();
        let all = summarize(&t);
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(
            type_line(&all[0]),
            "forest: WORKING fight (11 pass, last at 10:11 UTC; enemy=wolf 1x|bat 1x|spider 1x|+8, untested for other enemy values) — except enemy=troll: failed fight 2x; WORKING there sneak (1 pass)"
        );
        assert!(all[0].mixed.is_empty() && all[0].exceptions[0].is_rule());
        // One failure for another name: a possible exception, not yet a rule.
        record(&mut t, "A4", &[("level", "forest"), ("enemy", "golem")], "fight", None, 0, Some("G1"));
        let line = type_line(&summarize(&t)[0]);
        assert!(line.ends_with("— except enemy=troll: failed fight 2x; WORKING there sneak (1 pass) — possible exception enemy=golem: failed fight 1x"), "{line}");
        // Searching a name finds its type and the exception.
        let hits = matching_lines(&t, "troll", 6);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].starts_with("forest: WORKING fight") && hits[0].contains("except enemy=troll"), "{hits:?}");
        // Thread documents for the moderator match by key only and carry no split hint.
        assert!(matching_lines_brief(&t, "troll", 6).is_empty());
        assert!(matching_lines_brief(&t, "forest", 6)[0].contains("except enemy=troll"));
        assert!(case_types_block(&t, 12).contains("Name fields (enemy) never join the key"));
    }

    #[test]
    fn a_field_joins_the_key_of_its_kind_only_after_outcomes_split_on_it() {
        let mut t = vec![];
        let q = |level: &'static str, weather: &'static str| [("game", "quest"), ("level", level), ("weather", weather)];
        record(&mut t, "A1", &q("forest", "rain"), "fight", None, 100, Some("Q1"));
        record(&mut t, "A1", &q("forest", "dry"), "fight", None, 100, Some("Q2"));
        record(&mut t, "A1", &q("forest", "rain"), "fight", None, 100, Some("Q3"));
        record(&mut t, "A2", &q("cave", "dry"), "fight", None, 0, Some("Q4"));
        // One failure in the cave is not yet evidence: the key is the kind alone.
        let a = analyze(&t);
        assert!(a.learned.kinds.is_empty());
        assert_eq!(a.types.len(), 1);
        assert_eq!(type_line(&a.types[0]), "quest: changed? fight (3 pass [level=forest, weather=rain 2x|dry 1x], then failed 1x, last at 10:04 UTC) | failed: fight [level=cave, weather=dry] — no working rule yet");
        assert!(split_hint(&a.types[0], &a.vocab).unwrap().starts_with("mixed: fight passed 3x and failed 1x"));
        // A second one: fight passed in the forest and not in the cave, so `level` joins the key and
        // every trial of the kind is re-keyed. Weather (random here) stays out.
        record(&mut t, "A3", &q("cave", "rain"), "fight", None, 0, Some("Q5"));
        let a = analyze(&t);
        let learned = a.learned.fields_of(&(s("game"), s("quest")));
        assert_eq!(learned.len(), 1);
        assert_eq!(learned[0].text(), "level since 10:05 UTC (fight passed for forest, not for cave)");
        let keys: Vec<&str> = a.types.iter().map(|x| x.case_type.as_str()).collect();
        assert_eq!(keys, vec!["quest/cave", "quest/forest"]);
        assert_eq!(type_line(a.type_of("quest/forest").unwrap()), "quest/forest: fight passed 3x (last at 10:03 UTC; weather=rain 2x|dry 1x)");
        assert_eq!(a.kind_line(&t, &(s("game"), s("quest"))), "quest: level since 10:05 UTC (fight passed for forest, not for cave); also recorded: weather");
        // Another option works in the cave; weather keeps varying at random and never joins.
        record(&mut t, "A3", &q("cave", "dry"), "sneak", None, 100, Some("Q6"));
        for (i, w) in ["rain", "dry", "dry", "rain", "rain", "dry"].iter().enumerate() {
            record(&mut t, "A1", &[("game", "quest"), ("level", "forest"), ("weather", w)], "fight", None, 100, Some(&format!("R{i}")));
        }
        record(&mut t, "A3", &q("cave", "rain"), "sneak", None, 100, Some("Q7"));
        let a = analyze(&t);
        assert_eq!(a.learned.fields_of(&(s("game"), s("quest"))).len(), 1);
        assert_eq!(type_line(a.type_of("quest/cave").unwrap()), "quest/cave: sneak passed 2x (last at 10:13 UTC; weather=dry 1x|rain 1x)");
        // A trial without the key field shows `?` in its place.
        assert_eq!(a.key_of(&CaseFields(pairs(&[("game", "quest")]))), "quest/?");
        let block = case_types_block(&t, 12);
        assert!(block.contains("Key fields per kind, learned from outcomes") && block.contains("- quest: level since 10:05 UTC (fight passed for forest, not for cave); also recorded: weather\n"), "{block}");
        assert!(block.contains("Key = the case's kind (game), then the key fields learned for that kind, in the order learned (`?` = not given)."), "{block}");
    }

    #[test]
    fn a_field_with_many_values_needs_more_evidence_than_one_with_two() {
        // `hero` has six values; two failures for one of them is chance, not a rule.
        let heroes = ["ann", "bo", "cy", "di", "ed", "flo"];
        let mut t = vec![];
        for (i, h) in heroes.iter().cycle().take(18).enumerate() {
            record(&mut t, "A1", &[("game", "duel"), ("hero", h)], "attack", None, 100, Some(&format!("D{i}")));
        }
        record(&mut t, "A2", &[("game", "duel"), ("hero", "ann")], "attack", None, 0, Some("X1"));
        record(&mut t, "A2", &[("game", "duel"), ("hero", "ann")], "attack", None, 0, Some("X2"));
        let a = analyze(&t);
        assert!(!a.vocab.name_like("hero"));
        assert!(a.learned.kinds.is_empty(), "{:?}", a.learned);
        // A two-valued field with the same evidence is learned.
        let mut t = vec![];
        for (i, side) in ["left", "left", "left", "right"].iter().enumerate() {
            record(&mut t, "A1", &[("game", "duel"), ("side", side)], "attack", None, 100, Some(&format!("S{i}")));
        }
        record(&mut t, "A2", &[("game", "duel"), ("side", "right")], "attack", None, 0, Some("Y1"));
        record(&mut t, "A2", &[("game", "duel"), ("side", "right")], "attack", None, 0, Some("Y2"));
        record(&mut t, "A2", &[("game", "duel"), ("side", "right")], "attack", None, 0, Some("Y3"));
        assert_eq!(analyze(&t).learned.fields_of(&(s("game"), s("duel")))[0].field, "side");
    }

    #[test]
    fn claims_name_the_values_of_the_cases_behind_them() {
        // Few enemies: `enemy` is a low-variety field, recorded but not (yet) in the key.
        let ty = |e: &'static str| [("level", "forest"), ("enemy", e)];
        let mut t = vec![];
        record(&mut t, "A1", &ty("troll"), "fight", None, 0, Some("G1"));
        for i in 0..3 {
            record(&mut t, "A1", &ty("bat"), "fight", None, 100, Some(&format!("B{i}")));
        }
        let a = analyze(&t);
        assert!(a.learned.kinds.is_empty());
        // Every pass was a bat: WORKING, for exactly those values, with the troll failure named.
        assert_eq!(type_line(&a.types[0]), "forest: WORKING fight (3 pass, last at 10:04 UTC; enemy=bat), failed 1x (enemy=troll)");
        // Passes for different enemies: no WORKING claim, the values instead.
        record(&mut t, "A2", &ty("wolf"), "fight", None, 100, Some("W1"));
        let a = analyze(&t);
        assert_eq!(type_line(&a.types[0]), "forest: fight passed 4x (last at 10:05 UTC; enemy=bat 3x|wolf 1x), failed 1x (enemy=troll)");
        // Failed options name their cases' values too.
        let mut f = vec![];
        record(&mut f, "A1", &ty("troll"), "fight", None, 0, None);
        record(&mut f, "A1", &ty("ogre"), "fight", None, 0, None);
        assert_eq!(type_line(&summarize(&f)[0]), "forest: failed: fight (2x) [enemy=troll 1x|ogre 1x] — no working rule yet; the default action failed for this type: check the reason codes");
        // Key fields and name fields are not repeated as evidence.
        let forest = forest();
        // Name fields are listed (never required to agree) with a note that other names are untested.
        assert!(type_line(&summarize(&forest)[0]).starts_with("forest: WORKING fight (11 pass, last at 10:11 UTC; enemy=wolf 1x|bat 1x|spider 1x|+8, untested for other enemy values) — except enemy=troll"));
        // A kind that uses a name field: passes that recorded none say so.
        let mut cave = forest.clone();
        record(&mut cave, "A1", &[("level", "cave")], "fight", None, 100, Some("K1"));
        record(&mut cave, "A1", &[("level", "cave"), ("enemy", "bat")], "sneak", None, 100, Some("K2"));
        let a = analyze(&cave);
        assert_eq!(
            type_line(a.type_of("cave").unwrap()),
            "cave: WORKING sneak (1 pass, last at 10:16 UTC; enemy=bat, untested for other enemy values); also WORKING fight (1 pass, last at 10:15 UTC; enemy=none recorded, untested for other enemy values) (more than one option passes: compare the values each passed for with your case's; an attribute outside the type, such as an amount, may decide)"
        );
    }

    #[test]
    fn a_value_that_loses_about_as_often_as_the_others_never_joins_the_key() {
        // Six heroes; attack loses about half the time for every hero (something outside the
        // fields decides). One hero gathered four more losses than wins: by count alone it would
        // look special, but its rate does not stand out from the others'.
        let mut t = vec![];
        let heroes = ["ann", "bo", "cy", "di", "ed", "flo"];
        // Rounds of duels, every hero once per round.
        for round in 0..6 {
            for h in heroes {
                let win = if h == "ann" { round == 0 } else { round % 2 == 0 };
                record(&mut t, "A1", &[("game", "duel"), ("hero", h)], "attack", None, if win { 100 } else { 0 }, Some(&format!("{h}{round}")));
            }
        }
        let a = analyze(&t);
        assert!(!a.vocab.name_like("hero"));
        assert!(a.learned.kinds.is_empty(), "{:?}", a.learned);
    }

    #[test]
    fn closest_cases_rank_trials_by_matching_values() {
        let mut t = vec![];
        let q = |level: &'static str, enemy: &'static str, time: &'static str| [("level", level), ("enemy", enemy), ("time", time)];
        record(&mut t, "A1", &q("forest", "troll", "night"), "sneak", None, 100, Some("1"));
        record(&mut t, "A1", &q("forest", "troll", "day"), "fight", None, 0, Some("2"));
        record(&mut t, "A1", &q("forest", "troll", "day"), "sneak", None, 100, Some("3"));
        record(&mut t, "A1", &q("forest", "troll", "day"), "fight", None, 100, Some("3b"));
        record(&mut t, "A1", &q("forest", "troll", "day"), "fight", None, 0, Some("3c"));
        record(&mut t, "A1", &q("cave", "troll", "night"), "fight", None, 100, Some("4"));
        record(&mut t, "A1", &q("cave", "bat", "day"), "fight", None, 100, Some("5"));
        let lines = closest_cases(&t, "forest troll day", 6);
        assert_eq!(
            lines,
            vec![
                s("[level=forest enemy=troll time=day] fight PASS 1x FAIL 2x (newest FAIL), sneak PASS 1x"),
                s("[level=forest enemy=troll] sneak PASS 1x"),
            ]
        );
        // One matching value only counts when nothing matches two.
        assert_eq!(closest_cases(&t, "bat", 6), vec![s("[enemy=bat] fight PASS 1x")]);
        assert!(closest_cases(&t, "refund", 6).is_empty());
    }

    #[test]
    fn fields_keep_their_order_through_json_and_old_objects_load() {
        let f = CaseFields(pairs(&[("level", "forest"), ("enemy", "troll"), ("area", "north")]));
        let j = serde_json::to_string(&f).unwrap();
        assert_eq!(j, r#"{"level":"forest","enemy":"troll","area":"north"}"#);
        assert_eq!(serde_json::from_str::<CaseFields>(&j).unwrap(), f);
        // Objects are read in document order, whatever the names (earlier journals: see crate tests).
        let other: CaseFields = serde_json::from_str(r#"{"zeta":"a","alpha":"b","mid":"c","beta":"d"}"#).unwrap();
        assert_eq!(other.key(), "a/b/c/d");
        assert_eq!(other.names().collect::<Vec<_>>(), vec!["zeta", "alpha", "mid", "beta"]);
        let pairs_form: CaseFields = serde_json::from_str(r#"[["level","forest"],["enemy","troll"]]"#).unwrap();
        assert_eq!(pairs_form.key(), "forest/troll");
        let with_null: CaseFields = serde_json::from_str(r#"{"level":"forest","enemy":null}"#).unwrap();
        assert_eq!(with_null.key(), "forest");
    }
}
