//! The relay's HTTP API (spec §5.3). Plain HTTP; TLS is terminated in front.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::Semaphore;

use crate::challenge::{Challenges, Confirm, IssueError};
use crate::fcm::{Address, PushSender, SendError};
use crate::handle::{self, Keys, OpenError, Sealed};
use crate::limits::{Keyer, Limit, Limiter};
use crate::provisional::HANDLE_LIFETIME_SECS;
use crate::wire::*;
use crate::Clock;

pub const MAX_BODY: usize = 2048;
pub const READ_DEADLINE: Duration = Duration::from_secs(5);
pub const MAX_IN_FLIGHT: usize = 64;

/// Rate limits and map caps (spec §4.6). Starting values, not measured.
#[derive(Debug, Clone)]
pub struct Rates {
    pub map_cap: usize,
    pub wake_handle: Limit,
    pub wake_address: Limit,
    pub wake_ip: Limit,
    pub wake_global: Limit,
    pub issue_ip: Limit,
    pub issue_address: Limit,
    pub issue_global: Limit,
    pub confirm_ip: Limit,
    pub max_in_flight: usize,
}

impl Default for Rates {
    fn default() -> Rates {
        let handle = Limit { per_minute: 10, per_hour: 120 };
        Rates {
            map_cap: 4096,
            wake_handle: handle,
            wake_address: handle,
            wake_ip: Limit::hourly(600),
            wake_global: Limit::hourly(6000),
            issue_ip: Limit::hourly(10),
            issue_address: Limit::hourly(5),
            issue_global: Limit::hourly(600),
            confirm_ip: Limit::hourly(60),
            max_in_flight: MAX_IN_FLIGHT,
        }
    }
}

pub struct Options {
    pub keys: Keys,
    pub allowed_apps: Vec<String>,
    pub trusted_proxy: Option<IpAddr>,
    pub rates: Rates,
}

pub struct AppState {
    keys: Keys,
    allowed_apps: Vec<String>,
    trusted_proxy: Option<IpAddr>,
    sender: Arc<dyn PushSender>,
    clock: Arc<dyn Clock>,
    keyer: Arc<Keyer>,
    challenges: Challenges,
    wake_handle: Limiter,
    wake_address: Limiter,
    wake_ip: Limiter,
    wake_global: Limiter,
    issue_ip: Limiter,
    issue_address: Limiter,
    issue_global: Limiter,
    confirm_ip: Limiter,
    in_flight: Arc<Semaphore>,
}

impl AppState {
    pub fn new(opts: Options, sender: Arc<dyn PushSender>, clock: Arc<dyn Clock>) -> Arc<AppState> {
        let keyer = Arc::new(Keyer::new());
        let r = opts.rates;
        let l = |limit| Limiter::new(r.map_cap, limit);
        Arc::new(AppState {
            keys: opts.keys,
            allowed_apps: opts.allowed_apps,
            trusted_proxy: opts.trusted_proxy,
            sender,
            clock,
            challenges: Challenges::new(keyer.clone()),
            keyer,
            wake_handle: l(r.wake_handle),
            wake_address: l(r.wake_address),
            wake_ip: l(r.wake_ip),
            wake_global: Limiter::new(1, r.wake_global),
            issue_ip: l(r.issue_ip),
            issue_address: l(r.issue_address),
            issue_global: Limiter::new(1, r.issue_global),
            confirm_ip: l(r.confirm_ip),
            in_flight: Arc::new(Semaphore::new(r.max_in_flight)),
        })
    }

    /// Sizes of the bounded maps, for tests.
    pub fn map_sizes(&self) -> Vec<usize> {
        vec![
            self.wake_handle.len(),
            self.wake_address.len(),
            self.wake_ip.len(),
            self.issue_ip.len(),
            self.issue_address.len(),
            self.confirm_ip.len(),
            self.challenges.len(),
        ]
    }

    fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        if self.trusted_proxy == Some(peer)
            && let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
            && let Some(last) = v.rsplit(',').next()
            && let Ok(ip) = last.trim().parse::<IpAddr>()
        {
            return ip;
        }
        peer
    }

    fn ip_key(&self, ip: IpAddr) -> [u8; 16] {
        match ip {
            IpAddr::V4(a) => self.keyer.key("ip4", &a.octets()),
            // IPv6 sources are keyed by their /64.
            IpAddr::V6(a) => self.keyer.key("ip6", &a.octets()[..8]),
        }
    }

    fn addr_key(&self, kind: AddressKind, address: &str) -> [u8; 16] {
        self.keyer.key("addr", &[&[kind as u8][..], address.as_bytes()].concat())
    }

    fn binding(&self, p: Platform, app: &str, kind: AddressKind, address: &str) -> [u8; 32] {
        self.keyer.mac(&[
            b"bind",
            &[p as u8],
            app.as_bytes(),
            &[kind as u8],
            address.as_bytes(),
        ])
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/wake", post(wake))
        .route("/v1/handles", post(register))
        .route("/v1/handles/confirm", post(confirm))
        .route("/healthz", get(healthz))
        .fallback(|| async { err(StatusCode::NOT_FOUND, "not_found") })
        .with_state(state)
}

