//! HTTP routes under `/v1/memoire` (docs/api.md).
use axum::{
    Json, Router, extract::Request, response::IntoResponse, response::Response, routing::get,
};
use memoire_api::{PrincipalKind, Whoami};
use merezha_domain::Failure;
use merezha_platform::edge::{Authenticated, ErrorReply};
use merezha_platform::ports::PrincipalKind as MerezhaKind;

pub fn router() -> Router {
    Router::new().route("/whoami", get(whoami))
}

/// `GET /v1/memoire/whoami`: the caller and its active team.
async fn whoami(request: Request) -> Response {
    let principal = request
        .extensions()
        .get::<Authenticated>()
        .and_then(|a| a.0.clone());
    let Some(principal) = principal else {
        return ErrorReply::new(Failure::permanent("unauthenticated")).into_response();
    };
    let kind = match principal.kind {
        MerezhaKind::User => PrincipalKind::User,
        MerezhaKind::ApiKey => PrincipalKind::ApiKey,
        MerezhaKind::System => {
            return ErrorReply::new(Failure::permanent("unauthorized")).into_response();
        }
    };
    Json(Whoami {
        subject: principal.subject.as_str().to_owned(),
        kind,
        team_id: principal.tenant.as_ref().map(|t| t.as_str().to_owned()),
    })
    .into_response()
}
