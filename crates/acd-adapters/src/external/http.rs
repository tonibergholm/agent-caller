//! The producer routes. They authenticate with the hook token and answer
//! 401 for anything else, as the hook routes do.
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde::Serialize;

use super::wire::{self, AnswersResponse, PutBody};
use super::{ExternalAdapter, PutError};
use crate::hooks::endpoint::HookAuth;

const BODY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Ctx {
    adapter: Arc<ExternalAdapter>,
    auth: Arc<HookAuth>,
}

pub fn router(adapter: Arc<ExternalAdapter>, auth: Arc<HookAuth>) -> Router {
    Router::new()
        .route("/external/v1/producers/{pid}/state", any(state))
        .route("/external/v1/producers/{pid}/answers", any(answers))
        .route("/external/v1/producers/{pid}", any(stop))
        .with_state(Ctx { adapter, auth })
}

fn unauthorized() -> Response {
    StatusCode::UNAUTHORIZED.into_response()
}

fn json<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(body),
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn rejected(why: &str) -> Response {
    json(
        StatusCode::UNPROCESSABLE_ENTITY,
        &serde_json::json!({ "error": why }),
    )
}

/// The method, token and producer id, in that order. A bad method or token
/// is indistinguishable from an unknown route.
fn gate(
    ctx: &Ctx,
    method: &Method,
    wanted: &Method,
    req: &Request<Body>,
    pid: Result<Path<String>, PathRejection>,
) -> Result<String, Turned> {
    if method != wanted {
        return Err(Turned::Unauthorized);
    }
    let token = req
        .headers()
        .get("x-acd-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ctx.auth.check(token).is_none() {
        return Err(Turned::Unauthorized);
    }
    match pid {
        Ok(Path(pid)) if wire::valid_producer_id(&pid) => Ok(pid),
        _ => Err(Turned::BadProducerId),
    }
}

enum Turned {
    Unauthorized,
    BadProducerId,
}

impl IntoResponse for Turned {
    fn into_response(self) -> Response {
        match self {
            Turned::Unauthorized => unauthorized(),
            Turned::BadProducerId => {
                rejected("producer id: not a-z0-9- of 1 to 32, starting with a-z0-9")
            }
        }
    }
}

async fn state(
    State(ctx): State<Ctx>,
    pid: Result<Path<String>, PathRejection>,
    req: Request<Body>,
) -> Response {
    let pid = match gate(&ctx, req.method(), &Method::PUT, &req, pid) {
        Ok(pid) => pid,
        Err(turned) => return turned.into_response(),
    };
    let body = match tokio::time::timeout(
        BODY_TIMEOUT,
        axum::body::to_bytes(req.into_body(), wire::MAX_BODY + 1),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return rejected("body: longer than 256 KiB"),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    let body = match PutBody::parse(&body) {
        Ok(body) => body,
        Err(wire::Invalid(why)) => return rejected(&why),
    };
    match ctx.adapter.put(&pid, body) {
        Ok(response) => json(StatusCode::OK, &response),
        Err(PutError::Invalid(wire::Invalid(why))) => rejected(&why),
        Err(PutError::Stopped) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn wait_ms(query: Option<&str>) -> Option<u64> {
    let raw = query
        .into_iter()
        .flat_map(|q| q.split('&'))
        .find_map(|pair| pair.strip_prefix("wait_ms="));
    match raw {
        None => Some(0),
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|ms| *ms <= wire::MAX_WAIT_MS),
    }
}

async fn answers(
    State(ctx): State<Ctx>,
    pid: Result<Path<String>, PathRejection>,
    req: Request<Body>,
) -> Response {
    let pid = match gate(&ctx, req.method(), &Method::GET, &req, pid) {
        Ok(pid) => pid,
        Err(turned) => return turned.into_response(),
    };
    let Some(wait_ms) = wait_ms(req.uri().query()) else {
        return rejected("wait_ms: not a number up to 25000");
    };
    match ctx
        .adapter
        .answers(&pid, Duration::from_millis(wait_ms))
        .await
    {
        Some(answers) => json(StatusCode::OK, &AnswersResponse { answers }),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn stop(
    State(ctx): State<Ctx>,
    pid: Result<Path<String>, PathRejection>,
    req: Request<Body>,
) -> Response {
    let pid = match gate(&ctx, req.method(), &Method::DELETE, &req, pid) {
        Ok(pid) => pid,
        Err(turned) => return turned.into_response(),
    };
    if ctx.adapter.delete(&pid) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}