/// Serve on `listener` until the future is dropped.
pub async fn serve(listener: tokio::net::TcpListener, state: Arc<AppState>) -> std::io::Result<()> {
    axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await
}

fn err(status: StatusCode, code: &'static str) -> Response {
    let mut r = (status, axum::Json(json!({"error": code}))).into_response();
    if status == StatusCode::TOO_MANY_REQUESTS {
        r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    }
    r
}

fn hex8(k: &[u8; 16]) -> String {
    k[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// One log line per request: route, status, platform and a short tag.
fn log(route: &str, status: u16, platform: Option<Platform>, tag: Option<[u8; 16]>) {
    tracing::info!(
        route,
        status,
        platform = platform.map(Platform::as_str).unwrap_or("-"),
        tag = tag.map(|t| hex8(&t)).unwrap_or_default(),
        "request"
    );
}

async fn read_json<T: DeserializeOwned>(body: Body) -> Result<T, Response> {
    let bytes = match tokio::time::timeout(READ_DEADLINE, to_bytes(body, MAX_BODY)).await {
        Err(_) => return Err(err(StatusCode::REQUEST_TIMEOUT, "timeout")),
        Ok(Err(_)) => return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "too_large")),
        Ok(Ok(b)) => b,
    };
    serde_json::from_slice(&bytes).map_err(|_| err(StatusCode::BAD_REQUEST, "bad_request"))
}

async fn healthz() -> Response {
    axum::Json(json!({"ok": true})).into_response()
}

async fn wake(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let (res, platform, tag) = wake_inner(&st, peer.ip(), &headers, body).await;
    log("wake", res.status().as_u16(), platform, tag);
    res
}

async fn wake_inner(
    st: &AppState,
    peer: IpAddr,
    headers: &HeaderMap,
    body: Body,
) -> (Response, Option<Platform>, Option<[u8; 16]>) {
    let req: WakeRequest = match read_json(body).await {
        Ok(r) => r,
        Err(e) => return (e, None, None),
    };
    if req.v != 1 {
        return (err(StatusCode::BAD_REQUEST, "bad_request"), None, None);
    }
    let now = st.clock.now_secs();
    // Cheapest checks first, each before anything is sent.
    if !st.wake_global.check([0; 16], now) {
        return (err(StatusCode::TOO_MANY_REQUESTS, "rate_limited"), None, None);
    }
    let ip = st.client_ip(peer, headers);
    if !st.wake_ip.check(st.ip_key(ip), now) {
        return (err(StatusCode::TOO_MANY_REQUESTS, "rate_limited"), None, None);
    }
    let opened = match handle::open(&st.keys, &req.handle, now) {
        Ok(s) if st.allowed_apps.contains(&s.app) => s,
        Ok(_) | Err(OpenError::Invalid) => return (err(StatusCode::BAD_REQUEST, "invalid_handle"), None, None),
        Err(OpenError::Gone) => return (err(StatusCode::NOT_FOUND, "gone"), None, None),
    };
    // Keyed only now, so garbage handles cannot fill these maps.
    let hkey = st.keyer.key("handle", req.handle.as_bytes());
    let platform = Some(opened.platform);
    if !st.wake_handle.check(hkey, now)
        || !st.wake_address.check(st.addr_key(opened.kind, &opened.address), now)
    {
        return (err(StatusCode::TOO_MANY_REQUESTS, "rate_limited"), platform, Some(hkey));
    }
    let Ok(_permit) = st.in_flight.clone().try_acquire_owned() else {
        return (err(StatusCode::SERVICE_UNAVAILABLE, "busy"), platform, Some(hkey));
    };
    if !st.sender.supports(opened.platform) {
        return (err(StatusCode::SERVICE_UNAVAILABLE, "unavailable"), platform, Some(hkey));
    }
    let to = Address { kind: opened.kind, value: opened.address };
    let res = match st.sender.wake(&to).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(SendError::Gone) => err(StatusCode::NOT_FOUND, "gone"),
        Err(SendError::Unavailable) => err(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    };
    (res, platform, Some(hkey))
}

/// Validated registration fields.
struct Fields {
    platform: Platform,
    app: String,
    kind: AddressKind,
    address: String,
}

