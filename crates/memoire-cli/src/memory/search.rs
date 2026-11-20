//! Full-text search: query terms, the FTS5 expression built from them, the
//! ranking statement, and the near-duplicate check of `remember`.
//!
//! User text never reaches FTS5's query syntax: it is cut into terms, and
//! each term goes in double quotes.
use super::model::Status;
use super::store::{Batch, MEMORY_COLUMNS, int, text};
use memoire_api::SqlValue;
use std::collections::BTreeSet;

/// Most terms a query keeps.
pub const MAX_TERMS: usize = 32;

/// bm25 weight of a match in the title.
pub const TITLE_WEIGHT: f64 = 4.0;
/// bm25 weight of a match in the body.
pub const BODY_WEIGHT: f64 = 1.0;
/// bm25 weight of a match in the tags.
pub const TAGS_WEIGHT: f64 = 2.0;
/// bm25 weight of a match in the scope.
pub const SCOPE_WEIGHT: f64 = 0.5;
/// Trust added per distinct confirmer of a confirmed memory.
pub const CONFIRMER_BOOST: f64 = 0.15;
/// Confirmers past this many add nothing more.
pub const CONFIRMERS_COUNTED: i64 = 4;
/// Trust of a disputed memory.
pub const DISPUTED_TRUST: f64 = 0.5;
/// What recency never falls below.
pub const RECENCY_FLOOR: f64 = 0.75;
/// Days after which recency has lost half of what it can lose.
pub const RECENCY_HALF_LIFE_DAYS: f64 = 90.0;

/// A near-duplicate's title shares at least this share of tokens (Dice
/// coefficient) with the new one. Settled by the pair table in the tests.
pub const DUPLICATE_THRESHOLD: f64 = 0.65;
/// Most candidates the duplicate check compares.
pub const DUPLICATE_CANDIDATES: i64 = 20;

/// Words that say nothing about what a title is about.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "by", "can", "did", "do", "does", "for",
    "from", "i", "in", "into", "is", "it", "its", "must", "of", "on", "or", "our", "should", "so",
    "than", "that", "the", "then", "these", "this", "those", "to", "was", "we", "were", "will",
    "with", "you", "your",
];

/// The search terms of `text`: lowercase runs of letters and digits, first
/// occurrence kept, at most [`MAX_TERMS`].
pub fn terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let word = word.to_lowercase();
        if !terms.contains(&word) {
            terms.push(word);
            if terms.len() == MAX_TERMS {
                break;
            }
        }
    }
    terms
}

/// The FTS5 expression matching any of `terms`, each quoted. Terms hold
/// letters and digits only, so no quote can end one early.
pub fn match_any(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// The tokens a title is compared on: its terms without stopwords, with a
/// plural `s` folded so "deploys" and "deploy" meet.
pub fn title_tokens(title: &str) -> BTreeSet<String> {
    terms(title)
        .into_iter()
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .map(|t| {
            if t.chars().count() > 3 && t.ends_with('s') && !t.ends_with("ss") {
                t[..t.len() - 1].to_owned()
            } else {
                t
            }
        })
        .collect()
}

/// The Dice coefficient of two token sets, `2·|A ∩ B| / (|A| + |B|)`.
pub fn similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    2.0 * a.intersection(b).count() as f64 / (a.len() + b.len()) as f64
}

/// Selects the duplicate candidates of a title with `tokens`: memories of
/// `scope`, still current, whose title matches a token, best first. With
/// `except`, the memory being replaced, which is never a candidate and
/// takes none of the slots.
pub fn duplicate_candidates(
    batch: &mut Batch,
    tokens: &BTreeSet<String>,
    scope: &str,
    except: Option<&str>,
) -> usize {
    let tokens: Vec<String> = tokens.iter().cloned().collect();
    let mut params = vec![
        text(&format!("{{title}} : ({})", match_any(&tokens))),
        text(scope),
        int(DUPLICATE_CANDIDATES),
    ];
    let exclude = match except {
        Some(id) => {
            params.push(text(id));
            "AND m.id <> ?4 "
        }
        None => "",
    };
    batch.push(
        format!(
            "SELECT m.id, m.title, m.status FROM \
             (SELECT rowid AS seq, bm25(memoire_memory_fts) AS rank FROM memoire_memory_fts \
              WHERE memoire_memory_fts MATCH ?1) h \
             JOIN memoire_memory_memories m ON m.seq = h.seq \
             WHERE m.scope = ?2 AND m.status NOT IN ('superseded', 'forgotten') {exclude}\
             ORDER BY h.rank, m.id LIMIT ?3"
        ),
        params,
    )
}

