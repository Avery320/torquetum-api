use super::{Bootstrap, TorApiHost};
use axum::http::StatusCode;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::{self, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error as WsError, Message, client::IntoClientRequest},
};
use uuid::Uuid;

const WEB_ORIGIN: &str = "http://127.0.0.1:5173";
const DEADLINE: Duration = Duration::from_secs(3);
const FIXTURE_TOKEN: &str = "IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI";
type Studio = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn authority(host: &TorApiHost) -> &str {
    &host.state.authority
}

fn ticket(host: &TorApiHost) -> Bootstrap {
    host.issue_desktop_ticket(WEB_ORIGIN.into()).unwrap()
}

async fn open_socket(
    bootstrap: &Bootstrap,
    origin: &str,
    requested_host: Option<&str>,
) -> Result<Studio, WsError> {
    let mut request = bootstrap.bridge_url.as_str().into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", origin.parse().unwrap());
    if let Some(requested_host) = requested_host {
        request
            .headers_mut()
            .insert("host", requested_host.parse().unwrap());
    }
    timeout(DEADLINE, connect_async(request))
        .await
        .unwrap()
        .map(|(socket, _)| socket)
}

async fn receive(studio: &mut Studio) -> Message {
    timeout(DEADLINE, studio.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

async fn control(studio: &mut Studio, message: Value) {
    studio
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap();
}

async fn attach(bootstrap: &Bootstrap, origin: &str) -> Studio {
    let mut studio = open_socket(bootstrap, origin, None).await.unwrap();
    control(
        &mut studio,
        json!({"type":"attach", "ticket":bootstrap.ticket}),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(receive(&mut studio).await.to_text().unwrap()).unwrap(),
        json!({"type":"attached"})
    );
    studio
}

async fn register(studio: &mut Studio, instance: Uuid) {
    control(studio, json!({"type":"register", "instanceId":instance})).await;
    assert_eq!(
        serde_json::from_str::<Value>(receive(studio).await.to_text().unwrap()).unwrap(),
        json!({"type":"registered", "instanceId":instance})
    );
}

async fn start_host() -> (TorApiHost, watch::Sender<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let (shutdown, receiver) = watch::channel(());
    let host = TorApiHost::new(listener.local_addr().unwrap().port(), receiver);
    let router = host
        .router()
        .into_make_service_with_connect_info::<SocketAddr>();
    let mut closing = host.state.shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = closing.changed().await;
            })
            .await;
    });
    (host, shutdown)
}

async fn project_host() -> (TorApiHost, Studio, Uuid, watch::Sender<()>) {
    let (host, shutdown) = start_host().await;
    let mut studio = attach(&ticket(&host), WEB_ORIGIN).await;
    let instance = Uuid::new_v4();
    register(&mut studio, instance).await;
    (host, studio, instance, shutdown)
}

async fn relay(studio: &mut Studio) -> (Value, Vec<u8>) {
    let Message::Binary(bytes) = receive(studio).await else {
        panic!("expected a binary relay request");
    };
    let header_len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    let body_start = 4 + header_len;
    (
        serde_json::from_slice(&bytes[4..body_start]).unwrap(),
        bytes[body_start..].to_vec(),
    )
}

async fn respond(studio: &mut Studio, relay_id: &Value, status: u16, body: &[u8]) {
    respond_with(
        studio,
        json!({"type":"response", "relayId":relay_id, "status":status, "mediaType":"application/json"}),
        body,
    )
    .await;
}

async fn respond_with(studio: &mut Studio, header: Value, body: &[u8]) {
    let header = serde_json::to_vec(&header).unwrap();
    let mut frame = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    frame.extend_from_slice(&header);
    frame.extend_from_slice(body);
    studio.send(Message::Binary(frame.into())).await.unwrap();
}

async fn send_http(
    host: &TorApiHost,
    request_line: &str,
    requested_host: &str,
    headers: &str,
    body: &[u8],
) -> TcpStream {
    let mut stream = TcpStream::connect(authority(host)).await.unwrap();
    let mut request = format!(
        "{request_line} HTTP/1.1\r\nHost: {requested_host}\r\nConnection: close\r\n{headers}\r\n"
    )
    .into_bytes();
    request.extend_from_slice(body);
    stream.write_all(&request).await.unwrap();
    stream
}

async fn project_request(host: &TorApiHost, instance: Uuid) -> TcpStream {
    send_http(
        host,
        "GET /v0.0/project",
        authority(host),
        &format!("Authorization: Bearer {instance}.{FIXTURE_TOKEN}\r\n"),
        b"",
    )
    .await
}