fn validate(st: &AppState, platform: &str, app: &str, kind: &str, address: &str) -> Result<Fields, Response> {
    let bad = || err(StatusCode::BAD_REQUEST, "bad_request");
    let platform = Platform::parse(platform).ok_or_else(|| err(StatusCode::BAD_REQUEST, "unsupported_platform"))?;
    if !st.sender.supports(platform) {
        return Err(err(StatusCode::BAD_REQUEST, "unsupported_platform"));
    }
    if !app_ok(app) || !st.allowed_apps.iter().any(|a| a == app) {
        return Err(err(StatusCode::BAD_REQUEST, "unsupported_app"));
    }
    let kind = AddressKind::parse(kind).ok_or_else(bad)?;
    if kind.platform() != platform || !address_ok(address) {
        return Err(bad());
    }
    Ok(Fields { platform, app: app.to_string(), kind, address: address.to_string() })
}

async fn register(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let (res, platform, tag) = register_inner(&st, peer.ip(), &headers, body).await;
    log("register", res.status().as_u16(), platform, tag);
    res
}

async fn register_inner(
    st: &AppState,
    peer: IpAddr,
    headers: &HeaderMap,
    body: Body,
) -> (Response, Option<Platform>, Option<[u8; 16]>) {
    let req: RegisterRequest = match read_json(body).await {
        Ok(r) => r,
        Err(e) => return (e, None, None),
    };
    let f = match validate(st, &req.platform, &req.app, &req.address_kind, &req.address) {
        Ok(f) => f,
        Err(e) => return (e, None, None),
    };
    let platform = Some(f.platform);
    let now = st.clock.now_secs();
    let ip = st.client_ip(peer, headers);
    let akey = st.addr_key(f.kind, &f.address);
    if !st.issue_global.check([0; 16], now)
        || !st.issue_ip.check(st.ip_key(ip), now)
        || !st.issue_address.check(akey, now)
    {
        return (err(StatusCode::TOO_MANY_REQUESTS, "rate_limited"), platform, None);
    }
    let binding = st.binding(f.platform, &f.app, f.kind, &f.address);
    let (id, code) = match st.challenges.issue(binding, now) {
        Ok(x) => x,
        Err(IssueError::Busy) => return (err(StatusCode::SERVICE_UNAVAILABLE, "busy"), platform, None),
        Err(IssueError::BindingCap) => {
            return (err(StatusCode::TOO_MANY_REQUESTS, "rate_limited"), platform, None);
        }
    };
    let Ok(_permit) = st.in_flight.clone().try_acquire_owned() else {
        st.challenges.cancel(&id);
        return (err(StatusCode::SERVICE_UNAVAILABLE, "busy"), platform, None);
    };
    let to = Address { kind: f.kind, value: f.address };
    let res = match st.sender.challenge(&to, &code).await {
        Ok(()) => (StatusCode::ACCEPTED, axum::Json(ChallengeIssued { challenge_id: id })).into_response(),
        Err(SendError::Gone) => {
            st.challenges.cancel(&id);
            err(StatusCode::NOT_FOUND, "gone")
        }
        Err(SendError::Unavailable) => {
            st.challenges.cancel(&id);
            err(StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        }
    };
    (res, platform, None)
}

async fn confirm(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let (res, platform) = confirm_inner(&st, peer.ip(), &headers, body).await;
    log("confirm", res.status().as_u16(), platform, None);
    res
}

async fn confirm_inner(
    st: &AppState,
    peer: IpAddr,
    headers: &HeaderMap,
    body: Body,
) -> (Response, Option<Platform>) {
    let req: ConfirmRequest = match read_json(body).await {
        Ok(r) => r,
        Err(e) => return (e, None),
    };
    let now = st.clock.now_secs();
    let ip = st.client_ip(peer, headers);
    if !st.confirm_ip.check(st.ip_key(ip), now) {
        return (err(StatusCode::TOO_MANY_REQUESTS, "rate_limited"), None);
    }
    let f = match validate(st, &req.platform, &req.app, &req.address_kind, &req.address) {
        Ok(f) => f,
        Err(e) => return (e, None),
    };
    let platform = Some(f.platform);
    let binding = st.binding(f.platform, &f.app, f.kind, &f.address);
    match st.challenges.confirm(&req.challenge_id, &req.code, &binding, now) {
        Confirm::Unknown => (err(StatusCode::NOT_FOUND, "unknown_challenge"), platform),
        Confirm::Failed => (err(StatusCode::FORBIDDEN, "challenge_failed"), platform),
        Confirm::Ok => {
            let issued = u32::try_from(now).ok();
            let expires = u32::try_from(now + HANDLE_LIFETIME_SECS).ok();
            let (Some(issued_at), Some(expires_at)) = (issued, expires) else {
                return (err(StatusCode::SERVICE_UNAVAILABLE, "unavailable"), platform);
            };
            let handle = handle::seal(
                &st.keys,
                &Sealed { platform: f.platform, kind: f.kind, app: f.app, address: f.address, issued_at, expires_at },
            );
            let expires_at = chrono::DateTime::from_timestamp(i64::from(expires_at), 0)
                .map(|d| d.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
                .unwrap_or_default();
            (axum::Json(HandleIssued { handle, expires_at }).into_response(), platform)
        }
    }
}
