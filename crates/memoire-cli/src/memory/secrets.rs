//! A scanner that refuses text that looks like a credential before a memory is written.
//!
//! [`scan`] returns the name of the first matching pattern and never the matched text, so a
//! refusal message cannot leak the secret it refuses. The patterns are written by hand
//! (provider key prefixes, PEM headers, JWTs, `NAME=value` assignments).
//! This is a guard against accidents, not a guarantee: a secret in an unknown format passes.

/// The pattern names, in the order `scan` checks them.
pub const PATTERNS: [&str; 10] = [
    "private_key",
    "memoire_api_key",
    "memoire_session_token",
    "anthropic_api_key",
    "openai_api_key",
    "github_token",
    "aws_access_key_id",
    "slack_token",
    "jwt",
    "env_assignment",
];

/// The name of the first secret pattern found in `text`, in the order of
/// [`PATTERNS`]; `None` when it finds none. The matched text is never returned.
pub fn scan(text: &str) -> Option<&'static str> {
    let checks: [fn(&str) -> bool; 10] = [
        has_private_key,
        |t| has_prefixed(t.as_bytes(), "mzk_", 20, is_token),
        |t| has_prefixed(t.as_bytes(), "mzs_", 20, is_token),
        |t| has_prefixed(t.as_bytes(), "sk-ant-", 20, is_token),
        has_openai_key,
        has_github_token,
        has_aws_key,
        has_slack_token,
        has_jwt,
        has_env_assignment,
    ];
    PATTERNS
        .iter()
        .zip(checks)
        .find(|(_, check)| check(text))
        .map(|(name, _)| *name)
}