/// What recall filters on.
#[derive(Debug)]
pub struct Filter<'a> {
    /// `None`: every scope.
    pub scopes: Option<&'a [String]>,
    pub statuses: &'a [Status],
    /// Any of them; empty: any kind.
    pub kinds: &'a [String],
    /// All of them; empty: no tag needed.
    pub tags: &'a [String],
    pub limit: i64,
}

fn names(list: &[String]) -> Vec<&str> {
    list.iter().map(String::as_str).collect()
}

fn json_or_null(items: Option<Vec<&str>>) -> SqlValue {
    match items {
        Some(items) => SqlValue::Text(serde_json::to_string(&items).expect("strings serialize")),
        None => SqlValue::Null,
    }
}

/// The filter's conditions on `m`, numbered from `?{first}`, and their
/// parameters. Lists travel as one JSON parameter each, so the statement
/// text does not depend on how many values a list has.
fn conditions(filter: &Filter<'_>, first: usize) -> (String, Vec<SqlValue>) {
    let [scopes, statuses, kinds, tags] = [first, first + 1, first + 2, first + 3];
    let sql = format!(
        "(?{scopes} IS NULL OR m.scope IN (SELECT value FROM json_each(?{scopes}))) \
         AND m.status IN (SELECT value FROM json_each(?{statuses})) \
         AND (?{kinds} IS NULL OR m.kind IN (SELECT value FROM json_each(?{kinds}))) \
         AND (?{tags} IS NULL OR NOT EXISTS (SELECT 1 FROM json_each(?{tags}) w \
              WHERE w.value NOT IN (SELECT value FROM json_each(m.tags))))"
    );
    let params = vec![
        json_or_null(filter.scopes.map(names)),
        json_or_null(Some(filter.statuses.iter().map(|s| s.as_str()).collect())),
        json_or_null((!filter.kinds.is_empty()).then(|| names(filter.kinds))),
        json_or_null((!filter.tags.is_empty()).then(|| names(filter.tags))),
    ];
    (sql, params)
}

/// Ranks the memories matching any of `terms`, in one statement on the
/// store's clock:
///
/// ```text
/// relevance = -bm25(title 4.0, body 1.0, tags 2.0, scope 0.5)
/// trust     = 1 + 0.15 * min(confirmers, 4) when confirmed, 0.5 when disputed, else 1
/// recency   = 0.75 + 0.25 / (1 + age_days / 90)
/// score     = relevance * trust * recency
/// ```
///
/// `age_days` counts from `updated_at`, the last write about the memory, a
/// confirmation included.
///
/// bm25() is only defined in the query holding the MATCH, so it is read
/// there and the score is computed around it.
pub fn ranked(batch: &mut Batch, terms: &[String], filter: &Filter<'_>) -> usize {
    let (conditions, mut params) = conditions(filter, 2);
    let span = 1.0 - RECENCY_FLOOR;
    let sql = format!(
        "SELECT x.*, x.relevance \
           * (CASE x.status \
                WHEN 'confirmed' THEN 1.0 + {CONFIRMER_BOOST:?} * min(x.confirmers, {CONFIRMERS_COUNTED}) \
                WHEN 'disputed' THEN {DISPUTED_TRUST:?} ELSE 1.0 END) \
           * ({RECENCY_FLOOR:?} + {span:?} / (1.0 + max(julianday('now') - julianday(x.updated_at), 0.0) \
                                                / {RECENCY_HALF_LIFE_DAYS:?})) AS score \
         FROM (SELECT {MEMORY_COLUMNS}, h.relevance \
               FROM (SELECT rowid AS seq, \
                       -bm25(memoire_memory_fts, {TITLE_WEIGHT:?}, {BODY_WEIGHT:?}, {TAGS_WEIGHT:?}, {SCOPE_WEIGHT:?}) AS relevance \
                     FROM memoire_memory_fts WHERE memoire_memory_fts MATCH ?1) h \
               JOIN memoire_memory_memories m ON m.seq = h.seq \
               WHERE {conditions}) x \
         ORDER BY score DESC, x.updated_at DESC, x.id ASC LIMIT ?6"
    );
    params.insert(0, text(&match_any(terms)));
    params.push(int(filter.limit));
    batch.push(sql, params)
}

