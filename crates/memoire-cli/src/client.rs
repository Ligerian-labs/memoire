//! The gateway's HTTP API (`docs/api.md`), one method per route.
use crate::config::Secret;
use crate::error::{Error, Result, exit};
use memoire_api::{
    CreateDatabase, Database, ErrorBody, ExtendDatabase, Page, QueryRequest, QueryResponse, Schema,
    Snapshot, Whoami,
};
use reqwest::{
    Method, StatusCode, Url,
    blocking::{Body, RequestBuilder, Response},
    header::{self, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    io::{Read as _, Write},
    path::Path,
    time::Duration,
};

/// Every route but export and import answers well within this; the longest
/// statement deadline is 30 seconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Export and import move up to 1 GiB.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const SQLITE_TYPE: &str = "application/vnd.sqlite3";

pub struct Client {
    http: reqwest::blocking::Client,
    base: Url,
    bearer: Option<HeaderValue>,
    /// Extra headers on every request (`Origin`, `Merezha-Organization`).
    headers: Vec<(&'static str, HeaderValue)>,
}

/// Parses and checks a gateway URL: `http` or `https`, no query or fragment.
/// A path is kept as a prefix, for a gateway served below one.
pub fn parse_server(server: &str) -> Result<Url> {
    let invalid = |why: &str| {
        Error::local(
            "invalid_server",
            exit::USAGE,
            format!("--server must be an http(s) URL such as https://memoire.example: {why}"),
        )
    };
    let url = Url::parse(server.trim()).map_err(|e| invalid(&e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("the scheme is not http or https"));
    }
    if url.host().is_none() || url.cannot_be_a_base() {
        return Err(invalid("no host"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("a query or fragment is not allowed"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("credentials do not belong in the URL"));
    }
    Ok(url)
}

impl Client {
    /// A client for `server`, authenticating with `bearer` when given.
    pub fn new(server: &str, bearer: Option<&Secret>) -> Result<Self> {
        let base = parse_server(server)?;
        let bearer = bearer
            .map(|key| {
                let mut value = HeaderValue::from_str(&format!("Bearer {}", key.expose()))
                    .map_err(|_| {
                        Error::local(
                            "invalid_key",
                            exit::AUTH,
                            "the API key has characters no key has",
                        )
                    })?;
                value.set_sensitive(true);
                Ok::<_, Error>(value)
            })
            .transpose()?;
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("memoire-cli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(REQUEST_TIMEOUT)
            // A redirect would carry the credential somewhere unplanned.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::local("client_failed", exit::FAILURE, e.to_string()))?;
        Ok(Self {
            http,
            base,
            bearer,
            headers: Vec::new(),
        })
    }

    /// The gateway's origin, `scheme://host[:port]`.
    pub fn origin(&self) -> String {
        self.base.origin().ascii_serialization()
    }

    /// The URL as given, normalized.
    pub fn server(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    /// Sends `name: value` with every request from now on.
    pub fn with_header(mut self, name: &'static str, value: &str) -> Result<Self> {
        let value = HeaderValue::from_str(value).map_err(|_| {
            Error::local(
                "invalid_argument",
                exit::USAGE,
                format!("`{value}` is not a valid {name}"),
            )
        })?;
        self.headers.push((name, value));
        Ok(self)
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("checked in parse_server")
            .pop_if_empty()
            .extend(segments);
        url
    }

    fn request(&self, method: Method, segments: &[&str]) -> RequestBuilder {
        self.authorized(self.http.request(method, self.url(segments)))
    }

    /// Adds the credential and the extra headers.
    fn authorized(&self, mut request: RequestBuilder) -> RequestBuilder {
        if let Some(bearer) = &self.bearer {
            request = request.header(header::AUTHORIZATION, bearer.clone());
        }
        for (name, value) in &self.headers {
            request = request.header(*name, value.clone());
        }
        request
    }

    /// Sends the request; any status but 2xx becomes an error.
    fn send(&self, request: RequestBuilder) -> Result<Response> {
        let response = request.send().map_err(transport)?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        Err(failure(status, response))
    }

    fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let response = self.send(request)?;
        let bytes = response.bytes().map_err(transport)?;
        serde_json::from_slice(&bytes).map_err(|e| {
            Error::local(
                "invalid_response",
                exit::FAILURE,
                format!("the gateway's answer does not follow the API: {e}"),
            )
        })
    }

    /// A request to `/v1/memoire/databases/{name}/{rest…}`.
    fn database_request(&self, method: Method, name: &str, rest: &[&str]) -> RequestBuilder {
        let mut segments = vec!["v1", "memoire", "databases", name];
        segments.extend_from_slice(rest);
        self.request(method, &segments)
    }

    pub fn whoami(&self) -> Result<Whoami> {
        self.json(self.request(Method::GET, &["v1", "memoire", "whoami"]))
    }

    /// Every database, following the cursor to the last page.
    pub fn list_databases(&self, all: bool) -> Result<Vec<Database>> {
        let state = if all { "all" } else { "active" };
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut query = vec![("state", state.to_owned()), ("limit", "100".to_owned())];
            if let Some(cursor) = cursor.take() {
                query.push(("cursor", cursor));
            }
            let mut url = self.url(&["v1", "memoire", "databases"]);
            url.query_pairs_mut().extend_pairs(&query);
            let page: Page<Database> = self.json(self.authorized(self.http.get(url)))?;
            items.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(items),
            }
        }
    }

    pub fn create_database(&self, body: &CreateDatabase) -> Result<Database> {
        self.json(
            self.request(Method::POST, &["v1", "memoire", "databases"])
                .body_json(body),
        )
    }

    pub fn database(&self, name: &str) -> Result<Database> {
        self.json(self.database_request(Method::GET, name, &[]))
    }

    pub fn delete_database(&self, name: &str) -> Result<()> {
        self.send(self.database_request(Method::DELETE, name, &[]))
            .map(drop)
    }

    pub fn extend(&self, name: &str, body: &ExtendDatabase) -> Result<Database> {
        self.json(
            self.database_request(Method::POST, name, &["extend"])
                .body_json(body),
        )
    }

    pub fn query(&self, name: &str, body: &QueryRequest) -> Result<QueryResponse> {
        self.json(
            self.database_request(Method::POST, name, &["query"])
                .body_json(body),
        )
    }

    pub fn schema(&self, name: &str) -> Result<Schema> {
        self.json(self.database_request(Method::GET, name, &["schema"]))
    }

    /// Streams the export into `out`; the number of bytes written. A read
    /// failure is the transfer's; a write failure is `out`'s, reported with
    /// `path`.
    pub fn export(&self, name: &str, out: &mut impl Write, path: &Path) -> Result<u64> {
        let mut response = self.send(
            self.database_request(Method::GET, name, &["export"])
                .timeout(TRANSFER_TIMEOUT),
        )?;
        let sqlite = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case(SQLITE_TYPE));
        if !sqlite {
            return Err(Error::local(
                "invalid_response",
                exit::FAILURE,
                "the gateway's export is not a SQLite file; is --server a memoire gateway?",
            ));
        }
        let mut buffer = vec![0; 64 * 1024];
        let mut total = 0;
        loop {
            let read = match response.read(&mut buffer) {
                Ok(0) => return Ok(total),
                Ok(read) => read,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(Error::local(
                        "transport_failed",
                        exit::UNAVAILABLE,
                        format!("the export stopped: {e}"),
                    ));
                }
            };
            out.write_all(&buffer[..read])
                .map_err(|e| crate::config::io_error(path, &e))?;
            total += read as u64;
        }
    }

    /// Uploads `len` bytes of a SQLite file.
    pub fn import(&self, name: &str, file: std::fs::File, len: u64) -> Result<Database> {
        self.json(
            self.database_request(Method::PUT, name, &["import"])
                .timeout(TRANSFER_TIMEOUT)
                .header(header::CONTENT_TYPE, SQLITE_TYPE)
                .body(Body::sized(file, len)),
        )
    }

    pub fn snapshots(&self, name: &str) -> Result<Vec<Snapshot>> {
        let page: Page<Snapshot> =
            self.json(self.database_request(Method::GET, name, &["snapshots"]))?;
        Ok(page.items)
    }

    pub fn restore(&self, name: &str, snapshot: &str) -> Result<Database> {
        self.json(self.database_request(Method::POST, name, &["snapshots", snapshot, "restore"]))
    }

    // Identity's routes (docs/api.md, "Identity routes the clients use").

    /// Signs a member in for a bearer session; its `mzs_…` token. Needs the
    /// `Origin` header of the application origin.
    pub fn sign_in(&self, email: &str, password: &Secret) -> Result<Secret> {
        #[derive(Deserialize)]
        struct Session {
            token: Secret,
        }
        #[derive(Deserialize)]
        struct SignedIn {
            session: Session,
        }
        let signed: SignedIn = self.json(
            self.request(Method::POST, &["v1", "identity", "sessions"])
                .body_json(&json!({
                    "email": email,
                    "password": password.expose(),
                    "transport": "bearer",
                })),
        )?;
        Ok(signed.session.token)
    }

    pub fn sign_out(&self) -> Result<()> {
        self.send(self.request(Method::DELETE, &["v1", "identity", "session"]))
            .map(drop)
    }

    /// Creates a machine (service account) in `team`; its object as
    /// identity returns it.
    pub fn create_service_account(&self, team: &str, name: &str) -> Result<Value> {
        #[derive(Deserialize)]
        struct Created {
            service_account: Value,
        }
        let created: Created = self.json(
            self.request(
                Method::POST,
                &["v1", "identity", "organizations", team, "service-accounts"],
            )
            .body_json(&json!({ "name": name })),
        )?;
        Ok(created.service_account)
    }

    pub fn create_api_key(&self, team: &str, body: &NewApiKey<'_>) -> Result<CreatedApiKey> {
        self.json(
            self.request(
                Method::POST,
                &["v1", "identity", "organizations", team, "api-keys"],
            )
            .body_json(body),
        )
    }
}

/// A JSON request body.
trait BodyJson {
    fn body_json(self, body: &impl Serialize) -> Self;
}

impl BodyJson for RequestBuilder {
    fn body_json(self, body: &impl Serialize) -> Self {
        self.header(header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(body).expect("request bodies serialize"))
    }
}

/// `POST /v1/identity/organizations/{org}/api-keys`.
#[derive(Serialize)]
pub struct NewApiKey<'a> {
    pub name: &'a str,
    pub permissions: &'a [&'a str],
    pub expires_at: &'a str,
    pub service_account_id: &'a str,
}

