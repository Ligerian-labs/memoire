//! What a memory is on this side of the wire: ids, validated fields, size
//! limits and the JSON shapes the verbs print.
use crate::{Invalid, Refusal};
use serde::Serialize;

/// Longest title, in characters.
pub const TITLE_MAX_CHARS: usize = 120;
/// Largest body, in bytes of UTF-8.
pub const BODY_MAX_BYTES: usize = 8 * 1024;
/// Most tags on one memory.
pub const TAGS_MAX: usize = 10;
/// Most evidence items on one memory or confirmation.
pub const EVIDENCE_MAX_ITEMS: usize = 10;
/// Longest evidence item, in characters.
pub const EVIDENCE_MAX_CHARS: usize = 500;
/// Longest reason, in characters.
pub const REASON_MAX_CHARS: usize = 500;
/// Longest `--agent`, in characters.
pub const AGENT_MAX_CHARS: usize = 64;
/// Longest `--session`, in characters.
pub const SESSION_MAX_CHARS: usize = 128;

/// The scope a memory gets outside a git checkout with an `origin`.
pub const GLOBAL_SCOPE: &str = "global";
/// The kind a memory gets without `--kind`.
pub const DEFAULT_KIND: &str = "fact";

const ID_PREFIX: &str = "mem_";
/// Crockford's base32, as ULIDs use it.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const ULID_LEN: usize = 26;

/// A ULID: 48 bits of Unix milliseconds, then 80 random bits, in 26
/// characters of Crockford base32, so ids sort by creation time.
pub fn ulid(millis: u64, random: [u8; 10]) -> String {
    let mut value = u128::from(millis & ((1 << 48) - 1)) << 80;
    for (i, byte) in random.iter().enumerate() {
        value |= u128::from(*byte) << (8 * (9 - i));
    }
    (0..ULID_LEN)
        .map(|i| {
            let shift = 5 * (ULID_LEN - 1 - i);
            char::from(CROCKFORD[((value >> shift) & 0x1f) as usize])
        })
        .collect()
}

/// A fresh ULID from the system clock and the operating system's
/// randomness.
/// The error is the operating system's.
pub fn fresh_ulid() -> Result<String, String> {
    let mut random = [0; 10];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    Ok(ulid(millis, random))
}

/// A new memory id, `mem_` and a ULID.
pub fn new_memory_id() -> Result<String, String> {
    Ok(format!("{ID_PREFIX}{}", fresh_ulid()?))
}

/// A memory id as typed: `mem_` and 26 Crockford characters, in either
/// case; returned uppercase as generated.
pub fn parse_memory_id(text: &str) -> Result<String, String> {
    let invalid = || format!("`{text}` is not a memory id (mem_ and 26 base32 characters)");
    let ulid = text.strip_prefix(ID_PREFIX).ok_or_else(invalid)?;
    let ulid = ulid.to_ascii_uppercase();
    // 26 characters hold 130 bits; a ULID's top two are zero.
    if ulid.len() != ULID_LEN
        || !ulid.bytes().all(|b| CROCKFORD.contains(&b))
        || ulid.as_bytes()[0] > b'7'
    {
        return Err(invalid());
    }
    Ok(format!("{ID_PREFIX}{ulid}"))
}

/// `--scope`: `^[a-z0-9][a-z0-9._:/-]{0,127}$`.
pub fn parse_scope(text: &str) -> Result<String, String> {
    if crate::scope::valid(text) {
        Ok(text.to_owned())
    } else {
        Err("a scope is 1-128 characters: a lowercase letter or digit, then lowercase letters, digits or . _ : / -".into())
    }
}

/// `--kind`: `^[a-z][a-z0-9-]{1,31}$`.
pub fn parse_kind(text: &str) -> Result<String, String> {
    let mut chars = text.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && (2..=32).contains(&text.len())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(text.to_owned())
    } else {
        Err(
            "a kind is 2-32 characters: a lowercase letter, then lowercase letters, digits or -"
                .into(),
        )
    }
}

/// `--tag`: `^[a-z0-9][a-z0-9._-]{0,47}$`.
pub fn parse_tag(text: &str) -> Result<String, String> {
    let mut chars = text.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && text.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
    if ok {
        Ok(text.to_owned())
    } else {
        Err("a tag is 1-48 characters: a lowercase letter or digit, then lowercase letters, digits or . _ -".into())
    }
}