/// The memories passing `filter` written about most recently (`updated_at`),
/// for a recall without a query.
pub fn newest(batch: &mut Batch, filter: &Filter<'_>) -> usize {
    let (conditions, mut params) = conditions(filter, 1);
    params.push(int(filter.limit));
    batch.push(
        format!(
            "SELECT {MEMORY_COLUMNS} FROM memoire_memory_memories m WHERE {conditions} \
             ORDER BY m.updated_at DESC, m.id LIMIT ?5"
        ),
        params,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn terms_are_lowercase_words_without_repeats() {
        assert_eq!(
            terms("Deploy the API; deploy AGAIN!"),
            strings(&["deploy", "the", "api", "again"])
        );
        assert_eq!(terms(""), Vec::<String>::new());
        assert_eq!(terms("  --- ** ()"), Vec::<String>::new());
        assert_eq!(
            terms("MEREZHA_TEST_DATABASE_URL=x"),
            strings(&["merezha", "test", "database", "url", "x"])
        );
        assert_eq!(
            terms("Café ÉTÉ 東京 v1.2"),
            strings(&["café", "été", "東京", "v1", "2"])
        );
        let many: String = (0..40).map(|i| format!("w{i} ")).collect();
        assert_eq!(terms(&many).len(), MAX_TERMS);
        assert_eq!(terms(&many)[31], "w31");
    }

    #[test]
    fn hostile_text_never_reaches_query_syntax() {
        for (input, expected) in [
            (
                r#""quoted" AND (x OR y) -z title:*"#,
                r#""quoted" OR "and" OR "x" OR "or" OR "y" OR "z" OR "title""#,
            ),
            ("NEAR(a b, 2)", r#""near" OR "a" OR "b" OR "2""#),
            ("^start* col:val", r#""start" OR "col" OR "val""#),
            ("it's \"x\"\"", r#""it" OR "s" OR "x""#),
            ("naïve — façade", r#""naïve" OR "façade""#),
        ] {
            let expression = match_any(&terms(input));
            assert_eq!(expression, expected, "{input}");
            // Every quote opens or closes a term: nothing escapes one.
            assert_eq!(expression.matches('"').count() % 2, 0);
            for term in terms(input) {
                assert!(term.chars().all(char::is_alphanumeric), "{term}");
            }
        }
        assert_eq!(match_any(&strings(&["a\"b"])), "\"a\"\"b\"");
    }

    /// Titles agents wrote for the same fact twice.
    const SAME: [(&str, &str); 10] = [
        (
            "Deploys require the VPN to be connected",
            "Deploy requires VPN connection",
        ),
        (
            "Run migrations before starting the API server",
            "Run the migrations before you start the API server",
        ),
        (
            "Use pnpm, not npm, in the web workspace",
            "The web workspace uses pnpm instead of npm",
        ),
        (
            "Integration tests need MEREZHA_TEST_DATABASE_URL",
            "The integration tests need MEREZHA_TEST_DATABASE_URL set",
        ),
        (
            "Staging database password rotates every 90 days",
            "Staging DB password rotates every 90 days",
        ),
        (
            "cargo fmt must pass before pushing",
            "Run cargo fmt before pushing",
        ),
        (
            "The billing service times out after 30 seconds",
            "Billing service timeout is 30 seconds",
        ),
        (
            "Do not commit directly to main",
            "Never commit directly to the main branch",
        ),
        (
            "Flaky test: auth_login_timeout fails on CI",
            "auth_login_timeout test is flaky on CI",
        ),
        (
            "Gateway listens on port 28960 in development",
            "Dev gateway listens on port 28960",
        ),
    ];

    /// Related titles of distinct facts in one scope.
    const DIFFERENT: [(&str, &str); 9] = [
        (
            "Deploy requires VPN access",
            "Deploy pipeline caches Docker layers",
        ),
        (
            "Run migrations before starting the API server",
            "Run the API server with debug logging",
        ),
        (
            "Staging database password rotates every 90 days",
            "Production database backups run every 6 hours",
        ),
        (
            "Use pnpm in the web workspace",
            "Use cargo in the guests workspace",
        ),
        (
            "Integration tests need a PostgreSQL database",
            "Unit tests need no database",
        ),
        ("Retry flaky tests once on CI", "Never retry failed deploys"),
        ("Cache npm packages in CI", "Cache cargo registry in CI"),
        (
            "API keys expire after 90 days",
            "Session tokens expire after 30 days",
        ),
        ("Gateway listens on port 28960", "Gateway logs go to stderr"),
    ];

    fn score(a: &str, b: &str) -> f64 {
        similarity(&title_tokens(a), &title_tokens(b))
    }

    #[test]
    fn near_duplicates_collide_and_distinct_facts_do_not() {
        let mut lowest_same = f64::MAX;
        for (a, b) in SAME {
            let s = score(a, b);
            assert!(s >= DUPLICATE_THRESHOLD, "{s:.3}: {a} / {b}");
            assert_eq!(s, score(b, a));
            lowest_same = lowest_same.min(s);
        }
        let mut highest_different = 0.0_f64;
        for (a, b) in DIFFERENT {
            let s = score(a, b);
            assert!(s < DUPLICATE_THRESHOLD, "{s:.3}: {a} / {b}");
            highest_different = highest_different.max(s);
        }
        // The closest calls on each side, so a threshold change that
        // narrows the margin shows up here.
        assert!((lowest_same - 2.0 / 3.0).abs() < 1e-9, "{lowest_same}");
        assert!(
            (highest_different - 0.6).abs() < 1e-9,
            "{highest_different}"
        );
        assert_eq!(score("Deploy", "Deploy"), 1.0);
        assert_eq!(score("the of a", "the of a"), 0.0);
    }

    #[test]
    fn title_tokens_drop_stopwords_and_fold_plurals() {
        let tokens: Vec<String> = title_tokens("The deploys of this class use APIs")
            .into_iter()
            .collect();
        assert_eq!(tokens, strings(&["api", "class", "deploy", "use"]));
        assert!(title_tokens("a the of").is_empty());
    }

    #[test]
    fn filters_bind_each_list_as_one_parameter() {
        let scopes = strings(&["global", "project:x/y/z"]);
        let tags = strings(&["deploy"]);
        let filter = Filter {
            scopes: Some(&scopes),
            statuses: &Status::RECALLED,
            kinds: &[],
            tags: &tags,
            limit: 10,
        };
        let mut batch = Batch::new();
        ranked(&mut batch, &strings(&["vpn"]), &filter);
        newest(
            &mut batch,
            &Filter {
                scopes: None,
                ..filter
            },
        );
        let request = format!("{batch:?}");
        assert!(request.contains("?6"), "{request}");
        assert_eq!(batch.len(), 2);
        let (sql, params) = conditions(&filter, 1);
        assert_eq!(params.len(), 4);
        assert!(sql.contains("?4") && !sql.contains("?5"));
        assert_eq!(
            params[0],
            SqlValue::Text(r#"["global","project:x/y/z"]"#.into())
        );
        assert_eq!(
            params[1],
            SqlValue::Text(r#"["inferred","confirmed","disputed"]"#.into())
        );
        assert_eq!(params[2], SqlValue::Null);
    }
}