/// Token character: ASCII `[A-Za-z0-9_-]`.
fn is_token(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_alnum_underscore(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_alnum_hyphen(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

/// True when index `i` is the start of the text or follows a non-token byte.
fn boundary(b: &[u8], i: usize) -> bool {
    i == 0 || !is_token(b[i - 1])
}

/// For every boundary-anchored occurrence of `prefix`, the maximal run of bytes
/// accepted by `ok` that follows it.
fn runs_after<'a>(
    b: &'a [u8],
    prefix: &'a str,
    ok: fn(u8) -> bool,
) -> impl Iterator<Item = &'a [u8]> {
    (0..b.len())
        .filter(move |&i| boundary(b, i) && b[i..].starts_with(prefix.as_bytes()))
        .map(move |i| {
            let rest = &b[i + prefix.len()..];
            let n = rest.iter().take_while(|&&c| ok(c)).count();
            &rest[..n]
        })
}

fn has_prefixed(b: &[u8], prefix: &str, min: usize, ok: fn(u8) -> bool) -> bool {
    runs_after(b, prefix, ok).any(|run| run.len() >= min)
}

/// A `-----BEGIN ` marker followed on the same line by `PRIVATE KEY-----`.
fn has_private_key(text: &str) -> bool {
    const BEGIN: &str = "-----BEGIN ";
    text.split('\n').any(|line| {
        line.find(BEGIN)
            .is_some_and(|at| line[at + BEGIN.len()..].contains("PRIVATE KEY-----"))
    })
}

fn has_openai_key(text: &str) -> bool {
    runs_after(text.as_bytes(), "sk-", is_token)
        .any(|run| run.len() >= 20 && run.iter().any(u8::is_ascii_digit))
}

fn has_github_token(text: &str) -> bool {
    let b = text.as_bytes();
    let short = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
    short.iter().any(|p| has_prefixed(b, p, 30, is_alnum))
        || has_prefixed(b, "github_pat_", 30, is_alnum_underscore)
}

/// `AKIA` or `ASIA` plus exactly 16 `[A-Z0-9]`, not embedded in a longer alphanumeric run.
fn has_aws_key(text: &str) -> bool {
    let b = text.as_bytes();
    (0..b.len()).any(|i| {
        let head = b[i..].starts_with(b"AKIA") || b[i..].starts_with(b"ASIA");
        let end = i + 20;
        head && end <= b.len()
            && (i == 0 || !is_alnum(b[i - 1]))
            && b[i + 4..end]
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && b.get(end).is_none_or(|&c| !is_alnum(c))
    })
}

fn has_slack_token(text: &str) -> bool {
    ["xoxa-", "xoxb-", "xoxp-", "xoxr-", "xoxs-"]
        .iter()
        .any(|p| has_prefixed(text.as_bytes(), p, 10, is_alnum_hyphen))
}

/// Consume at least `min` token bytes and a dot; return what follows the dot.
fn jwt_segment(b: &[u8], min: usize) -> Option<&[u8]> {
    let n = b.iter().take_while(|&&c| is_token(c)).count();
    if n < min {
        return None;
    }
    b[n..].strip_prefix(b".")
}

/// A header that starts `eyJ` (base64 of `{"`), a payload of at least two
/// characters (`e30` is `{}`, claims that are empty), and a signature of at
/// least 10.
fn has_jwt(text: &str) -> bool {
    let b = text.as_bytes();
    (0..b.len()).filter(|&i| boundary(b, i)).any(|i| {
        b[i..]
            .strip_prefix(b"eyJ")
            .and_then(|header| jwt_segment(header, 10))
            .and_then(|payload| jwt_segment(payload, 2))
            .is_some_and(|sig| sig.iter().take_while(|&&c| is_token(c)).count() >= 10)
    })
}

fn has_env_assignment(text: &str) -> bool {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .any(is_secret_assignment)
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Parse `[export ]NAME = VALUE` and decide whether VALUE looks like a real secret.
fn is_secret_assignment(line: &str) -> bool {
    let mut rest = line.trim_start_matches(is_blank);
    if let Some(after) = rest.strip_prefix("export")
        && after.starts_with(is_blank)
    {
        rest = after.trim_start_matches(is_blank);
    }
    let name_len = rest
        .bytes()
        .take_while(|&c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        .count();
    let (name, after) = rest.split_at(name_len);
    if !is_secret_name(name) {
        return false;
    }
    let Some(value) = after.trim_start_matches(is_blank).strip_prefix('=') else {
        return false;
    };
    assigned_value(value.trim_start_matches(is_blank))
        .is_some_and(|(inner, quoted)| is_secret_value(inner, quoted))
}

/// The value of an assignment and whether it was quoted, when the rest of
/// the line is nothing or a `#` comment; `None` for prose. A quoted value
/// runs to its closing quote, an unquoted one to the first whitespace.
fn assigned_value(value: &str) -> Option<(&str, bool)> {
    let only_comment = |rest: &str| {
        let rest = rest.trim_start_matches(is_blank);
        rest.is_empty() || rest.starts_with('#')
    };
    if let Some(quote) = value.chars().next().filter(|q| matches!(q, '"' | '\'')) {
        let inner = &value[1..];
        if let Some(end) = closing_quote(inner, quote) {
            let rest = &inner[end + 1..];
            // A comment needs whitespace before its `#`.
            let commented = rest.is_empty() || (rest.starts_with(is_blank) && only_comment(rest));
            return commented.then_some((&inner[..end], true));
        }
    }
    let end = value.find(char::is_whitespace).unwrap_or(value.len());
    only_comment(&value[end..]).then_some((&value[..end], false))
}

/// Where `quote` closes a quoted value that started just before `inner`:
/// in double quotes a backslash escapes the next character, so `\"` does
/// not close it; single quotes have no escapes.
fn closing_quote(inner: &str, quote: char) -> Option<usize> {
    if quote == '\'' {
        return inner.find(quote);
    }
    let mut escaped = false;
    for (at, c) in inner.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(at),
            _ => {}
        }
    }
    None
}

fn is_secret_name(name: &str) -> bool {
    !name.starts_with(|c: char| c.is_ascii_digit())
        && ["SECRET", "TOKEN", "PASSWORD", "API_KEY"]
            .iter()
            .any(|k| name.contains(k))
}

fn is_secret_value(inner: &str, quoted: bool) -> bool {
    inner.chars().count() >= 8
        && (quoted || !inner.contains(char::is_whitespace))
        && !is_placeholder(inner)
        && !is_plain_value(inner)
}

/// Limits, file paths and URLs are configuration, not credentials: all digits,
/// a leading `/`, `~`, `./` or `../`, or a `://` anywhere.
fn is_plain_value(value: &str) -> bool {
    value.bytes().all(|c| c.is_ascii_digit())
        || value.starts_with(['/', '~'])
        || value.starts_with("./")
        || value.starts_with("../")
        || value.contains("://")
}

fn is_placeholder(value: &str) -> bool {
    const MARKERS: [&str; 10] = [
        "changeme",
        "change-me",
        "change_me",
        "redacted",
        "example",
        "placeholder",
        "your",
        "todo",
        "dummy",
        "secret-here",
    ];
    let lower = value.to_ascii_lowercase();
    value.starts_with(['$', '<', '{', '['])
        || value.contains("...")
        || value.contains('…')
        || value.chars().all(|c| "xX*.-_".contains(c))
        || MARKERS.iter().any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic alphanumeric filler of `n` characters.
    fn body(n: usize) -> String {
        "a1B2c3D4e5F6g7H8".chars().cycle().take(n).collect()
    }

    /// Prefix (kept in pieces by callers) plus a filler body.
    fn tok(prefix: &str, n: usize) -> String {
        prefix.to_owned() + &body(n)
    }

    fn mzk() -> String {
        tok(&["mz", "k_"].concat(), 24)
    }

    fn mzs() -> String {
        tok(&["mz", "s_"].concat(), 24)
    }

    fn anthropic() -> String {
        tok(&["sk", "-ant-"].concat(), 30)
    }

    fn openai() -> String {
        tok(&["s", "k-"].concat(), 30)
    }

    fn github(prefix: &str) -> String {
        tok(&[prefix, "_"].concat(), 36)
    }

    fn github_pat() -> String {
        tok(&["github", "_pat_"].concat(), 40)
    }

    fn aws(prefix: &str) -> String {
        [prefix, "IA", "ABCD1234EFGH5678"].concat()
    }

    fn slack(letter: char) -> String {
        ["xo", "x", &letter.to_string(), "-1234567890ab"].concat()
    }

    fn jwt() -> String {
        let seg = tok(&["ey", "J"].concat(), 14);
        format!("{seg}.{seg}.{}", body(16))
    }

    fn pem(kind: &str) -> String {
        ["-----BEGIN ", kind, "PRIVATE", " KEY-----"].concat()
    }

    fn assign(name: &str, value: &str) -> String {
        format!("{name}={value}")
    }

    fn embed(secret: &str) -> String {
        format!("Notes from today: remember {secret} for later, thanks.")
    }

    fn check(name: &str, samples: &[String]) {
        for s in samples {
            assert_eq!(scan(&embed(s)), Some(name), "embedded: {name}");
            assert_eq!(scan(s), Some(name), "bare: {name}");
        }
    }

    #[test]
    fn detects_private_keys() {
        let kinds = ["", "RSA ", "OPENSSH ", "EC "];
        let samples: Vec<String> = kinds.iter().map(|k| pem(k)).collect();
        check("private_key", &samples);
    }

    #[test]
    fn private_key_needs_both_parts_on_one_line() {
        let split = ["-----BEGIN \n", "PRIVATE", " KEY-----"].concat();
        assert_eq!(scan(&split), None);
    }

    #[test]
    fn detects_memoire_keys_and_tokens() {
        check("memoire_api_key", &[mzk(), mzk() + "-_x"]);
        check("memoire_session_token", &[mzs(), mzs() + "_-y"]);
    }

    #[test]
    fn detects_anthropic_and_openai_keys() {
        check("anthropic_api_key", &[anthropic(), anthropic() + "-z9"]);
        check("openai_api_key", &[openai(), openai() + "-b8"]);
    }

    #[test]
    fn detects_github_tokens() {
        let mut samples: Vec<String> = ["ghp", "gho", "ghu", "ghs", "ghr"]
            .iter()
            .map(|p| github(p))
            .collect();
        samples.push(github_pat());
        check("github_token", &samples);
    }

    #[test]
    fn detects_aws_access_key_ids() {
        check("aws_access_key_id", &[aws("AK"), aws("AS")]);
    }

    #[test]
    fn detects_slack_tokens() {
        let samples: Vec<String> = "abprs".chars().map(slack).collect();
        check("slack_token", &samples);
    }

    #[test]
    fn detects_jwts() {
        check("jwt", &[jwt(), format!("Authorization: Bearer {}", jwt())]);
        // Empty claims: the payload is `e30`, base64 of `{}`.
        let header = tok(&["ey", "J"].concat(), 14);
        let empty_claims = format!("{header}.{}.{}", ["e3", "0"].concat(), body(16));
        check("jwt", &[empty_claims]);
        let one_char = format!("{header}.e.{}", body(16));
        assert_eq!(scan(&one_char), None, "a payload of one character");
        let short_sig = format!("{header}.{}.{}", ["e3", "0"].concat(), body(9));
        assert_eq!(scan(&short_sig), None, "a signature under 10");
    }

    #[test]
    fn detects_env_assignments() {
        let v = body(20);
        assert_eq!(scan("API_KEY=12345678abcdef"), Some("env_assignment"));
        assert_eq!(scan("PASSWORD=a/b/c/d/e/f/g/h"), Some("env_assignment"));
        let samples = [
            assign("API_KEY", &v),
            format!("export {}", assign("DB_PASSWORD", &v)),
            assign("CLIENT_SECRET", &format!("\"{v}\"")),
            assign("AUTH_TOKEN", &format!("'{v}'")),
            assign("PASSWORD", "\"correct horse battery staple\""),
            format!("SECRET_KEY = {v}"),
            format!("    \tAPI_KEY={v}"),
            format!("export\tAPI_KEY  =\t{v}  "),
            format!("MY_API_KEY_2={v}"),
        ];
        for s in &samples {
            assert_eq!(scan(s), Some("env_assignment"), "{s}");
        }
        // An escaped quote does not end a double-quoted value.
        let escaped = format!("\"{}\\\"{}\"", body(2), body(10));
        assert_eq!(scan(&assign("PASSWORD", &escaped)), Some("env_assignment"));
        let commented = format!("{} # prod", assign("API_KEY", &escaped));
        assert_eq!(scan(&commented), Some("env_assignment"));
        // A trailing comment does not hide the value.
        for line in [
            format!("{} # prod", assign("API_KEY", &v)),
            format!("{}\t#prod", assign("API_KEY", &v)),
            format!("{} #", assign("DB_PASSWORD", &v)),
            format!(
                "{}  # from the vault",
                assign("CLIENT_SECRET", &format!("\"{v}\""))
            ),
            format!("{} #x", assign("AUTH_TOKEN", &format!("'{v}'"))),
            format!("export {}  # dev", assign("API_KEY", &v)),
        ] {
            assert_eq!(scan(&line), Some("env_assignment"), "{line}");
        }
        let crlf = format!("first line\r\n{}\r\nlast line\r\n", assign("API_KEY", &v));
        assert_eq!(scan(&crlf), Some("env_assignment"));
        let tok_line = assign("TOKEN", &v);
        let fenced = format!("Config:\n```sh\nexport PATH=/bin\n{tok_line}\n```\nDone.");
        assert_eq!(scan(&fenced), Some("env_assignment"));
    }

    #[test]
    fn reports_first_pattern_in_order() {
        let both = format!("{}\n{}", assign("API_KEY", &body(20)), github("ghp"));
        assert_eq!(scan(&both), Some("github_token"));
        assert_eq!(scan(&anthropic()), Some("anthropic_api_key"));
        let mixed = format!("{} {}", jwt(), pem(""));
        assert_eq!(scan(&mixed), Some("private_key"));
    }

    #[test]
    fn prose_and_code_pass() {
        let sha = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        let uuid = "123e4567-e89b-12d3-a456-426614174000";
        let rust = "fn main() {\n    let n = 3;\n    println!(\"{n}\");\n}\n";
        let shell = "#!/bin/sh\nset -eu\nfor f in *.txt; do\n  echo \"$f\"\ndone\n";
        let md = "See [the docs](https://example.org/docs/start?x=1) for details.\n";
        let no_digit = ["s", "k-", "abcdefghij-klmnopqrst-uvwxyz-abcd"].concat();
        let short_ghp = tok(&["gh", "p_"].concat(), 10);
        let aws15 = tok(&["AK", "IA"].concat(), 0) + "ABCD1234EFGH567";
        let aws17 = aws("AK") + "Z";
        let glued = "foo".to_owned() + &aws("AS");
        let multibyte = "café naïve 日本語のテキスト 🦀🎉 Привет";
        let cases: Vec<&str> = vec![
            "API keys start with mzk_ and sessions with mzs_.",
            "Use sk-ant- keys for Anthropic and sk- keys for OpenAI.",
            "risk-management-framework-2026-edition-final",
            "task-force-alpha-bravo-charlie-delta-2",
            &no_digit,
            "mzk_fake_member_key",
            &short_ghp,
            &aws15,
            &aws17,
            &glued,
            "Set MEMOIRE_API_KEY and MEMOIRE_SERVER before running the agent.",
            "MEMOIRE_API_KEY=$MEMOIRE_API_KEY",
            "API_KEY=${API_KEY}",
            "PASSWORD=<your password>",
            "TOKEN=…",
            "SECRET_KEY=xxxxxxxxxxxx",
            "DB_PASSWORD=changeme123",
            "API_KEY=your-key-here",
            "PASSWORD=",
            "TOKEN=short",
            "TOKEN = the value printed by login",
            "MAX_TOKENS=4096 is plenty",
            "MAX_TOKENS=4096 # plenty",
            "API_KEY=\"quoted words\" and then prose",
            "API_KEY=\"no space before\"#comment",
            "API_KEY=$API_KEY # from the environment",
            // In single quotes a backslash is a character: the value ends
            // at the second quote, and prose follows it.
            "API_KEY='abc\\' then words about it",
            "PASSWORD=\"short\\\"\" # quoted",
            "MAX_TOKENS=4096",
            "let password = read_password()?;",
            "token = client.fetch_token(scope)",
            rust,
            shell,
            md,
            sha,
            uuid,
            "-----BEGIN CERTIFICATE-----",
            "eyJ is how base64 JSON starts",
            "version 1.98.0-rc.1 or 12.4.7.3",
            "",
            multibyte,
        ];
        for text in cases {
            assert_eq!(scan(text), None, "{text}");
        }
    }

    #[test]
    fn placeholders_and_prose_values_pass() {
        for value in [
            "\"<insert value>\"",
            "[redacted-value]",
            "abc...xyz123",
            "********",
            "-_-_-_-_",
            "an-Example-value",
            "'put TODO here ok'",
            "SECRET-HERE-please",
            "unquoted words here",
            "40960000",
            "/run/secrets/app-token",
            "~/.config/app/key.txt",
            "./secrets/password.txt",
            "../secrets/password.txt",
            "'https://auth.internal.test/oauth/token'",
        ] {
            assert_eq!(scan(&assign("API_KEY", value)), None, "{value}");
        }
        for line in [
            "MAX_TOKENS=40960000",
            "TOKEN_FILE=/run/secrets/app-token",
            "API_KEY_PATH=~/.config/app/key.txt",
            "PASSWORD_FILE=./secrets/password.txt",
            "TOKEN_URL=https://auth.example.org/oauth/token",
        ] {
            assert_eq!(scan(line), None, "{line}");
        }
        assert_eq!(scan("1TOKEN=abcdefgh12345"), None);
        assert_eq!(scan("TOKEN abcdefgh12345"), None);
    }

    #[test]
    fn aws_ids_respect_both_sides() {
        let id = aws("AK");
        assert_eq!(scan(&format!("id={id};")), Some("aws_access_key_id"));
        assert_eq!(scan(&format!("_{id}_")), Some("aws_access_key_id"));
        assert_eq!(scan(&format!("a{id}")), None);
        assert_eq!(scan(&format!("{id}a")), None);
    }

    #[test]
    fn prefixed_patterns_respect_boundaries() {
        let tokens = [
            ("memoire_api_key", mzk()),
            ("memoire_session_token", mzs()),
            ("anthropic_api_key", anthropic()),
            ("openai_api_key", openai()),
            ("github_token", github("gho")),
            ("github_token", github_pat()),
            ("slack_token", slack('b')),
            ("jwt", jwt()),
        ];
        for (name, t) in &tokens {
            assert_eq!(scan(t), Some(*name), "start");
            assert_eq!(scan(&format!("text {t}")), Some(*name), "end");
            for lead in ["(", "\"", "=", " ", "\n"] {
                assert_eq!(scan(&format!("a{lead}{t}")), Some(*name), "after {lead:?}");
            }
            assert_eq!(scan(&format!("x{t}")), None, "glued: {name}");
            assert_eq!(scan(&format!("x_{t}")), None, "glued underscore: {name}");
            assert_eq!(scan(&format!("a-{t}")), None, "glued hyphen: {name}");
        }
    }

    #[test]
    fn never_panics_on_odd_input() {
        let long = "é".repeat(50_000) + &"sk-".repeat(20_000) + &"\r\n".repeat(1000);
        assert_eq!(scan(&long), None);
        let tail = format!("{}é", ["ey", "J"].concat());
        assert_eq!(scan(&tail), None);
        assert_eq!(scan("export \u{a0}TOKEN=\"é"), None);
        assert_eq!(scan("API_KEY=\"é"), None);
    }
}
