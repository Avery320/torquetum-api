//! Local HTTP transport and Studio WebSocket bridge for Torquetum.
//!
//! The host validates the transport and forwards each request to its registered
//! Project. The Studio session owns client pairing, grants, and geometry exchange.

use std::{
    collections::{HashMap, hash_map::Entry},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        ConnectInfo, DefaultBodyLimit, FromRequest, Path, Request, State, WebSocketUpgrade,
        rejection::PathRejection,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{MethodFilter, get, on},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    sync::{Semaphore, mpsc, oneshot, watch},
    time::{Instant, timeout},
};
use uuid::Uuid;

mod project_location;
mod web_bootstrap;

use project_location::ProjectLocation;

const BRIDGE_PATH: &str = "/internal/tor-api/bridge";
const API_PREFIX: &str = "/v0.0";
const RECEIPTS_PATH: &str = "/requests";
const TICKET_TTL: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const SMALL_REQUEST_BYTES: usize = 4 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const SMALL_RESPONSE_BYTES: usize = 16 * 1024;
const COLLECTION_BYTES: usize = 32 * 1024 * 1024;
const REQUEST_ID: HeaderName = HeaderName::from_static("x-tor-request-id");
const MAX_REQUESTS: usize = 4;
const MAX_CONNECTIONS: usize = 32;
const MAX_TICKETS: usize = 64;
const MAX_ROUTES: usize = 128;

pub struct TorApiHost {
    state: Arc<HostState>,
}

/// Startup data for the Studio bridge. Its one-use ticket is bound to the requesting origin.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub endpoint: String,
    pub bridge_url: String,
    pub ticket: String,
    pub max_body_bytes: usize,
}

#[derive(Debug)]
enum TicketError {
    CapacityExhausted,
    EntropyUnavailable,
}

impl TorApiHost {
    pub fn new(port: u16, shutdown: watch::Receiver<()>) -> Self {
        Self::with_project_directory(port, shutdown, project_location::local_directory())
    }

    fn with_project_directory(
        port: u16,
        shutdown: watch::Receiver<()>,
        project_directory: Option<PathBuf>,
    ) -> Self {
        Self {
            state: Arc::new(HostState {
                port,
                authority: format!("127.0.0.1:{port}"),
                project_directory,
                registry: Mutex::default(),
                requests: Semaphore::new(MAX_REQUESTS),
                connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
                shutdown,
            }),
        }
    }

    pub fn router(&self) -> Router {
        let mut routes = Router::new();
        for (path, method, route) in [
            ("/pair", MethodFilter::POST, Route::Pair),
            ("/project", MethodFilter::GET, Route::Project),
            ("/asterism/ports", MethodFilter::GET, Route::AsterismPorts),
            (
                "/asterism/outputs/{id}",
                MethodFilter::GET,
                Route::AsterismOutput,
            ),
            (
                "/asterism/inputs/{id}",
                MethodFilter::PUT,
                Route::AsterismInput,
            ),
            (
                &format!("{RECEIPTS_PATH}/{{request_id}}"),
                MethodFilter::GET,
                Route::Receipt,
            ),
        ] {
            routes = routes.route(
                path,
                on(method, move |State(state), target, request| {
                    forward(state, route, target, request)
                }),
            );
        }
        Router::new()
            .nest(API_PREFIX, routes)
            .route(BRIDGE_PATH, get(bridge))
            .layer(middleware::from_fn_with_state(
                self.state.clone(),
                local_request,
            ))
            .with_state(self.state.clone())
    }

    /// Desktop must verify the requesting WebView before asking for a ticket.
    pub fn issue_desktop_ticket(&self, origin: String) -> Result<Bootstrap, String> {
        if origin != "null" && !valid_desktop_origin(&origin) {
            return Err("Invalid Tor API bootstrap origin.".into());
        }
        self.state
            .issue_ticket(origin)
            .map_err(|error| match error {
                TicketError::CapacityExhausted => {
                    "Too many pending Tor API bootstrap tickets.".into()
                }
                TicketError::EntropyUnavailable => {
                    "Could not create a secure Tor API bootstrap ticket.".into()
                }
            })
    }
}