async fn finish_http(mut stream: TcpStream) -> (StatusCode, String, Vec<u8>) {
    let mut received = Vec::new();
    timeout(DEADLINE, stream.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    let offset = received
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response headers");
    let headers = String::from_utf8(received[..offset].to_vec()).unwrap();
    let status = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    (
        StatusCode::from_bytes(status.as_bytes()).unwrap(),
        headers,
        received[offset + 4..].to_vec(),
    )
}

fn rejects_upgrade(result: Result<Studio, WsError>) {
    assert!(matches!(result, Err(WsError::Http(response)) if response.status().is_client_error()));
}

async fn assert_closed(studio: &mut Studio) {
    let message = timeout(DEADLINE, studio.next()).await.unwrap();
    assert!(matches!(
        message,
        Some(Ok(Message::Close(_)) | Err(_)) | None
    ));
}

#[tokio::test]
async fn ticket_is_bound_to_origin_one_use_and_expiry() {
    let (host, _shutdown) = start_host().await;
    let valid = ticket(&host);
    rejects_upgrade(open_socket(&valid, "http://other.example", None).await);
    rejects_upgrade(open_socket(&valid, WEB_ORIGIN, Some("other.example")).await);
    rejects_upgrade(open_socket(&valid, "null", None).await);
    let _studio = attach(&valid, WEB_ORIGIN).await;

    let fresh = ticket(&host);
    let mut replay = open_socket(&fresh, WEB_ORIGIN, None).await.unwrap();
    control(&mut replay, json!({"type":"attach", "ticket":valid.ticket})).await;
    assert_closed(&mut replay).await;
    let _fresh_studio = attach(&fresh, WEB_ORIGIN).await;

    let expired = ticket(&host);
    time::pause();
    time::advance(Duration::from_secs(31)).await;
    time::resume();
    rejects_upgrade(open_socket(&expired, WEB_ORIGIN, None).await);

    let desktop = host.issue_desktop_ticket("null".into()).unwrap();
    let _desktop_studio = attach(&desktop, "null").await;
}

#[tokio::test]
async fn routes_only_to_the_studio_that_owns_the_project() {
    let (host, mut owner, instance, _shutdown) = project_host().await;
    let mut other = attach(&ticket(&host), WEB_ORIGIN).await;
    let waiting = project_request(&host, instance).await;
    let (request, body) = relay(&mut owner).await;
    assert_eq!(request["instanceId"], json!(instance));
    assert_eq!(request["route"], "project");
    assert!(body.is_empty());

    respond(&mut other, &request["relayId"], 200, b"wrong socket").await;
    control(
        &mut other,
        json!({"type":"register", "instanceId":instance}),
    )
    .await;
    assert_closed(&mut other).await;
    respond(&mut owner, &request["relayId"], 200, b"owning socket").await;

    let (status, headers, body) = finish_http(waiting).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .lines()
            .any(|line| line == "cache-control: no-store")
    );
    assert_eq!(body, b"owning socket");
}