fn label(text: &str, what: &str, max: usize) -> Result<String, String> {
    if text.chars().count() > max {
        return Err(format!("{what} is at most {max} characters"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!("{what} has a control character"));
    }
    Ok(text.to_owned())
}

/// `--agent` / `MEMOIRE_AGENT`.
pub fn parse_agent(text: &str) -> Result<String, String> {
    label(text, "an agent name", AGENT_MAX_CHARS)
}

/// `--session` / `MEMOIRE_SESSION`.
pub fn parse_session(text: &str) -> Result<String, String> {
    label(text, "a session", SESSION_MAX_CHARS)
}

/// The live status of a memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Inferred,
    Confirmed,
    Disputed,
    Superseded,
    Forgotten,
}

impl Status {
    /// What recall searches without `--status`.
    pub const RECALLED: [Status; 3] = [Status::Inferred, Status::Confirmed, Status::Disputed];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inferred => "inferred",
            Self::Confirmed => "confirmed",
            Self::Disputed => "disputed",
            Self::Superseded => "superseded",
            Self::Forgotten => "forgotten",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Inferred,
            Self::Confirmed,
            Self::Disputed,
            Self::Superseded,
            Self::Forgotten,
        ]
        .into_iter()
        .find(|s| s.as_str() == text)
    }

    /// Whether the memory may still be revised or confirmed.
    pub fn is_current(self) -> bool {
        !matches!(self, Self::Superseded | Self::Forgotten)
    }
}

fn too_large(field: &'static str, limit: usize, size: usize) -> Refusal {
    Refusal::TooLarge {
        field: field.to_owned(),
        limit,
        size,
    }
}

fn usage(invalid: Invalid) -> Refusal {
    Refusal::InvalidArgument(invalid)
}

/// A title: trimmed, one line, 1 to 120 characters.
pub fn title(text: &str) -> Result<String, Refusal> {
    let text = text.trim();
    if text.is_empty() {
        return Err(usage(Invalid::EmptyTitle));
    }
    if text.contains(['\n', '\r']) {
        return Err(usage(Invalid::MultilineTitle));
    }
    let chars = text.chars().count();
    if chars > TITLE_MAX_CHARS {
        return Err(too_large("title", TITLE_MAX_CHARS, chars));
    }
    Ok(text.to_owned())
}

/// A body: at most 8 KiB, possibly empty, kept as given.
pub fn body(text: String) -> Result<String, Refusal> {
    if text.len() > BODY_MAX_BYTES {
        return Err(too_large("body", BODY_MAX_BYTES, text.len()));
    }
    Ok(text)
}

/// Tags with repeats dropped, first occurrence kept, at most 10.
pub fn tags(list: impl IntoIterator<Item = String>) -> Result<Vec<String>, Refusal> {
    let tags = dedupe(list);
    if tags.len() > TAGS_MAX {
        return Err(too_large("tags", TAGS_MAX, tags.len()));
    }
    Ok(tags)
}

/// Evidence items, trimmed, repeats dropped: at most 10, each 1 to 500
/// characters.
pub fn evidence(list: impl IntoIterator<Item = String>) -> Result<Vec<String>, Refusal> {
    let items = dedupe(list.into_iter().map(|e| e.trim().to_owned()));
    if items.iter().any(String::is_empty) {
        return Err(usage(Invalid::EmptyEvidence));
    }
    if items.len() > EVIDENCE_MAX_ITEMS {
        return Err(too_large("evidence", EVIDENCE_MAX_ITEMS, items.len()));
    }
    if let Some(long) = items
        .iter()
        .find(|e| e.chars().count() > EVIDENCE_MAX_CHARS)
    {
        return Err(too_large(
            "evidence",
            EVIDENCE_MAX_CHARS,
            long.chars().count(),
        ));
    }
    Ok(items)
}

/// A reason: trimmed, 1 to 500 characters.
pub fn reason(text: &str) -> Result<String, Refusal> {
    let text = text.trim();
    if text.is_empty() {
        return Err(usage(Invalid::EmptyReason));
    }
    let chars = text.chars().count();
    if chars > REASON_MAX_CHARS {
        return Err(too_large("reason", REASON_MAX_CHARS, chars));
    }
    Ok(text.to_owned())
}

