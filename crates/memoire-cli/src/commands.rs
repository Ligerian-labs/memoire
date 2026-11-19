//! What each command does.
use crate::cli::{Cli, Command, CreateArgs, DbCommand, KeysCommand, QueryArgs, SnapshotsCommand};
use crate::client::{Client, NewApiKey};
use crate::config::{Credentials, Env, io_error};
use crate::error::{Error, Result, exit};
use crate::output::{self, Out};
use crate::{input, shell};
use memoire_api::{
    CreateDatabase, ExtendDatabase, Params, QueryMode, QueryRequest, SqlValue, Statement,
    StatementResult, Whoami,
};
use serde_json::json;
use std::path::Path;

/// The permissions a machine key gets (docs/api.md, "Authentication and the
/// team"); `--read-only` keeps only the first.
pub const PERMISSIONS: [&str; 5] = [
    "memoire.databases.read",
    "memoire.databases.write",
    "memoire.databases.create",
    "memoire.databases.delete",
    "memoire.databases.snapshots",
];

pub fn run(cli: Cli, env: &Env) -> Result<()> {
    let out = Out { json: cli.json };
    match cli.command {
        Command::Login { server, key_file } => login(&out, env, &server, key_file.as_deref()),
        Command::Whoami => whoami(&out, env),
        Command::Db(command) => db(&out, &client(env)?, command),
        Command::Query(args) => query(&out, &client(env)?, &args),
        Command::Shell { db, write } => {
            let mode = if write {
                QueryMode::Write
            } else {
                QueryMode::Read
            };
            shell::run(&out, &client(env)?, &db, mode)
        }
        Command::Export { db, output } => export(&out, &client(env)?, &db, &output),
        Command::Import { db, file } => import(&out, &client(env)?, &db, &file),
        Command::Snapshots(command) => snapshots(&out, &client(env)?, command),
        Command::Keys(KeysCommand::Create {
            name,
            team,
            expires,
            read_only,
        }) => create_key(&out, env, &name, team, expires, read_only),
    }
}

/// A client with the configured server and key.
fn client(env: &Env) -> Result<Client> {
    let (server, key) = env.login()?;
    Client::new(&server, Some(&key))
}

fn print_whoami(out: &Out, server: &str, me: &Whoami) -> Result<()> {
    if out.json {
        return out.json(&json!({
            "server": server, "subject": me.subject, "kind": me.kind, "team_id": me.team_id,
        }));
    }
    let kind = match me.kind {
        memoire_api::PrincipalKind::User => "user",
        memoire_api::PrincipalKind::ApiKey => "api_key",
    };
    out.line(&format!(
        "server   {server}\nsubject  {}\nkind     {kind}\nteam     {}",
        me.subject,
        me.team_id.as_deref().unwrap_or("none")
    ))
}

fn login(out: &Out, env: &Env, server: &str, key_file: Option<&Path>) -> Result<()> {
    // Fail on a bad URL before asking for the key.
    let server = crate::client::parse_server(server)?;
    let server = server.as_str().trim_end_matches('/').to_owned();
    let key = input::api_key(key_file, env.api_key.as_ref())?;
    let client = Client::new(&server, Some(&key))?;
    let me = client.whoami()?;
    let path = env.store(&Credentials {
        server: server.clone(),
        api_key: key,
    })?;
    if me.team_id.is_none() {
        out.note("warning: this key has no team; database commands will answer tenant_required");
    }
    out.note(&format!("Stored the key in {}", path.display()));
    print_whoami(out, &server, &me)
}

fn whoami(out: &Out, env: &Env) -> Result<()> {
    let (server, key) = env.login()?;
    let client = Client::new(&server, Some(&key))?;
    print_whoami(out, client.server(), &client.whoami()?)
}

fn db(out: &Out, client: &Client, command: DbCommand) -> Result<()> {
    match command {
        DbCommand::Create(args) => create(out, client, &args),
        DbCommand::Ls { all } => {
            let databases = client.list_databases(all)?;
            if out.json {
                out.json(&databases)
            } else if databases.is_empty() {
                out.note("No databases.");
                Ok(())
            } else {
                out.line(&output::database_table(&databases))
            }
        }
        DbCommand::Info { name } => out.database(&client.database(&name)?),
        DbCommand::Rm { name } => {
            client.delete_database(&name)?;
            if out.json {
                out.json(&json!({"name": name, "deleted": true}))
            } else {
                out.line(&format!("Deleted {name}"))
            }
        }
        DbCommand::Extend { name, ttl } => out.database(&client.extend(
            &name,
            &ExtendDatabase {
                idle_ttl_seconds: ttl,
            },
        )?),
    }
}