impl HostState {
    fn issue_ticket(&self, origin: String) -> Result<Bootstrap, TicketError> {
        let mut registry = self.registry.lock().unwrap();
        let now = Instant::now();
        registry.tickets.retain(|_, ticket| ticket.expires_at > now);
        if registry.tickets.len() >= MAX_TICKETS {
            return Err(TicketError::CapacityExhausted);
        }
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| TicketError::EntropyUnavailable)?;
        let ticket = URL_SAFE_NO_PAD.encode(random);
        registry.tickets.insert(
            ticket.clone(),
            Ticket {
                origin,
                expires_at: now + TICKET_TTL,
            },
        );
        Ok(Bootstrap {
            endpoint: format!("http://{}", self.authority),
            bridge_url: format!("ws://{}{BRIDGE_PATH}", self.authority),
            ticket,
            max_body_bytes: COLLECTION_BYTES,
        })
    }
}

fn valid_desktop_origin(origin: &str) -> bool {
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return false;
    };
    origin == format!("{scheme}://{authority}") && !authority.as_str().contains('@')
}

struct HostState {
    port: u16,
    authority: String,
    project_directory: Option<PathBuf>,
    registry: Mutex<Registry>,
    requests: Semaphore,
    connections: Arc<Semaphore>,
    shutdown: watch::Receiver<()>,
}

#[derive(Default)]
struct Registry {
    tickets: HashMap<String, Ticket>,
    routes: HashMap<Uuid, SocketRoute>,
    pending: HashMap<Uuid, Pending>,
}

struct Ticket {
    origin: String,
    expires_at: Instant,
}

struct SocketRoute {
    owner: Uuid,
    outbound: mpsc::Sender<Bytes>,
    location: Option<ProjectLocation>,
}

struct Pending {
    owner: Uuid,
    instance_id: Uuid,
    route: Route,
    request_id: Option<Uuid>,
    response: oneshot::Sender<Response>,
}

/// Releases the pending entry when the waiting HTTP relay completes, times out, or is canceled.
struct PendingLease<'a> {
    state: &'a HostState,
    relay_id: Uuid,
}

impl Drop for PendingLease<'_> {
    fn drop(&mut self) {
        self.state
            .registry
            .lock()
            .unwrap()
            .pending
            .remove(&self.relay_id);
    }
}