fn dedupe(list: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = Vec::new();
    for item in list {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
}

/// Refuses the write when any field looks like it holds a secret. The
/// error names the pattern and the field, never the text.
pub fn guard_secrets<'a>(
    fields: impl IntoIterator<Item = (&'static str, &'a str)>,
) -> Result<(), Refusal> {
    for (field, text) in fields {
        if let Some(pattern) = crate::secrets::scan(text) {
            return Err(Refusal::SecretDetected {
                pattern: pattern.to_owned(),
                field: field.to_owned(),
            });
        }
    }
    Ok(())
}

/// Who wrote a revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Author {
    pub principal: String,
    pub agent: String,
    pub session: String,
}

/// A memory's current state, as every verb prints it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Memory {
    #[serde(skip)]
    pub seq: i64,
    pub id: String,
    pub rev: i64,
    /// The latest revision that changed title, body or kind, and its
    /// author's principal and session: confirmations are compared with
    /// them.
    #[serde(skip)]
    pub content_rev: i64,
    #[serde(skip)]
    pub content_principal: String,
    #[serde(skip)]
    pub content_session: String,
    pub kind: String,
    pub scope: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub evidence: Vec<String>,
    pub status: Status,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    /// The latest revision's author.
    pub author: Author,
    /// Distinct (principal, session) pairs whose confirmation counts.
    pub confirmers: i64,
    pub open_disputes: i64,
    pub created_at: String,
    /// The last time anything was written about the memory: a revision, a
    /// confirmation, a dispute or a resolution. Recall's recency and its
    /// newest-first order follow it.
    pub updated_at: String,
}