fn create(out: &Out, client: &Client, args: &CreateArgs) -> Result<()> {
    let db = client.create_database(&CreateDatabase {
        name: args.name.clone(),
        kind: args.kind(),
        idle_ttl_seconds: args.ttl,
    })?;
    out.database(&db)
}

/// A `-p` value: JSON in any parameter form when it parses as JSON, else
/// the text as given.
pub fn param(text: &str) -> Result<SqlValue> {
    // A number JSON cannot hold (`1e999`) still counts as meant for JSON.
    let numeric =
        text.starts_with(|c: char| c == '-' || c.is_ascii_digit()) && text.parse::<f64>().is_ok();
    let is_json = numeric || serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok();
    if !is_json {
        return Ok(SqlValue::Text(text.to_owned()));
    }
    SqlValue::from_json_text(text).map_err(|why| {
        Error::local(
            "invalid_param",
            exit::USAGE,
            format!("-p {text}: {why}; quote it as a JSON string to bind it as text"),
        )
    })
}

/// Runs one statement.
pub fn run_statement(
    client: &Client,
    db: &str,
    sql: &str,
    params: Vec<SqlValue>,
    mode: QueryMode,
) -> Result<StatementResult> {
    let response = client.query(
        db,
        &QueryRequest {
            mode,
            transaction: false,
            timeout_ms: None,
            statements: vec![Statement {
                sql: sql.to_owned(),
                params: Params::Positional(params),
            }],
        },
    )?;
    response.results.into_iter().next().ok_or_else(|| {
        Error::local(
            "invalid_response",
            exit::FAILURE,
            "the gateway returned no result for the statement",
        )
    })
}

fn query(out: &Out, client: &Client, args: &QueryArgs) -> Result<()> {
    let params = args
        .params
        .iter()
        .map(|p| param(p))
        .collect::<Result<Vec<_>>>()?;
    let result = run_statement(client, &args.db, &args.sql, params, args.mode())?;
    out.statement(&result)
}

fn export(out: &Out, client: &Client, db: &str, path: &Path) -> Result<()> {
    // Into a private temporary file beside the target, renamed over it only
    // once complete, so a failed export never leaves a partial file.
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| io_error(dir, &e))?;
    let bytes = client.export(db, &mut file)?;
    file.as_file().sync_all().map_err(|e| io_error(path, &e))?;
    file.persist(path).map_err(|e| io_error(path, &e.error))?;
    if out.json {
        out.json(&json!({"name": db, "path": path, "size_bytes": bytes}))
    } else {
        out.line(&format!(
            "Exported {db} to {} ({})",
            path.display(),
            output::size(bytes)
        ))
    }
}

fn import(out: &Out, client: &Client, db: &str, path: &Path) -> Result<()> {
    let file = std::fs::File::open(path).map_err(|e| io_error(path, &e))?;
    let len = file.metadata().map_err(|e| io_error(path, &e))?.len();
    out.database(&client.import(db, file, len)?)
}