async fn local_request(
    State(state): State<Arc<HostState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = if peer.ip().is_loopback()
        && single_header(request.headers(), header::HOST) == Some(&state.authority)
        && request.uri().query().is_none()
    {
        next.run(request).await
    } else {
        TransportError::InvalidRequest.into_response()
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn unique_header(headers: &HeaderMap, key: HeaderName) -> Option<&HeaderValue> {
    let mut values = headers.get_all(key).into_iter();
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

fn single_header(headers: &HeaderMap, key: HeaderName) -> Option<&str> {
    unique_header(headers, key)?.to_str().ok()
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum Control {
    Attach {
        ticket: String,
    },
    Register {
        instance_id: Uuid,
        project_id: String,
    },
    Unregister {
        instance_id: Uuid,
    },
}

async fn bridge(
    State(state): State<Arc<HostState>>,
    upgrade: WebSocketUpgrade,
    headers: HeaderMap,
) -> Result<Response, TransportError> {
    let origin = single_header(&headers, header::ORIGIN).ok_or(TransportError::InvalidBridge)?;
    if !state
        .registry
        .lock()
        .unwrap()
        .tickets
        .values()
        .any(|ticket| ticket.origin == origin && ticket.expires_at > Instant::now())
    {
        return Err(TransportError::InvalidBridge);
    }
    let permit = state
        .connections
        .clone()
        .try_acquire_owned()
        .map_err(|_| TransportError::Capacity)?;
    let origin = origin.to_owned();
    let limit = 4 + MAX_HEADER_BYTES + COLLECTION_BYTES;
    Ok(upgrade
        .max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            serve_socket(state, socket, origin).await;
        }))
}

async fn serve_socket(state: Arc<HostState>, mut socket: WebSocket, origin: String) {
    let mut shutdown = state.shutdown.clone();
    let message = tokio::select! {
        _ = shutdown.changed() => return,
        message = timeout(ATTACH_TIMEOUT, socket.recv()) => message,
    };
    let Ok(Some(Ok(Message::Text(text)))) = message else {
        return;
    };
    let Some(Control::Attach { ticket }) = parse_control(&text) else {
        return;
    };
    if !consume_ticket(&state, &ticket, &origin) {
        return;
    }
    let attached = json!({ "type": "attached" });
    if !send(&mut socket, Message::Text(attached.to_string().into())).await {
        return;
    }

    let owner = Uuid::new_v4();
    let (outbound, mut requests) = mpsc::channel(MAX_REQUESTS);
    loop {
        let received = tokio::select! {
            _ = shutdown.changed() => break,
            request = requests.recv() => {
                let Some(bytes) = request else { break };
                if !send(&mut socket, Message::Binary(bytes)).await {
                    break;
                }
                continue;
            }
            received = socket.recv() => received,
        };
        match received {
            Some(Ok(Message::Text(text))) => match parse_control(&text) {
                Some(Control::Register {
                    instance_id,
                    project_id,
                }) => {
                    let Some(locator) =
                        register(&state, &origin, &project_id, instance_id, owner, &outbound)
                    else {
                        break;
                    };
                    let ack = json!({ "type": "registered", "instanceId": instance_id, "locator": locator });
                    if !send(&mut socket, Message::Text(ack.to_string().into())).await {
                        break;
                    }
                }
                Some(Control::Unregister { instance_id }) => {
                    let mut registry = state.registry.lock().unwrap();
                    if registry
                        .routes
                        .get(&instance_id)
                        .is_some_and(|route| route.owner == owner)
                    {
                        registry.routes.remove(&instance_id);
                        registry
                            .pending
                            .retain(|_, request| request.instance_id != instance_id);
                    }
                }
                _ => break,
            },
            Some(Ok(Message::Binary(frame))) => receive_response(&state, owner, frame),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => (),
            _ => break,
        }
    }
    let mut registry = state.registry.lock().unwrap();
    registry.routes.retain(|_, route| route.owner != owner);
    registry.pending.retain(|_, request| request.owner != owner);
}

fn parse_control(text: &str) -> Option<Control> {
    if text.len() > MAX_HEADER_BYTES {
        return None;
    }
    serde_json::from_str(text).ok()
}

fn consume_ticket(state: &HostState, key: &str, origin: &str) -> bool {
    let mut registry = state.registry.lock().unwrap();
    if registry
        .tickets
        .get(key)
        .is_some_and(|ticket| ticket.origin == origin && ticket.expires_at > Instant::now())
    {
        registry.tickets.remove(key);
        true
    } else {
        false
    }
}

fn register(
    state: &HostState,
    origin: &str,
    project_id: &str,
    instance_id: Uuid,
    owner: Uuid,
    outbound: &mpsc::Sender<Bytes>,
) -> Option<Option<String>> {
    if project_id.trim().is_empty() || project_id.len() > 256 {
        return None;
    }
    let mut registry = state.registry.lock().unwrap();
    let full = registry.routes.len() >= MAX_ROUTES;
    let route = match registry.routes.entry(instance_id) {
        Entry::Occupied(entry) if entry.get().owner == owner => entry.into_mut(),
        Entry::Vacant(entry) if !full => {
            let location = state.project_directory.as_ref().and_then(|directory| {
                ProjectLocation::publish(directory, origin, project_id, state.port, instance_id)
                    .ok()
            });
            entry.insert(SocketRoute {
                owner,
                outbound: outbound.clone(),
                location,
            })
        }
        _ => return None,
    };
    Some(route.location.as_ref().map(|location| location.key.clone()))
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        timeout(ATTACH_TIMEOUT, socket.send(message)).await,
        Ok(Ok(()))
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Route {
    Pair,
    Project,
    AsterismPorts,
    AsterismOutput,
    AsterismInput,
    Receipt,
}

impl Route {
    fn unauthorized(self) -> TransportError {
        if self == Self::Pair {
            TransportError::InvalidInvite
        } else {
            TransportError::PairingRequired
        }
    }

    fn response_limit(self) -> usize {
        match self {
            Self::AsterismPorts | Self::AsterismOutput => COLLECTION_BYTES,
            _ => SMALL_RESPONSE_BYTES,
        }
    }
}

#[derive(Deserialize)]
struct PairInput {
    credential: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestHeader<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    relay_id: Uuid,
    instance_id: Uuid,
    route: Route,
    #[serde(skip_serializing_if = "Option::is_none")]
    bearer: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    if_match: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_sha256: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_hash: Option<&'a str>,
}

async fn forward(
    state: Arc<HostState>,
    route: Route,
    target: Result<Option<Path<String>>, PathRejection>,
    request: Request,
) -> Result<Response, TransportError> {
    let target = target
        .map_err(|_| TransportError::InvalidRequest)?
        .map(|Path(value)| value);
    if request.headers().contains_key(header::ORIGIN) || request.method() == Method::HEAD {
        return Err(TransportError::InvalidRequest);
    }
    let _capacity = state
        .requests
        .try_acquire()
        .map_err(|_| TransportError::Capacity)?;
    timeout(REQUEST_TIMEOUT, relay(&state, route, target, request))
        .await
        .map_err(|_| TransportError::Unavailable)?
}

async fn relay(
    state: &HostState,
    route: Route,
    target: Option<String>,
    request: Request,
) -> Result<Response, TransportError> {
    let (parts, body) = request.into_parts();
    let headers = &parts.headers;
    let media_type = single_header(headers, header::CONTENT_TYPE);
    let is_pair = route == Route::Pair;
    let is_input = route == Route::AsterismInput;
    let has_body = is_pair || is_input;
    let bearer = if is_pair {
        None
    } else {
        let token = single_header(headers, header::AUTHORIZATION)
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token);
        Some(token.ok_or(TransportError::PairingRequired)?)
    };
    let instance = bearer
        .map(|token| routing_instance(token).ok_or(TransportError::PairingRequired))
        .transpose()?;
    if !is_input && headers.contains_key(&REQUEST_ID) {
        return Err(TransportError::InvalidRequest);
    }
    let request_id = match route {
        Route::Receipt => Some(parse_request_id(target.as_deref())?),
        Route::AsterismInput => Some(parse_request_id(single_header(headers, REQUEST_ID))?),
        _ => None,
    };
    let if_match = if is_input {
        if !headers.contains_key(header::IF_MATCH) {
            return Err(TransportError::PreconditionRequired);
        }
        Some(
            single_header(headers, header::IF_MATCH)
                .filter(|value| strong_etag(value))
                .ok_or(TransportError::InvalidRequest)?,
        )
    } else {
        None
    };
    if has_body
        && (!media_type.is_some_and(json_media_type)
            || headers.contains_key(header::CONTENT_ENCODING))
    {
        return Err(TransportError::UnsupportedMedia);
    }
    let limit = if is_input {
        COLLECTION_BYTES
    } else {
        SMALL_REQUEST_BYTES
    };
    if single_header(headers, header::CONTENT_LENGTH)
        .and_then(|length| length.parse::<usize>().ok())
        .is_some_and(|length| length > limit)
    {
        return Err(TransportError::TooLarge);
    }
    let mut request = Request::new(body);
    DefaultBodyLimit::max(limit).apply(&mut request);
    let body = read_body(request).await?;
    if !has_body && !body.is_empty() {
        return Err(TransportError::InvalidRequest);
    }
    let (instance_id, credential_hash) = if is_pair {
        let input: PairInput =
            serde_json::from_slice(&body).map_err(|_| TransportError::InvalidRequest)?;
        let credential = credential_bytes(&input.credential).ok_or_else(|| route.unauthorized())?;
        (
            Uuid::from_slice(&credential[..16]).ok(),
            Some(format!("{:x}", Sha256::digest(&credential[16..]))),
        )
    } else {
        (instance, None)
    };
    let instance_id = instance_id.ok_or_else(|| route.unauthorized())?;
    // Replay checks compare the original body bytes, including JSON formatting.
    let body_sha256 = is_input.then(|| format!("{:x}", Sha256::digest(&body)));
    let port_id = target.as_deref().filter(|_| route != Route::Receipt);
    let relay_id = Uuid::new_v4();
    let header = RequestHeader {
        kind: "request",
        relay_id,
        instance_id,
        route,
        bearer,
        media_type,
        port_id,
        request_id,
        if_match,
        body_sha256: body_sha256.as_deref(),
        credential_hash: credential_hash.as_deref(),
    };
    let frame = request_frame(header, &body)?;
    let (reply, receiver) = oneshot::channel();
    {
        let mut registry = state.registry.lock().unwrap();
        let target = registry
            .routes
            .get(&instance_id)
            .ok_or_else(|| route.unauthorized())?;
        let owner = target.owner;
        target
            .outbound
            .try_send(frame)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransportError::Capacity,
                mpsc::error::TrySendError::Closed(_) => TransportError::Unavailable,
            })?;
        registry.pending.insert(
            relay_id,
            Pending {
                owner,
                instance_id,
                route,
                request_id,
                response: reply,
            },
        );
    }
    let _pending = PendingLease { state, relay_id };
    receiver.await.map_err(|_| TransportError::Unavailable)
}