/// A memory with its recall score.
#[derive(Serialize)]
pub struct Scored<'a> {
    #[serde(flatten)]
    pub memory: &'a Memory,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// One entry of a memory's history.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Revision {
    pub memory_id: String,
    pub rev: i64,
    pub kind: String,
    pub scope: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub evidence: Vec<String>,
    pub status: Status,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub author_principal: String,
    pub author_agent: String,
    pub author_session: String,
    pub reason: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Confirmation {
    pub rev: i64,
    pub principal: String,
    pub agent: String,
    pub session: String,
    pub evidence: Vec<String>,
    pub created_at: String,
    /// Whether it covers the current text and comes from someone other
    /// than the content author.
    pub counts: bool,
}

/// One entry of the store's log.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    pub seq: i64,
    pub memory_id: String,
    pub verb: String,
    pub rev: Option<i64>,
    pub scope: Option<String>,
    pub principal: String,
    pub agent: String,
    pub session: String,
    pub summary: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Dispute {
    pub rev: i64,
    pub principal: String,
    pub agent: String,
    pub session: String,
    pub reason: String,
    pub evidence: Vec<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub resolution: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RefusalClass;

    #[test]
    fn ulids_are_26_crockford_characters_that_sort_by_time() {
        let id = ulid(1_795_082_400_000, [0xab; 10]);
        assert_eq!(id.len(), 26);
        assert!(id.bytes().all(|b| CROCKFORD.contains(&b)), "{id}");
        assert_eq!(ulid(0, [0; 10]), "0".repeat(26));
        assert_eq!(ulid(u64::MAX, [0xff; 10]), format!("7{}", "Z".repeat(25)));
        // A later millisecond sorts after any randomness of an earlier one.
        assert!(ulid(1_000, [0xff; 10]) < ulid(1_001, [0; 10]));
        assert!(ulid(1_000, [0; 10]) < ulid(1_000, [1; 10]));
        let fresh = new_memory_id().unwrap();
        assert_eq!(parse_memory_id(&fresh).unwrap(), fresh);
        assert_ne!(fresh, new_memory_id().unwrap());
    }

    #[test]
    fn memory_ids_parse_in_either_case() {
        let id = format!("mem_{}", ulid(1_795_082_400_000, [7; 10]));
        let lower = format!("mem_{}", id[4..].to_ascii_lowercase());
        assert_eq!(parse_memory_id(&lower), Ok(id.clone()));
        for bad in [
            "",
            "mem_",
            &id[..id.len() - 1],
            &format!("{id}0"),
            &id.replace("mem_", "mom_"),
            &format!("MEM_{}", &id[4..]),
            &format!("mem_8{}", "Z".repeat(25)),
            &format!("mem_{}U", "0".repeat(25)),
            &format!("mem_{}IL", "0".repeat(24)),
        ] {
            assert!(parse_memory_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn field_syntax_follows_the_patterns() {
        for ok in ["global", "project:github.com/acme/widgets", "run:42", "0"] {
            assert!(parse_scope(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Global", ":x", "a b", &"a".repeat(129), "café"] {
            assert!(parse_scope(bad).is_err(), "{bad}");
        }
        assert!(parse_scope(&"a".repeat(128)).is_ok());
        for ok in ["fact", "pitfall", "x1", "run-book"] {
            assert!(parse_kind(ok).is_ok(), "{ok}");
        }
        for bad in ["f", "1fact", "Fact", "a_b", &"a".repeat(33)] {
            assert!(parse_kind(bad).is_err(), "{bad}");
        }
        for ok in ["deploy", "v1.2", "ci_cd", "0", &"t".repeat(48)] {
            assert!(parse_tag(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "Deploy", "a/b", &"t".repeat(49)] {
            assert!(parse_tag(bad).is_err(), "{bad}");
        }
        assert!(parse_agent(&"a".repeat(64)).is_ok());
        assert!(parse_agent(&"a".repeat(65)).is_err());
        assert!(parse_agent("").is_ok());
        assert!(parse_session("s\n1").is_err());
        assert!(parse_session(&"é".repeat(128)).is_ok());
        assert!(parse_session(&"s".repeat(129)).is_err());
    }

    fn field(refusal: &Refusal) -> (String, serde_json::Value, serde_json::Value) {
        let data = refusal.data();
        (
            refusal.code().to_owned(),
            data["field"].clone(),
            data["limit"].clone(),
        )
    }

    #[test]
    fn limits_refuse_what_is_too_large() {
        assert_eq!(title("  A title  ").unwrap(), "A title");
        assert_eq!(title(&"t".repeat(120)).unwrap().len(), 120);
        assert_eq!(
            field(&title(&"t".repeat(121)).unwrap_err()),
            ("memory_too_large".into(), "title".into(), 120.into())
        );
        assert_eq!(title(&"é".repeat(120)).unwrap().chars().count(), 120);
        assert_eq!(title(" ").unwrap_err().class(), RefusalClass::Usage);
        assert_eq!(
            title("two\nlines").unwrap_err().class(),
            RefusalClass::Usage
        );

        assert!(body("b".repeat(8192)).is_ok());
        assert!(body(String::new()).is_ok());
        assert_eq!(
            field(&body("b".repeat(8193)).unwrap_err()),
            ("memory_too_large".into(), "body".into(), 8192.into())
        );
        // Bytes, not characters: 4097 two-byte characters pass 8 KiB.
        assert!(body("é".repeat(4097)).is_err());

        let many = |n: usize| (0..n).map(|i| format!("t{i}")).collect::<Vec<_>>();
        assert_eq!(tags(many(10)).unwrap().len(), 10);
        assert_eq!(tags(["a", "b", "a"].map(String::from)).unwrap(), ["a", "b"]);
        assert_eq!(
            field(&tags(many(11)).unwrap_err()),
            ("memory_too_large".into(), "tags".into(), 10.into())
        );
        assert_eq!(evidence(many(10)).unwrap().len(), 10);
        assert_eq!(
            field(&evidence(many(11)).unwrap_err()).1,
            serde_json::json!("evidence")
        );
        assert!(evidence(["e".repeat(500)]).is_ok());
        assert_eq!(
            field(&evidence(["e".repeat(501)]).unwrap_err()).2,
            serde_json::json!(500)
        );
        assert_eq!(
            evidence(["  ".to_owned()]).unwrap_err().class(),
            RefusalClass::Usage
        );
        assert_eq!(reason(" why ").unwrap(), "why");
        assert_eq!(reason("").unwrap_err().class(), RefusalClass::Usage);
        assert_eq!(
            field(&reason(&"r".repeat(501)).unwrap_err()).1,
            serde_json::json!("reason")
        );
    }

    #[test]
    fn statuses_round_trip() {
        for status in [
            Status::Inferred,
            Status::Confirmed,
            Status::Disputed,
            Status::Superseded,
            Status::Forgotten,
        ] {
            assert_eq!(Status::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::json!(status.as_str())
            );
        }
        assert_eq!(Status::parse("Inferred"), None);
        assert!(!Status::Superseded.is_current() && Status::Disputed.is_current());
    }
}