fn snapshots(out: &Out, client: &Client, command: SnapshotsCommand) -> Result<()> {
    match command {
        SnapshotsCommand::Ls { db } => {
            let snapshots = client.snapshots(&db)?;
            if out.json {
                return out.json(&snapshots);
            }
            if snapshots.is_empty() {
                out.note("No snapshots yet; persistent databases are snapshotted every 6 hours.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = snapshots
                .iter()
                .map(|s| {
                    vec![
                        s.id.clone(),
                        s.created_at.clone(),
                        output::size(s.size_bytes),
                        match s.reason {
                            memoire_api::SnapshotReason::Scheduled => "scheduled".into(),
                            memoire_api::SnapshotReason::PreRestore => "pre_restore".into(),
                        },
                    ]
                })
                .collect();
            out.line(&output::table(&["ID", "CREATED", "SIZE", "REASON"], &rows))
        }
        SnapshotsCommand::Restore { db, id } => {
            let restored = client.restore(&db, &id)?;
            out.note(&format!("Restored {db} from {id}"));
            out.database(&restored)
        }
    }
}

/// `keys create` (docs/api.md, "Identity routes the clients use").
fn create_key(
    out: &Out,
    env: &Env,
    name: &str,
    team: Option<String>,
    expires: u64,
    read_only: bool,
) -> Result<()> {
    let team = match team {
        Some(team) => team,
        None => {
            let none = || {
                Error::local(
                    "team_required",
                    exit::USAGE,
                    "no team to create the machine in: pass --team org_…",
                )
            };
            let (server, key) = env.login().map_err(|_| none())?;
            Client::new(&server, Some(&key))?
                .whoami()?
                .team_id
                .ok_or_else(none)?
        }
    };
    let server = env.server()?;
    let expires_at = expires_at(expires)?;
    let anonymous = Client::new(&server, None)?;
    let origin = anonymous.origin();
    let (email, password) = input::sign_in()?;
    let session = anonymous
        .with_header("origin", &origin)?
        .sign_in(&email, &password)?;
    let member =
        Client::new(&server, Some(&session))?.with_header("merezha-organization", &team)?;
    let created = mint(&member, &team, name, &expires_at, read_only);
    // The session was only for this; end it whatever happened.
    if let Err(e) = member.sign_out() {
        out.note(&format!(
            "warning: could not sign the temporary session out: {}",
            e.message()
        ));
    }
    let (machine, key) = created?;
    if out.json {
        return out.json(&json!({
            "team_id": team,
            "service_account": machine,
            "api_key": key.api_key,
            "key": key.key.expose(),
        }));
    }
    out.note(&format!(
        "Created machine {name} ({}) in {team} with key {} ({}), expiring {expires_at}.\n\
         The key is shown once, below; store it now.",
        machine["id"].as_str().unwrap_or("?"),
        key.api_key["id"].as_str().unwrap_or("?"),
        if read_only {
            "read-only"
        } else {
            "read and write"
        },
    ));
    out.line(key.key.expose())
}

fn mint(
    member: &Client,
    team: &str,
    name: &str,
    expires_at: &str,
    read_only: bool,
) -> Result<(serde_json::Value, crate::client::CreatedApiKey)> {
    let machine = member.create_service_account(team, name)?;
    let machine_id = machine["id"].as_str().ok_or_else(|| {
        Error::local(
            "invalid_response",
            exit::FAILURE,
            "identity returned a service account without an id",
        )
    })?;
    let permissions = if read_only {
        &PERMISSIONS[..1]
    } else {
        &PERMISSIONS[..]
    };
    let key = member.create_api_key(
        team,
        &NewApiKey {
            name,
            permissions,
            expires_at,
            service_account_id: machine_id,
        },
    )?;
    Ok((machine, key))
}

/// Now plus `seconds`, as RFC 3339 UTC to the second.
fn expires_at(seconds: u64) -> Result<String> {
    let too_long = || Error::local("invalid_argument", exit::USAGE, "--expires is too long");
    let seconds = i64::try_from(seconds).map_err(|_| too_long())?;
    let at = time::OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .expect("0 is a valid nanosecond")
        .checked_add(time::Duration::seconds(seconds))
        .ok_or_else(too_long)?;
    at.format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| too_long())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_read_json_or_fall_back_to_text() {
        assert_eq!(param("42").unwrap(), SqlValue::Integer(42));
        assert_eq!(param("2.5").unwrap(), SqlValue::Real(2.5));
        assert_eq!(param("null").unwrap(), SqlValue::Null);
        assert_eq!(param("true").unwrap(), SqlValue::Integer(1));
        assert_eq!(param("\"42\"").unwrap(), SqlValue::Text("42".into()));
        assert_eq!(param("hello").unwrap(), SqlValue::Text("hello".into()));
        assert_eq!(param("").unwrap(), SqlValue::Text(String::new()));
        assert_eq!(
            param(r#"{"type":"blob","value":"AAE="}"#).unwrap(),
            SqlValue::Blob(vec![0, 1])
        );
        assert_eq!(
            param("9223372036854775807").unwrap(),
            SqlValue::Integer(i64::MAX)
        );
        for bad in ["[1]", r#"{"a":1}"#, "1e999", "9223372036854775808"] {
            assert_eq!(param(bad).unwrap_err().code(), "invalid_param", "{bad}");
        }
    }

    #[test]
    fn key_expiry_is_rfc3339_utc() {
        let at = expires_at(90 * 86_400).unwrap();
        assert!(at.ends_with('Z') && at.len() == 20, "{at}");
        assert!(expires_at(u64::MAX).is_err());
    }
}