fn parse_request_id(value: Option<&str>) -> Result<Uuid, TransportError> {
    value
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(TransportError::InvalidRequest)
}

fn json_media_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/json"))
}

fn strong_etag(value: &str) -> bool {
    value
        .strip_prefix('"')
        .and_then(|tag| tag.strip_suffix('"'))
        .is_some_and(|tag| tag.bytes().all(|byte| matches!(byte, b'!' | b'#'..=b'~')))
}

async fn read_body(request: Request) -> Result<Bytes, TransportError> {
    Bytes::from_request(request, &())
        .await
        .map_err(|rejection| {
            if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                TransportError::TooLarge
            } else {
                TransportError::InvalidRequest
            }
        })
}

// The instance ID selects a route. The Project session validates the credential itself.
fn routing_instance(credential: &str) -> Option<Uuid> {
    Uuid::from_slice(&credential_bytes(credential)?[..16]).ok()
}

fn credential_bytes(credential: &str) -> Option<[u8; 48]> {
    if credential.len() != 64 {
        return None;
    }
    let mut bytes = [0; 48];
    URL_SAFE_NO_PAD.decode_slice(credential, &mut bytes).ok()?;
    Some(bytes)
}

// The TypeScript bridge reads a big-endian u32 header length, a UTF-8 JSON header, and the raw body.
fn request_frame(header: RequestHeader<'_>, body: &[u8]) -> Result<Bytes, TransportError> {
    let header = serde_json::to_vec(&header).map_err(|_| TransportError::InvalidRequest)?;
    if header.len() > MAX_HEADER_BYTES {
        return Err(TransportError::TooLarge);
    }
    let mut frame = Vec::with_capacity(4 + header.len() + body.len());
    frame.extend_from_slice(&(header.len() as u32).to_be_bytes());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(body);
    Ok(frame.into())
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum Reply {
    Response {
        relay_id: Uuid,
        status: u16,
        media_type: Option<String>,
        retry_after_seconds: Option<u32>,
    },
}

fn receive_response(state: &HostState, owner: Uuid, frame: Bytes) {
    let Some(prefix) = frame.first_chunk::<4>() else {
        return;
    };
    let length = u32::from_be_bytes(*prefix) as usize;
    if length > MAX_HEADER_BYTES
        || frame.len() < 4 + length
        || frame.len() - 4 - length > COLLECTION_BYTES
    {
        return;
    }
    let Ok(Reply::Response {
        relay_id,
        status,
        media_type,
        retry_after_seconds,
    }) = serde_json::from_slice(&frame[4..4 + length])
    else {
        return;
    };
    let Some(status) = StatusCode::from_u16(status)
        .ok()
        .filter(|status| (200..600).contains(&status.as_u16()))
    else {
        return;
    };
    let mut registry = state.registry.lock().unwrap();
    let Entry::Occupied(entry) = registry.pending.entry(relay_id) else {
        return;
    };
    let pending = entry.get();
    if pending.owner != owner || frame.len() - 4 - length > pending.route.response_limit() {
        return;
    }
    // Build the relative receipt URL from the admitted write's request ID at the HTTP host.
    let location = match (status, pending.route, pending.request_id) {
        (StatusCode::ACCEPTED, Route::AsterismInput, Some(id)) => {
            Some(format!("{API_PREFIX}{RECEIPTS_PATH}/{id}"))
        }
        (StatusCode::ACCEPTED, _, _) => return,
        _ => None,
    };
    if media_type
        .as_deref()
        .is_some_and(|value| !json_media_type(value))
    {
        return;
    }

    let mut response = (status, frame.slice(4 + length..)).into_response();
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CONTENT_TYPE, media_type),
        (header::LOCATION, location),
        (
            header::RETRY_AFTER,
            retry_after_seconds.map(|seconds| seconds.to_string()),
        ),
    ] {
        if let Some(value) = value {
            let Ok(value) = HeaderValue::from_str(&value) else {
                return;
            };
            headers.insert(name, value);
        }
    }
    let _ = entry.remove().response.send(response);
}