#[tokio::test]
async fn forwards_pair_body_and_blocks_web_headers_and_chunked_oversize() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../fixtures/connection.json")).unwrap();
    let (host, _shutdown) = start_host().await;
    let bootstrap = ticket(&host);
    let mut studio = attach(&bootstrap, WEB_ORIGIN).await;
    let bridge: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/bridge.json")).unwrap();
    assert_eq!(
        json!(bootstrap.max_body_bytes),
        bridge["bootstrap"]["maxBodyBytes"]
    );
    let instance = bridge["register"]["instanceId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    register(&mut studio, instance).await;
    let pair_body = fixture["pair"]["request"]["body"].to_string();
    let headers = format!(
        "Content-Type: application/json\r\nContent-Length: {}\r\n",
        pair_body.len()
    );
    let from_pair = send_http(
        &host,
        "POST /v0.0/pair",
        authority(&host),
        &format!("{headers}X-Forwarded-For: 192.0.2.1\r\nX-Foo: secret-xxxx\r\n"),
        pair_body.as_bytes(),
    )
    .await;
    let (message, forwarded) = relay(&mut studio).await;
    let mut expected = bridge["request"].clone();
    expected["relayId"] = message["relayId"].clone();
    assert_eq!(message, expected);
    assert_eq!(forwarded, pair_body.as_bytes());
    let expected_body = fixture["pair"]["response"]["body"].to_string();
    respond(
        &mut studio,
        &message["relayId"],
        200,
        expected_body.as_bytes(),
    )
    .await;
    let (status, _, body) = finish_http(from_pair).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, expected_body.as_bytes());

    let old_route = send_http(
        &host,
        "POST /v1/pair",
        authority(&host),
        &headers,
        pair_body.as_bytes(),
    )
    .await;
    assert_eq!(finish_http(old_route).await.0, StatusCode::NOT_FOUND);

    for (host_header, extra) in [
        ("other.example", String::new()),
        (authority(&host), format!("Origin: {WEB_ORIGIN}\r\n")),
    ] {
        let rejected = send_http(
            &host,
            "GET /v0.0/project",
            host_header,
            &format!("Authorization: Bearer {instance}.{FIXTURE_TOKEN}\r\n{extra}"),
            b"",
        )
        .await;
        assert_eq!(finish_http(rejected).await.0, StatusCode::BAD_REQUEST);
    }

    let preflight = send_http(
        &host,
        "OPTIONS /v0.0/pair",
        authority(&host),
        &format!("Origin: {WEB_ORIGIN}\r\nAccess-Control-Request-Method: POST\r\n"),
        b"",
    )
    .await;
    let (status, headers, _) = finish_http(preflight).await;
    assert!(status.is_client_error());
    assert!(
        !headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin")
    );

    let chunked_body = format!("1001\r\n{}\r\n0\r\n\r\n", "x".repeat(4097));
    let rejected = send_http(
        &host,
        "POST /v0.0/pair",
        authority(&host),
        "Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n",
        chunked_body.as_bytes(),
    )
    .await;
    assert_eq!(finish_http(rejected).await.0, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn unregister_disconnect_and_shutdown_release_pending_http_calls() {
    let (host, mut studio, instance, shutdown) = project_host().await;

    let waiting = project_request(&host, instance).await;
    relay(&mut studio).await;
    control(
        &mut studio,
        json!({"type":"unregister","instanceId":instance}),
    )
    .await;
    assert_eq!(
        finish_http(waiting).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    register(&mut studio, instance).await;

    let waiting = project_request(&host, instance).await;
    relay(&mut studio).await;
    drop(studio);
    assert_eq!(
        finish_http(waiting).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        finish_http(project_request(&host, instance).await).await.0,
        StatusCode::UNAUTHORIZED
    );

    let mut studio = attach(&ticket(&host), WEB_ORIGIN).await;
    register(&mut studio, instance).await;
    let waiting = project_request(&host, instance).await;
    relay(&mut studio).await;
    drop(shutdown);
    assert_closed(&mut studio).await;
    assert_eq!(
        finish_http(waiting).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn relay_timeout_releases_capacity_for_another_request() {
    let (host, mut studio, instance, _shutdown) = project_host().await;
    let mut waiting = Vec::new();
    for _ in 0..4 {
        waiting.push(project_request(&host, instance).await);
        relay(&mut studio).await;
    }
    assert_eq!(
        finish_http(project_request(&host, instance).await).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );

    time::pause();
    time::advance(Duration::from_secs(16)).await;
    time::resume();
    for request in waiting {
        assert_eq!(
            finish_http(request).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    let request = project_request(&host, instance).await;
    let (message, _) = relay(&mut studio).await;
    respond(&mut studio, &message["relayId"], 200, b"{}").await;
    assert_eq!(finish_http(request).await.0, StatusCode::OK);
}

#[tokio::test]
async fn generates_write_receipt_location_and_validates_exact_output_tag() {
    let (host, mut studio, instance, _shutdown) = project_host().await;
    let request_id = Uuid::new_v4();
    let bearer = format!("Authorization: Bearer {instance}.{FIXTURE_TOKEN}\r\n");
    let auth = format!("{bearer}X-Tor-Request-Id: {request_id}\r\n");
    let body = br#"{"schemaVersion":"0.0"}"#;
    let body_headers = format!(
        "{auth}Content-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    let input = "PUT /v0.0/asterism/inputs/fixture";
    let missing_tag = send_http(&host, input, authority(&host), &body_headers, body).await;
    assert_eq!(
        finish_http(missing_tag).await.0,
        StatusCode::PRECONDITION_REQUIRED
    );

    let headers = format!("{body_headers}If-Match: \"r-1\"\r\n");
    let location = format!("/v0.0/requests/{request_id}");
    let location_header = format!("location: {location}");
    let accepted = json!({ "requestId": request_id }).to_string();
    for _ in 0..2 {
        let client = send_http(&host, input, authority(&host), &headers, body).await;
        let (request, forwarded) = relay(&mut studio).await;
        assert_eq!(request["portId"], "fixture");
        assert_eq!(request["requestId"], json!(request_id));
        assert_eq!(request["ifMatch"], "\"r-1\"");
        assert_eq!(forwarded, body);
        respond(&mut studio, &request["relayId"], 202, accepted.as_bytes()).await;
        let (status, headers, received) = finish_http(client).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(headers.lines().any(|line| line == location_header));
        assert_eq!(received, accepted.as_bytes());
    }

    let client = send_http(
        &host,
        &format!("GET {location}"),
        authority(&host),
        &bearer,
        b"",
    )
    .await;
    let (request, _) = relay(&mut studio).await;
    assert_eq!(request["route"], "receipt");
    assert_eq!(request["requestId"], json!(request_id));
    respond(&mut studio, &request["relayId"], 200, b"{}").await;
    assert_eq!(finish_http(client).await.0, StatusCode::OK);

    let client = send_http(
        &host,
        "GET /v0.0/asterism/outputs/fixture",
        authority(&host),
        &auth,
        b"",
    )
    .await;
    let (request, _) = relay(&mut studio).await;
    assert_eq!(request["requestId"], json!(request_id));
    respond(&mut studio, &request["relayId"], 202, b"{}").await;
    let mut reply = json!({"type":"response", "relayId":request["relayId"], "status":200, "mediaType":"application/json"});
    respond_with(&mut studio, reply.clone(), b"missing receipt proof").await;
    reply["etag"] = json!("\"r-2\"");
    respond_with(&mut studio, reply, b"{}").await;
    let (status, headers, body) = finish_http(client).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.lines().any(|line| line == "etag: \"r-2\""));
    assert_eq!(body, b"{}");
}
