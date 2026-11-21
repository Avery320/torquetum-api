use std::{net::SocketAddr, sync::Arc};

use super::{HostState, SMALL_REQUEST_BYTES, TicketError, TorApiHost, unique_header};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, FromRequest, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};

#[derive(Clone)]
struct WebBootstrap {
    host: Arc<HostState>,
    dev_origin: Option<HeaderValue>,
}

impl TorApiHost {
    /// Adds the origin-checked bootstrap used by the local Studio page to attach its bridge.
    pub fn web_router(&self, dev_origin: Option<&str>) -> Router {
        self.router().merge(
            Router::new()
                .route("/internal/tor-api/bootstrap", post(bootstrap))
                .layer(DefaultBodyLimit::max(SMALL_REQUEST_BYTES))
                .with_state(WebBootstrap {
                    host: self.state.clone(),
                    dev_origin: dev_origin.and_then(local_dev_origin),
                }),
        )
    }
}

fn local_port(authority: &str) -> Option<u16> {
    let (host, port) = authority.split_once(':')?;
    let number: u16 = port.parse().ok()?;
    (matches!(host, "localhost" | "127.0.0.1") && number != 0 && port == number.to_string())
        .then_some(number)
}

fn local_dev_origin(value: &str) -> Option<HeaderValue> {
    local_port(value.strip_prefix("http://")?)?;
    HeaderValue::from_str(value).ok()
}

fn accepted_origin<'a>(
    peer: SocketAddr,
    port: u16,
    dev_origin: Option<&HeaderValue>,
    headers: &'a HeaderMap,
) -> Option<&'a HeaderValue> {
    if !peer.ip().is_loopback() {
        return None;
    }
    let host = unique_header(headers, header::HOST)?.to_str().ok()?;
    if local_port(host) != Some(port) {
        return None;
    }
    let origin = unique_header(headers, header::ORIGIN)?;
    (origin.to_str().ok()?.strip_prefix("http://") == Some(host) || dev_origin == Some(origin))
        .then_some(origin)
}

async fn bootstrap(
    State(state): State<WebBootstrap>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let origin = accepted_origin(
        peer,
        state.host.port,
        state.dev_origin.as_ref(),
        request.headers(),
    )
    .filter(|_| request.uri().query().is_none())
    .cloned();
    let mut response = if let Some(origin) = &origin {
        match Bytes::from_request(request, &()).await {
            Ok(body) if body.is_empty() => {
                let origin = origin.to_str().expect("validated local origin").to_owned();
                match state.host.issue_ticket(origin) {
                    Ok(ticket) => Json(ticket).into_response(),
                    Err(TicketError::CapacityExhausted) => {
                        StatusCode::TOO_MANY_REQUESTS.into_response()
                    }
                    Err(TicketError::EntropyUnavailable) => {
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    }
                }
            }
            Ok(_) => StatusCode::BAD_REQUEST.into_response(),
            Err(rejection) => rejection.status().into_response(),
        }
    } else {
        StatusCode::FORBIDDEN.into_response()
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    if let Some(origin) = origin {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_requires_actual_loopback_and_the_intended_origin() {
        let dev = local_dev_origin("http://127.0.0.1:5184").unwrap();
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let remote: SocketAddr = "192.0.2.3:5000".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:44584"));
        headers.insert(header::ORIGIN, dev.clone());
        headers.insert("x-forwarded-for", HeaderValue::from_static("127.0.0.1"));
        assert!(accepted_origin(remote, 44584, Some(&dev), &headers).is_none());
        assert!(accepted_origin(local, 44584, None, &headers).is_none());
        assert_eq!(
            accepted_origin(local, 44584, Some(&dev), &headers),
            Some(&dev)
        );

        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:44584"),
        );
        assert!(accepted_origin(local, 44584, None, &headers).is_none());
        headers.insert(header::HOST, HeaderValue::from_static("localhost:44584"));
        assert!(accepted_origin(local, 44584, None, &headers).is_some());
        headers.append(
            header::HOST,
            HeaderValue::from_static("other.example:44584"),
        );
        assert!(accepted_origin(local, 44584, None, &headers).is_none());
        assert!(local_dev_origin("http://sub.localhost:5184").is_none());
        assert!(local_dev_origin("http://localhost:5184/").is_none());
    }
}
