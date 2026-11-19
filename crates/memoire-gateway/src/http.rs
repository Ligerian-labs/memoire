//! What every memoire route shares: the caller and its team, JSON bodies
//! with field-level errors, and the error response.
// A refusal is the rendered error response, built once on a request's
// failure path; boxing it would buy nothing.
#![allow(clippy::result_large_err)]
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};
use memoire_api::limits::MAX_REQUEST_BYTES;
use merezha_application::{Caller, Scope};
use merezha_cli::application_http::EdgeCaller;
use merezha_domain::{Failure, Identity};
use merezha_platform::{
    ErrorReply, FieldError,
    edge::Authenticated,
    ports::{Authorizer, Principal, ResourceScope},
};
use serde_json::{Map, Value};
use std::sync::Arc;

/// A refused request, already rendered.
pub type Refusal = Response;

/// The error response of `failure`, its status from the host's registry.
pub fn refuse(failure: Failure) -> Refusal {
    ErrorReply::new(failure).into_response()
}

/// `422 validation_failed` with one detail per field.
pub fn invalid(details: Vec<(String, &'static str)>) -> Refusal {
    let details = details
        .into_iter()
        .filter_map(|(path, code)| FieldError::new(path, code).ok())
        .collect();
    ErrorReply::new(Failure::permanent("validation_failed"))
        .details(details)
        .into_response()
}

/// The caller of a team route: who the edge authenticated, the team it
/// acts in, and the host policy that admitted it.
pub struct TeamCaller {
    pub principal: Arc<Principal>,
    pub team: Identity,
    policy: Arc<dyn Authorizer>,
}

impl TeamCaller {
    /// The caller for the application, which checks the route again.
    pub fn edge(&self) -> EdgeCaller {
        EdgeCaller::new(Some(self.principal.clone()), self.policy.clone())
    }

    /// The same caller for the read that answers a change it was admitted
    /// to make: a key holding only `memoire.databases.create` still gets
    /// the database it created, so no second permission is checked.
    pub fn admitted(&self) -> Admitted {
        Admitted(self.edge())
    }
}

/// A caller whose route the gateway already admitted; see
/// [`TeamCaller::admitted`].
pub struct Admitted(EdgeCaller);

impl Caller for Admitted {
    fn principal(&self, scope: &Scope) -> Option<merezha_domain::Principal> {
        self.0.principal(scope)
    }
    fn tenant(&self) -> Option<Identity> {
        self.0.tenant()
    }
    fn authenticated(&self) -> bool {
        self.0.authenticated()
    }
    fn check(&self, _: &Identity, _: &Scope) -> Result<(), Failure> {
        Ok(())
    }
}

/// The principal the edge authenticated, or `401 unauthenticated`.
pub fn principal(headers_of: &Request) -> Result<Arc<Principal>, Refusal> {
    headers_of
        .extensions()
        .get::<Authenticated>()
        .and_then(|a| a.0.clone())
        .ok_or_else(|| refuse(Failure::permanent("unauthenticated")))
}

/// Admits the request to a team route needing `permission`, in the order
/// the contract fixes: `401 unauthenticated`, `403 tenant_required`, then
/// `403 unauthorized`, before the body is read.
pub fn admit(
    request: &Request,
    policy: &Arc<dyn Authorizer>,
    permission: &str,
) -> Result<TeamCaller, Refusal> {
    let principal = principal(request)?;
    let team = principal
        .tenant
        .clone()
        .ok_or_else(|| refuse(Failure::permanent("tenant_required")))?;
    let caller = TeamCaller {
        principal,
        team,
        policy: policy.clone(),
    };
    let permission = Identity::new(permission).map_err(refuse)?;
    policy
        .check(
            Some(&caller.principal),
            &permission,
            &ResourceScope::Tenant(caller.team.clone()),
        )
        .map_err(|denied| refuse(Failure::from(denied)))?;
    Ok(caller)
}

/// A JSON object body of at most 1 MiB: `415 unsupported_media_type`
/// without a JSON content type, `413 body_limit` past the limit and
/// `422 validation_failed` (path `""`) for anything but an object. An
/// empty body reads as `{}` when `empty_ok`.
pub async fn json_object(request: Request, empty_ok: bool) -> Result<Map<String, Value>, Refusal> {
    let (parts, body) = request.into_parts();
    let declared = declared_length(&parts.headers);
    if declared.is_some_and(|n| n > MAX_REQUEST_BYTES) {
        return Err(refuse(Failure::permanent("body_limit")));
    }
    let bytes = axum::body::to_bytes(body, MAX_REQUEST_BYTES)
        .await
        .map_err(|_| refuse(Failure::permanent("body_limit")))?;
    if bytes.is_empty() && empty_ok {
        return Ok(Map::new());
    }
    if !is_json(&parts.headers) {
        return Err(refuse(Failure::permanent("unsupported_media_type")));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(invalid(vec![(String::new(), "invalid")])),
    }
}

fn declared_length(headers: &HeaderMap) -> Option<usize> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
}

/// Reads the fields of a request body one by one, collecting a detail for
/// each problem: unknown fields fail `unknown_field`, missing required ones
/// `required`. `null` is the same as an absent optional field.
pub struct Fields {
    map: Map<String, Value>,
    pub details: Vec<(String, &'static str)>,
}

impl Fields {
    pub fn new(map: Map<String, Value>, known: &[&str]) -> Self {
        let details = map
            .keys()
            .filter(|key| !known.contains(&key.as_str()))
            .map(|key| (pointer(key), "unknown_field"))
            .collect();
        Self { map, details }
    }

    /// An optional field, `None` when absent or `null`.
    pub fn optional(&self, key: &str) -> Option<&Value> {
        self.map.get(key).filter(|v| !v.is_null())
    }

    /// A required field; records `required` when absent.
    pub fn required(&mut self, key: &str) -> Option<&Value> {
        if self.optional(key).is_none() {
            self.details.push((pointer(key), "required"));
            return None;
        }
        self.optional(key)
    }

    pub fn fail(&mut self, key: &str, code: &'static str) {
        self.details.push((pointer(key), code));
    }

    /// The collected details as a refusal, when there are any.
    pub fn finish(self) -> Result<(), Refusal> {
        if self.details.is_empty() {
            Ok(())
        } else {
            Err(invalid(self.details))
        }
    }
}

/// The JSON Pointer of a top-level field, escaped per RFC 6901.
fn pointer(key: &str) -> String {
    format!("/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// Every memoire response carries `Cache-Control: no-store`.
pub async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `204 No Content`.
pub fn no_content() -> Response {
    Response::builder()
        .status(204)
        .body(Body::empty())
        .unwrap_or_default()
}

/// Unix milliseconds as an RFC 3339 UTC timestamp to the second.
pub fn timestamp(unix_ms: u64) -> String {
    i64::try_from(unix_ms / 1000)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map(|t| t.to_string())
        .unwrap_or_default()
}
