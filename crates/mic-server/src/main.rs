use axum::{
    Json, Router,
    extract::{Path, Query, Request, State, WebSocketUpgrade, ws::Message as WsMessage},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
};
use futures_util::SinkExt;
use mic_network::{Action, Config, Core, validate_audio};
use mic_protocol::Source;
use mic_telemetry::{Range, valid_device_id};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
#[derive(Clone)]
struct App {
    core: Core,
    token: String,
    origins: Arc<Vec<String>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
}
type ApiResult = Result<Json<Value>, ApiError>;
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(json!({"error":{"code":self.0.as_u16(),"message":self.1}})),
        )
            .into_response()
    }
}
fn bad(s: impl ToString) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, s.to_string())
}
fn core_error(error: String) -> ApiError {
    let status = if error == "device not found" {
        StatusCode::NOT_FOUND
    } else if error.contains("timeout") || error.contains("timed out") {
        StatusCode::GATEWAY_TIMEOUT
    } else if error.contains("in progress")
        || error.contains("not connected")
        || error.contains("another device")
    {
        StatusCode::CONFLICT
    } else {
        StatusCode::BAD_REQUEST
    };
    ApiError(status, error)
}
async fn guard(State(app): State<App>, req: Request, next: Next) -> Response {
    let origin = req.headers().get("origin").and_then(|v| v.to_str().ok());
    if let Some(o) = origin
        && !app.origins.iter().any(|s| s == o)
    {
        return ApiError(StatusCode::FORBIDDEN, "origin rejected".into()).into_response();
    }
    if let Some(host) = req.headers().get("host").and_then(|h| h.to_str().ok())
        && !(host.starts_with("127.0.0.1:") || host.starts_with("localhost:"))
    {
        return ApiError(StatusCode::FORBIDDEN, "host rejected".into()).into_response();
    }
    let mutation = !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    );
    if mutation
        && req
            .headers()
            .get("x-mic-token")
            .and_then(|s| s.to_str().ok())
            != Some(&app.token)
    {
        return ApiError(StatusCode::FORBIDDEN, "missing or invalid API token".into())
            .into_response();
    }
    if req.method() == axum::http::Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Some(o) = origin {
            response.headers_mut().insert(
                "access-control-allow-origin",
                HeaderValue::from_str(o).unwrap(),
            );
            response.headers_mut().insert(
                "access-control-allow-methods",
                HeaderValue::from_static("GET,POST,PATCH,PUT,OPTIONS"),
            );
            response.headers_mut().insert(
                "access-control-allow-headers",
                HeaderValue::from_static("content-type,x-mic-token"),
            );
        }
        return response;
    }
    let origin = origin.map(str::to_string);
    let mut response = next.run(req).await;
    if let Some(o) = origin {
        response.headers_mut().insert(
            "access-control-allow-origin",
            HeaderValue::from_str(&o).unwrap(),
        );
        response
            .headers_mut()
            .insert("vary", HeaderValue::from_static("Origin"));
    }
    response
}
async fn status(State(a): State<App>) -> Json<Value> {
    Json(
        json!({"service":"online","protocol_version":1,"sample_rate":16000,"frame_ms":10,"api_token":a.token}),
    )
}
async fn devices(State(a): State<App>) -> Json<Value> {
    Json(json!(a.core.snapshot.read().await.devices))
}
async fn metrics(State(a): State<App>) -> Json<Value> {
    Json(json!(a.core.snapshot.read().await.metrics))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    code: String,
    #[serde(default)]
    reject: bool,
}
async fn pair(
    State(a): State<App>,
    Path(id): Path<String>,
    body: Result<Json<Pair>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(b) = body.map_err(bad)?;
    a.core
        .command(
            id,
            if b.reject {
                Action::Reject
            } else {
                Action::Pair(b.code)
            },
        )
        .await
        .map_err(core_error)?;
    Ok(Json(json!({"ok":true})))
}
async fn disconnect(State(a): State<App>, Path(id): Path<String>) -> ApiResult {
    a.core
        .command(id, Action::Disconnect)
        .await
        .map_err(core_error)?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Audio {
    source: Option<Source>,
    gain: Option<f32>,
    mute: Option<bool>,
    streaming: Option<bool>,
}
async fn audio(
    State(a): State<App>,
    Path(id): Path<String>,
    body: Result<Json<Audio>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(b) = body.map_err(bad)?;
    validate_audio(b.gain).map_err(bad)?;
    a.core
        .command(
            id,
            Action::Audio {
                source: b.source,
                gain: b.gain,
                mute: b.mute,
                streaming: b.streaming,
            },
        )
        .await
        .map_err(core_error)?;
    Ok(Json(json!({"ok":true})))
}
async fn outputs() -> ApiResult {
    let result = tokio::task::spawn_blocking(mic_audio::Playback::outputs)
        .await
        .map_err(bad)?
        .map_err(bad)?;
    Ok(Json(json!(result)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    id: String,
}
async fn output(
    State(a): State<App>,
    body: Result<Json<Output>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(b) = body.map_err(bad)?;
    let playback = a.core.playback.clone();
    let name = tokio::task::spawn_blocking(move || playback.select(&b.id))
        .await
        .map_err(bad)?
        .map_err(bad)?;
    Ok(Json(json!({"name":name})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    device_id: String,
    range: String,
}
async fn history(
    State(a): State<App>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult {
    let Query(q) = query.map_err(bad)?;
    if !valid_device_id(&q.device_id) {
        return Err(bad("invalid device_id"));
    }
    let range = Range::parse(&q.range).ok_or_else(|| bad("range must be 60s, 5m or 15m"))?;
    let history = a
        .core
        .telemetry
        .history(&q.device_id, range)
        .await
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "device telemetry not found".into()))?;
    Ok(Json(json!(history)))
}
async fn diagnostics(State(a): State<App>) -> Json<Value> {
    Json(
        json!({"continuity":a.core.playback.diagnostics.snapshot(),"capture":a.core.playback.capture.status()}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticsOptions {
    enabled: bool,
}
async fn diagnostics_options(
    State(a): State<App>,
    Json(b): Json<DiagnosticsOptions>,
) -> Json<Value> {
    a.core.playback.diagnostics.set_enabled(b.enabled);
    Json(json!({"ok":true}))
}
async fn capture_start(State(a): State<App>) -> ApiResult {
    let capture = a.core.playback.capture.clone();
    let status = tokio::task::spawn_blocking(move || capture.start())
        .await
        .map_err(bad)?
        .map_err(core_error)?;
    Ok(Json(json!(status)))
}
async fn capture_stop(State(a): State<App>) -> ApiResult {
    let capture = a.core.playback.capture.clone();
    let status = tokio::task::spawn_blocking(move || capture.stop())
        .await
        .map_err(bad)?
        .map_err(core_error)?;
    Ok(Json(json!(status)))
}
async fn events(State(a): State<App>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(4096).max_frame_size(4096).on_upgrade(move |mut socket| async move {
  let mut timer=tokio::time::interval(Duration::from_millis(500));
  let mut shutdown=a.shutdown.clone();
  let mut telemetry=a.core.telemetry.notifications.subscribe();
  loop {
   tokio::select! {
    _=shutdown.changed()=>{let _=socket.send(WsMessage::Close(None)).await;break;},
    notification=telemetry.recv()=>{
     let message=match notification {
      Ok(note)=>serde_json::to_string(&note).unwrap(),
      Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>json!({"kind":"resync_required"}).to_string(),
      Err(tokio::sync::broadcast::error::RecvError::Closed)=>break,
     };
     if !matches!(tokio::time::timeout(Duration::from_secs(2),socket.send(WsMessage::Text(message.into()))).await,Ok(Ok(()))){break;}
    },
    _=timer.tick()=>{
     let snapshot=a.core.snapshot.read().await.clone();
     let mut payload=serde_json::to_value(&snapshot).unwrap();
     payload["diagnostics"]=json!({"continuity":a.core.playback.diagnostics.snapshot(),"capture":a.core.playback.capture.status()});
     let send=socket.send(WsMessage::Text(payload.to_string().into()));
     if !matches!(tokio::time::timeout(Duration::from_secs(2),send).await,Ok(Ok(()))) {break;}
    },
    message=socket.recv()=>{
     match message {
      Some(Ok(WsMessage::Close(_)))=>{let _=socket.flush().await;break;},
      None|Some(Err(_))=>break,
      _=>{}
     }
    }
   }
  }
 })
}

fn port(name: &str, default: u16) -> anyhow::Result<u16> {
    Ok(std::env::var(name)
        .ok()
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(default))
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _timer = mic_protocol::TimerResolution::acquire();
    let http = port("MIC_HTTP_PORT", 8080)?;
    let config = Config {
        discovery: port("MIC_DISCOVERY_PORT", 41000)?,
        control: port("MIC_CONTROL_PORT", 41001)?,
        audio: port("MIC_AUDIO_PORT", 41002)?,
        jitter: std::env::var("MIC_JITTER_FRAMES")
            .unwrap_or("3".into())
            .parse()?,
    };
    let (core, task, stop) = Core::start(config).await?;
    let playback = core.playback.clone();
    if std::env::var("MIC_DISABLE_AUDIO").as_deref() != Ok("1") {
        match tokio::task::spawn_blocking(move || playback.select("default")).await? {
            Ok(name) => println!("Audio output: {name}"),
            Err(e) => eprintln!("Audio output unavailable: {e}"),
        };
    }
    let origins = vec![
        format!("http://127.0.0.1:{http}"),
        format!("http://localhost:{http}"),
        "http://localhost:4200".into(),
        "http://127.0.0.1:4200".into(),
    ];
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let app = App {
        core,
        token: hex::encode(rand::random::<[u8; 32]>()),
        origins: Arc::new(origins),
        shutdown: shutdown_rx,
    };
    let routes = Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/devices", get(devices))
        .route("/api/v1/devices/{id}/pair", post(pair))
        .route("/api/v1/devices/{id}/disconnect", post(disconnect))
        .route("/api/v1/devices/{id}/audio", patch(audio))
        .route("/api/v1/audio/outputs", get(outputs))
        .route("/api/v1/audio/output", put(output))
        .route("/api/v1/metrics", get(metrics))
        .route(
            "/api/v1/diagnostics",
            get(diagnostics).patch(diagnostics_options),
        )
        .route("/api/v1/diagnostics/capture/start", post(capture_start))
        .route("/api/v1/diagnostics/capture/stop", post(capture_stop))
        .route("/api/v1/telemetry/history", get(history))
        .route("/api/v1/events", get(events))
        .fallback_service(tower_http::services::ServeDir::new(
            "web/angular/dist/mic-ui/browser",
        ))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", http)).await?;
    println!("Wireless Mic PoC: http://127.0.0.1:{http}");
    axum::serve(listener, routes)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = shutdown_tx.send(true);
        })
        .await?;
    let _ = stop.send(());
    task.await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn api_validation() {
        assert!(serde_json::from_value::<Audio>(json!({"source":"invalid"})).is_err());
        assert!(serde_json::from_value::<Audio>(json!({"extra":1})).is_err());
        assert!(validate_audio(Some(1.1)).is_err());
        assert!(validate_audio(Some(f32::NAN)).is_err());
        assert!(validate_audio(Some(0.2)).is_ok());
    }
}