#[derive(Clone, Copy, Debug)]
enum TransportError {
    InvalidRequest,
    InvalidBridge,
    InvalidInvite,
    PairingRequired,
    UnsupportedMedia,
    PreconditionRequired,
    TooLarge,
    Capacity,
    Unavailable,
}

impl IntoResponse for TransportError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::InvalidRequest => (
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST",
                "Invalid Tor API request.",
            ),
            Self::InvalidBridge => (
                StatusCode::FORBIDDEN,
                "INVALID_REQUEST",
                "Invalid Studio bridge request.",
            ),
            Self::InvalidInvite => (
                StatusCode::UNAUTHORIZED,
                "INVITE_INVALID",
                "The Tor API invitation is unavailable.",
            ),
            Self::PairingRequired => (
                StatusCode::UNAUTHORIZED,
                "PAIRING_REQUIRED",
                "Pair again with the intended project.",
            ),
            Self::UnsupportedMedia => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "UNSUPPORTED_MEDIA_TYPE",
                "Use an uncompressed JSON request.",
            ),
            Self::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                "PRECONDITION_REQUIRED",
                "Provide the input port's strong If-Match tag.",
            ),
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "CONTENT_TOO_LARGE",
                "The request exceeds the Tor API transport limit.",
            ),
            Self::Capacity => (
                StatusCode::TOO_MANY_REQUESTS,
                "CAPACITY_EXCEEDED",
                "The Tor API transport is busy.",
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "BRIDGE_UNAVAILABLE",
                "The Studio bridge did not return a reply.",
            ),
        };
        let mut response = (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response();
        if matches!(self, Self::Capacity) {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}

#[cfg(test)]
mod tests;
