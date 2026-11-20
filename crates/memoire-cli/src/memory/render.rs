//! What the memory verbs print for people: one compact screen, the same
//! tables and field blocks as the rest of the CLI. JSON is the verbs' own.
use crate::output::{database_block, table};
use memoire_api::Database;
use memoire_memory::model::{Confirmation, Dispute, Event, Memory, Revision, Status};

/// Widest label in a field block.
const PAD: usize = "superseded by".len();

fn block(fields: &[(&str, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k:<PAD$}  {v}").trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The status, with the distinct confirmers of a confirmed memory.
pub fn status(memory: &Memory) -> String {
    if memory.status == Status::Confirmed {
        format!("confirmed ×{}", memory.confirmers)
    } else {
        memory.status.as_str().to_owned()
    }
}

/// The date of a timestamp.
fn date(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
}

fn who(principal: &str, agent: &str, session: &str) -> String {
    let mut text = principal.to_owned();
    if !agent.is_empty() {
        text.push_str(&format!(" ({agent})"));
    }
    if !session.is_empty() {
        text.push_str(&format!(" session {session}"));
    }
    text
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "-".into()
    } else {
        items.join(", ")
    }
}

/// `init`: the database, then the store.
/// `init`; without `database` (a key that may not read the catalog), the
/// store alone.
pub fn init(
    database: Option<&Database>,
    created: bool,
    migrated_from: i64,
    store_id: &str,
) -> String {
    let version = memoire_memory::schema::SCHEMA_VERSION;
    let store = if migrated_from == version {
        format!("schema {version}, up to date")
    } else {
        format!("schema {version}, migrated from {migrated_from}")
    };
    let store = format!("{:<15}  {store} ({store_id})", "memory store");
    match database {
        Some(database) => format!(
            "{}\n{:<15}  {}\n{store}",
            database_block(database),
            "database",
            if created { "created" } else { "kept" },
        ),
        None => store,
    }
}

/// The fields of one memory, then its body.
pub fn memory(memory: &Memory) -> String {
    let mut fields = vec![
        ("id", memory.id.clone()),
        ("title", memory.title.clone()),
        ("status", status(memory)),
        ("kind", memory.kind.clone()),
        ("scope", memory.scope.clone()),
        ("tags", list(&memory.tags)),
        ("rev", memory.rev.to_string()),
        (
            "author",
            who(
                &memory.author.principal,
                &memory.author.agent,
                &memory.author.session,
            ),
        ),
        ("created", memory.created_at.clone()),
        ("updated", memory.updated_at.clone()),
    ];
    if memory.open_disputes > 0 {
        fields.push(("disputes", format!("{} open", memory.open_disputes)));
    }
    if let Some(id) = &memory.supersedes {
        fields.push(("supersedes", id.clone()));
    }
    if let Some(id) = &memory.superseded_by {
        fields.push(("superseded by", id.clone()));
    }
    let mut text = block(&fields);
    if !memory.body.is_empty() {
        text.push_str("\n\n");
        text.push_str(memory.body.trim_end());
    }
    text
}

/// `show`: the memory, then its evidence, confirmations, disputes and,
/// with `--history`, its revisions.
pub fn show(
    memory: &Memory,
    confirmations: &[Confirmation],
    disputes: &[Dispute],
    revisions: Option<&[Revision]>,
) -> String {
    let mut sections = vec![self::memory(memory)];
    if !memory.evidence.is_empty() {
        let lines: Vec<String> = memory.evidence.iter().map(|e| format!("  {e}")).collect();
        sections.push(format!("Evidence\n{}", lines.join("\n")));
    }
    if !confirmations.is_empty() {
        let rows: Vec<Vec<String>> = confirmations
            .iter()
            .map(|c| {
                vec![
                    c.rev.to_string(),
                    who(&c.principal, &c.agent, &c.session),
                    if c.counts { "yes" } else { "no" }.into(),
                    date(&c.created_at).into(),
                    list(&c.evidence),
                ]
            })
            .collect();
        sections.push(format!(
            "Confirmations\n{}",
            table(&["REV", "BY", "COUNTS", "DATE", "EVIDENCE"], &rows)
        ));
    }
    if !disputes.is_empty() {
        let rows: Vec<Vec<String>> = disputes
            .iter()
            .map(|d| {
                vec![
                    d.rev.to_string(),
                    who(&d.principal, &d.agent, &d.session),
                    d.resolved_at
                        .as_deref()
                        .map_or_else(|| "open".to_owned(), |at| format!("resolved {}", date(at))),
                    date(&d.created_at).into(),
                    d.reason.clone(),
                ]
            })
            .collect();
        sections.push(format!(
            "Disputes\n{}",
            table(&["REV", "BY", "STATE", "DATE", "REASON"], &rows)
        ));
    }
    if let Some(revisions) = revisions {
        let blocks: Vec<String> = revisions.iter().map(revision).collect();
        sections.push(format!("Revisions\n{}", blocks.join("\n")));
    }
    sections.join("\n\n")
}

/// One revision of `show --history`: who wrote it, when and why, then the
/// title and body it had.
fn revision(r: &Revision) -> String {
    let mut lines = vec![
        format!(
            "rev {}  {}  {}  {}",
            r.rev,
            r.status.as_str(),
            r.created_at,
            who(&r.author_principal, &r.author_agent, &r.author_session)
        ),
        format!("  reason: {}", r.reason),
        format!("  title: {}", r.title),
    ];
    lines.extend(r.body.trim_end().lines().map(|line| format!("    {line}")));
    lines
        .join("\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `recall`: one memory per row, with the rev to pass `--rev`.
pub fn recall(memories: &[Memory]) -> String {
    let rows: Vec<Vec<String>> = memories
        .iter()
        .map(|m| {
            vec![
                m.id.clone(),
                m.rev.to_string(),
                status(m),
                m.kind.clone(),
                m.scope.clone(),
                date(&m.updated_at).into(),
                m.title.clone(),
            ]
        })
        .collect();
    table(
        &["ID", "REV", "STATUS", "KIND", "SCOPE", "UPDATED", "TITLE"],
        &rows,
    )
}

/// `supersede`: the memory replaced, then the one replacing it.
pub fn superseded(old: &Memory, new: &Memory) -> String {
    format!(
        "Superseded {}  {}\nby         {}  {}",
        old.id, old.title, new.id, new.title
    )
}

/// `log`: one event per row, newest first.
pub fn log(events: &[Event]) -> String {
    let rows: Vec<Vec<String>> = events
        .iter()
        .map(|e| {
            vec![
                e.seq.to_string(),
                e.created_at
                    .get(..19)
                    .unwrap_or(&e.created_at)
                    .replace('T', " "),
                e.verb.clone(),
                e.memory_id.clone(),
                e.scope.clone().unwrap_or_else(|| "-".into()),
                if e.agent.is_empty() {
                    e.principal.clone()
                } else {
                    e.agent.clone()
                },
                e.summary.clone().unwrap_or_default(),
            ]
        })
        .collect();
    table(
        &["SEQ", "WHEN", "VERB", "MEMORY", "SCOPE", "BY", "SUMMARY"],
        &rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use memoire_memory::model::Author;

    fn sample() -> Memory {
        Memory {
            seq: 1,
            id: "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8".into(),
            rev: 2,
            content_rev: 1,
            content_principal: "key_a".into(),
            content_session: "s1".into(),
            kind: "pitfall".into(),
            scope: "global".into(),
            title: "Deploys need the VPN".into(),
            body: "Connect first.\n".into(),
            tags: vec!["deploy".into(), "vpn".into()],
            evidence: vec!["docs/deploy.md".into()],
            status: Status::Confirmed,
            supersedes: None,
            superseded_by: None,
            author: Author {
                principal: "key_a".into(),
                agent: "agent-a".into(),
                session: "s1".into(),
            },
            confirmers: 2,
            open_disputes: 0,
            created_at: "2026-11-19T10:00:00.000Z".into(),
            updated_at: "2026-11-19T11:00:00.000Z".into(),
        }
    }

    #[test]
    fn log_rows_name_the_agent_or_else_the_principal() {
        let event = |agent: &str| Event {
            seq: 3,
            memory_id: "mem_x".into(),
            verb: "purge".into(),
            rev: None,
            scope: None,
            principal: "key_a".into(),
            agent: agent.into(),
            session: String::new(),
            summary: Some("leak".into()),
            created_at: "2026-11-19T10:00:00.123Z".into(),
        };
        let text = log(&[event("agent-a"), event("")]);
        let rows: Vec<&str> = text.lines().skip(2).collect();
        assert_eq!(
            rows[0],
            "3    2026-11-19 10:00:00  purge  mem_x   -      agent-a  leak"
        );
        assert!(rows[1].contains("  key_a    leak"), "{}", rows[1]);
    }

    #[test]
    fn recall_rows_show_confirmers_and_the_date() {
        let text = recall(&[sample()]);
        let row = text.lines().nth(2).unwrap();
        let header: Vec<&str> = text.lines().next().unwrap().split_whitespace().collect();
        assert_eq!(
            header,
            ["ID", "REV", "STATUS", "KIND", "SCOPE", "UPDATED", "TITLE"]
        );
        assert!(
            row.contains("  2    confirmed ×2  pitfall  global  2026-11-19  Deploys need the VPN"),
            "{row}"
        );
    }

    #[test]
    fn a_memory_is_a_field_block_then_its_body() {
        let text = memory(&sample());
        assert!(
            text.contains("\nauthor         key_a (agent-a) session s1\n"),
            "{text}"
        );
        assert!(text.ends_with("\n\nConnect first."), "{text}");
        let first = Revision {
            memory_id: "mem_x".into(),
            rev: 1,
            kind: "fact".into(),
            scope: "global".into(),
            title: "Deploys need a VPN".into(),
            body: "Connect.\nThen deploy.\n".into(),
            tags: vec![],
            evidence: vec![],
            status: Status::Inferred,
            supersedes: None,
            superseded_by: None,
            author_principal: "key_a".into(),
            author_agent: "agent-a".into(),
            author_session: "s1".into(),
            reason: "remembered".into(),
            created_at: "2026-11-19T10:00:00.000Z".into(),
        };
        let second = Revision {
            rev: 2,
            title: "Deploys need the VPN".into(),
            body: String::new(),
            reason: "precise".into(),
            ..first.clone()
        };
        let shown = show(&sample(), &[], &[], Some(&[first, second]));
        assert!(shown.contains("Evidence\n  docs/deploy.md"), "{shown}");
        assert!(
            shown.ends_with(
                "Revisions\n\
                 rev 1  inferred  2026-11-19T10:00:00.000Z  key_a (agent-a) session s1\n  \
                 reason: remembered\n  title: Deploys need a VPN\n    Connect.\n    Then deploy.\n\
                 rev 2  inferred  2026-11-19T10:00:00.000Z  key_a (agent-a) session s1\n  \
                 reason: precise\n  title: Deploys need the VPN"
            ),
            "{shown}"
        );
    }
}