/// Its `201`: the key's metadata and, this once, the key.
#[derive(Deserialize)]
pub struct CreatedApiKey {
    pub api_key: Value,
    pub key: Secret,
}

fn transport(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::local(
            "timeout",
            exit::UNAVAILABLE,
            format!("the gateway did not answer in time: {error}"),
        )
    } else if error.is_connect() {
        Error::local(
            "connection_failed",
            exit::UNAVAILABLE,
            format!("cannot reach the gateway: {error}"),
        )
    } else {
        Error::local(
            "transport_failed",
            exit::UNAVAILABLE,
            format!("the request failed: {error}"),
        )
    }
}

/// An error response: the gateway's body when it has one.
fn failure(status: StatusCode, response: Response) -> Error {
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let body = response.bytes().ok();
    match body.and_then(|b| serde_json::from_slice::<ErrorBody>(&b).ok()) {
        Some(body) => Error::Api {
            status: status.as_u16(),
            error: Box::new(body.error),
            retry_after,
        },
        None => Error::local(
            "invalid_response",
            if status.is_server_error() {
                exit::UNAVAILABLE
            } else {
                exit::FAILURE
            },
            format!(
                "the gateway answered {status} without an error body; is --server a memoire gateway?"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_are_http_urls_without_extras() {
        for ok in [
            "https://memoire.example",
            "http://127.0.0.1:28950/",
            "https://host.example/prefix",
        ] {
            assert!(parse_server(ok).is_ok(), "{ok}");
        }
        for bad in [
            "memoire.example",
            "ftp://memoire.example",
            "https://memoire.example/?a=1",
            "https://memoire.example/#x",
            "https://user:pw@memoire.example",
        ] {
            assert_eq!(
                parse_server(bad).unwrap_err().code(),
                "invalid_server",
                "{bad}"
            );
        }
    }

    #[test]
    fn paths_join_onto_the_base_and_encode_segments() {
        let client = Client::new("https://host.example/prefix/", None).unwrap();
        assert_eq!(
            client
                .url(&[
                    "v1",
                    "memoire",
                    "databases",
                    "x1",
                    "snapshots",
                    "snap_a/b",
                    "restore"
                ])
                .as_str(),
            "https://host.example/prefix/v1/memoire/databases/x1/snapshots/snap_a%2Fb/restore"
        );
        assert_eq!(client.origin(), "https://host.example");
        assert_eq!(client.server(), "https://host.example/prefix");
    }
}
