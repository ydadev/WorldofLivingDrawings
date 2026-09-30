use std::{
    net::SocketAddr,
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use tokio::{sync::Semaphore, time::timeout};
use uuid::Uuid;

use crate::{
    access::{AccessError, AccessStore, GrantKind, PairCode},
    blob_store::{BlobStore, BlobStoreError},
    paint_image::{self, PaintImageError},
    upload::{self, UploadError},
};

static PAINT_CPU: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));
static PAINT_QUEUE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(20)));

const OWNER_COOKIE: &str = "__Host-ldw-owner";
const CONTROLLER_COOKIE: &str = "__Host-ldw-controller";
const VIEWER_COOKIE: &str = "__Host-ldw-viewer";
const VIEWER_CLAIM_COOKIE: &str = "__Host-ldw-viewer-claim";

#[derive(Clone)]
pub struct AppState {
    pub access: AccessStore,
    pub blob_store: BlobStore,
    pub public_origin: Arc<str>,
    pub simulation_hub: crate::simulation::SimulationHub,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/ready", get(ready))
        .route("/api/login", post(login))
        .route("/api/owners", post(create_owner))
        .route("/api/sessions", post(create_session))
        .route(
            "/api/sessions/{id}/upload-intents",
            post(create_upload_intent),
        )
        .route(
            "/api/sessions/{id}/upload-intents/{intent_id}/paint",
            put(upload_paint).layer(DefaultBodyLimit::max(paint_image::MAX_UPLOAD_BYTES)),
        )
        .route(
            "/api/sessions/{id}/upload-intents/{intent_id}/finalize",
            post(finalize_upload),
        )
        .route("/api/sessions/{id}/paint/{blob_id}", get(private_paint))
        .route("/api/sessions/{id}/scene", get(scene))
        .route("/api/sessions/{id}/ws", get(crate::realtime::websocket))
        .route("/api/sessions/{id}/viewers", post(create_viewer))
        .route(
            "/api/sessions/{id}/viewer-claims",
            post(request_viewer_claim),
        )
        .route(
            "/api/sessions/{id}/viewer-claims/approve",
            post(approve_viewer_claim),
        )
        .route(
            "/api/sessions/{id}/viewer-claims/{claim_id}/activate",
            post(activate_viewer_claim),
        )
        .route(
            "/api/sessions/{id}/invitation",
            post(open_invitation).delete(close_invitation),
        )
        .route("/api/sessions/{id}/pair", post(pair))
        .layer(middleware::map_response(no_store))
        .with_state(state)
}

async fn ready(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    state.access.ping().await?;
    Ok(StatusCode::OK)
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[derive(Debug)]
struct ApiError(StatusCode, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<AccessError> for ApiError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Forbidden | AccessError::InvalidCredentials => {
                Self(StatusCode::FORBIDDEN, "ACCESS_DENIED")
            }
            AccessError::AlreadyInitialized => Self(StatusCode::CONFLICT, "ALREADY_INITIALIZED"),
            AccessError::InvalidAccount => Self(StatusCode::BAD_REQUEST, "INVALID_ACCOUNT"),
            AccessError::RateLimited => Self(StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED"),
            AccessError::ControllerLimit => Self(StatusCode::CONFLICT, "CONTROLLER_LIMIT"),
            AccessError::ViewerLimit => Self(StatusCode::CONFLICT, "VIEWER_LIMIT"),
            AccessError::ViewerClaimLimit => {
                Self(StatusCode::TOO_MANY_REQUESTS, "VIEWER_CLAIM_LIMIT")
            }
            AccessError::ViewerPending => Self(StatusCode::ACCEPTED, "VIEWER_PENDING"),
            AccessError::OwnerApprovalRequired => {
                Self(StatusCode::FORBIDDEN, "OWNER_APPROVAL_REQUIRED")
            }
            AccessError::Crypto | AccessError::SceneState | AccessError::Database(_) => {
                Self(StatusCode::INTERNAL_SERVER_ERROR, "SERVER_ERROR")
            }
        }
    }
}

impl From<UploadError> for ApiError {
    fn from(error: UploadError) -> Self {
        match error {
            UploadError::Forbidden => Self(StatusCode::FORBIDDEN, "ACCESS_DENIED"),
            UploadError::InvalidPaint => Self(StatusCode::BAD_REQUEST, "INVALID_PAINT_RESULT"),
            UploadError::StaleScene => Self(StatusCode::CONFLICT, "STALE_SCENE"),
            UploadError::SceneFull => Self(StatusCode::CONFLICT, "SCENE_FULL"),
            UploadError::IntentLimit => Self(StatusCode::TOO_MANY_REQUESTS, "UPLOAD_INTENT_LIMIT"),
            UploadError::StorageFull => {
                Self(StatusCode::INSUFFICIENT_STORAGE, "PAINT_STORAGE_FULL")
            }
            UploadError::Expired => Self(StatusCode::CONFLICT, "UPLOAD_INTENT_EXPIRED"),
            UploadError::InvalidExpiry => Self(StatusCode::CONFLICT, "UPLOAD_COMMAND_EXPIRED"),
            UploadError::Conflict => Self(StatusCode::CONFLICT, "UPLOAD_CONFLICT"),
            UploadError::Simulation(crate::simulation::SimulationError::InvalidPublication) => {
                Self(StatusCode::CONFLICT, "SCENE_FULL")
            }
            UploadError::Simulation(crate::simulation::SimulationError::SessionLimit) => {
                Self(StatusCode::CONFLICT, "SIMULATED_SESSION_LIMIT")
            }
            UploadError::InvalidScene
            | UploadError::Database(_)
            | UploadError::Storage(_)
            | UploadError::Simulation(_) => Self(StatusCode::INTERNAL_SERVER_ERROR, "SERVER_ERROR"),
        }
    }
}

impl From<PaintImageError> for ApiError {
    fn from(error: PaintImageError) -> Self {
        match error {
            PaintImageError::InvalidImage => Self(StatusCode::BAD_REQUEST, "INVALID_PAINT_IMAGE"),
            PaintImageError::TooLarge => Self(StatusCode::PAYLOAD_TOO_LARGE, "PAINT_TOO_LARGE"),
        }
    }
}

impl From<BlobStoreError> for ApiError {
    fn from(error: BlobStoreError) -> Self {
        match error {
            BlobStoreError::InvalidPath => Self(StatusCode::NOT_FOUND, "PAINT_NOT_FOUND"),
            BlobStoreError::InvalidPaint | BlobStoreError::CorruptBlob | BlobStoreError::Io(_) => {
                Self(StatusCode::INTERNAL_SERVER_ERROR, "PAINT_UNAVAILABLE")
            }
        }
    }
}

fn require_origin(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    if headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        != Some(state.public_origin.as_ref())
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "ORIGIN_DENIED"));
    }
    Ok(())
}

fn csrf<'a>(headers: &'a HeaderMap) -> Result<&'a str, ApiError> {
    headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError(StatusCode::FORBIDDEN, "CSRF_REQUIRED"))
}

fn cookie_token<'a>(jar: &'a CookieJar, name: &str) -> Result<&'a str, ApiError> {
    jar.get(name)
        .map(Cookie::value)
        .ok_or(ApiError(StatusCode::FORBIDDEN, "ACCESS_DENIED"))
}

fn auth_cookie(name: &'static str, token: String) -> Cookie<'static> {
    Cookie::build((name, token))
        .path("/")
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Strict)
        .build()
}

#[derive(Deserialize)]
struct LoginRequest {
    login: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    role: String,
    csrf: String,
}

async fn login(
    State(state): State<AppState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<LoginRequest>,
) -> Result<(CookieJar, Json<LoginResponse>), ApiError> {
    require_origin(&headers, &state)?;
    let peer_ip = peer
        .map(|Extension(ConnectInfo(address))| address.ip().to_string())
        .unwrap_or_else(|| "unknown-peer".to_owned());
    let grant = state
        .access
        .login(&input.login, &input.password, &peer_ip)
        .await?;
    let response = LoginResponse {
        role: grant.role,
        csrf: grant.csrf,
    };
    Ok((
        jar.add(auth_cookie(OWNER_COOKIE, grant.token)),
        Json(response),
    ))
}

#[derive(Deserialize)]
struct OwnerRequest {
    login: String,
    password: String,
}

#[derive(Serialize)]
struct IdResponse {
    id: Uuid,
}

async fn create_owner(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<OwnerRequest>,
) -> Result<Json<IdResponse>, ApiError> {
    require_origin(&headers, &state)?;
    let token = cookie_token(&jar, OWNER_COOKIE)?;
    state
        .access
        .check_owner_csrf(token, csrf(&headers)?)
        .await?;
    let id = state
        .access
        .create_owner(token, &input.login, &input.password)
        .await?;
    Ok(Json(IdResponse { id }))
}

#[derive(Serialize)]
struct SessionResponse {
    session_id: Uuid,
    scene_id: Uuid,
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<Json<SessionResponse>, ApiError> {
    require_origin(&headers, &state)?;
    let ids = state
        .access
        .create_session(cookie_token(&jar, OWNER_COOKIE)?, csrf(&headers)?)
        .await?;
    Ok(Json(SessionResponse {
        session_id: ids.session_id,
        scene_id: ids.scene_id,
    }))
}

async fn create_upload_intent(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<upload::UploadIntentRequest>,
) -> Result<(StatusCode, Json<upload::UploadIntentResponse>), ApiError> {
    require_origin(&headers, &state)?;
    let (kind, token) = if let Some(owner) = jar.get(OWNER_COOKIE) {
        (GrantKind::Owner, owner.value())
    } else {
        (
            GrantKind::Controller,
            cookie_token(&jar, CONTROLLER_COOKIE)?,
        )
    };
    state
        .access
        .check_socket_csrf(kind, token, csrf(&headers)?, session_id)
        .await?;
    let access = state.access.scene_access(kind, token, session_id).await?;
    let intent = upload::create_upload_intent(state.access.pool(), kind, &access, &input).await?;
    Ok((StatusCode::CREATED, Json(intent)))
}

async fn upload_paint(
    State(state): State<AppState>,
    Path((session_id, intent_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    jar: CookieJar,
    image: Bytes,
) -> Result<Json<upload::UploadedPaintResponse>, ApiError> {
    require_origin(&headers, &state)?;
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        != Some("image/png")
    {
        return Err(ApiError(StatusCode::UNSUPPORTED_MEDIA_TYPE, "PNG_REQUIRED"));
    }
    let (kind, token) = if let Some(owner) = jar.get(OWNER_COOKIE) {
        (GrantKind::Owner, owner.value())
    } else {
        (
            GrantKind::Controller,
            cookie_token(&jar, CONTROLLER_COOKIE)?,
        )
    };
    state
        .access
        .check_socket_csrf(kind, token, csrf(&headers)?, session_id)
        .await?;
    let access = state.access.scene_access(kind, token, session_id).await?;
    let _queued = PAINT_QUEUE
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "PAINT_QUEUE_FULL"))?;
    let worker = timeout(Duration::from_secs(10), PAINT_CPU.clone().acquire_owned())
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "PAINT_QUEUE_TIMEOUT"))?
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "SERVER_ERROR"))?;
    let normalized = timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let _worker = worker;
            paint_image::normalize_png(&image)
        }),
    )
    .await
    .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "PAINT_TIMEOUT"))?
    .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "SERVER_ERROR"))??;
    let stored =
        upload::store_paint(state.access.pool(), kind, &access, intent_id, normalized).await?;
    Ok(Json(stored))
}

async fn finalize_upload(
    State(state): State<AppState>,
    Path((session_id, intent_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<upload::FinalizeRequest>,
) -> Result<Json<upload::FinalizedPaintResponse>, ApiError> {
    require_origin(&headers, &state)?;
    let (kind, token) = if let Some(owner) = jar.get(OWNER_COOKIE) {
        (GrantKind::Owner, owner.value())
    } else {
        (
            GrantKind::Controller,
            cookie_token(&jar, CONTROLLER_COOKIE)?,
        )
    };
    state
        .access
        .check_socket_csrf(kind, token, csrf(&headers)?, session_id)
        .await?;
    let access = state.access.scene_access(kind, token, session_id).await?;
    let result = upload::finalize_upload(
        state.access.pool(),
        &state.blob_store,
        kind,
        &access,
        intent_id,
        input.expires_at,
    )
    .await?;
    Ok(Json(result))
}

async fn private_paint(
    State(state): State<AppState>,
    Path((session_id, blob_id)): Path<(Uuid, String)>,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    let (kind, token) = if let Some(owner) = jar.get(OWNER_COOKIE) {
        (GrantKind::Owner, owner.value())
    } else if let Some(controller) = jar.get(CONTROLLER_COOKIE) {
        (GrantKind::Controller, controller.value())
    } else {
        (GrantKind::Viewer, cookie_token(&jar, VIEWER_COOKIE)?)
    };
    let access = state.access.scene_access(kind, token, session_id).await?;
    let authorized: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM scenes c \
         CROSS JOIN LATERAL jsonb_array_elements(COALESCE(c.state->'entities', '[]'::jsonb)) AS entity(value) \
         WHERE c.id = $1 AND c.session_id = $2 AND entity.value->>'paintBlobId' = $3)",
    )
    .bind(access.scene.scene_id)
    .bind(session_id)
    .bind(&blob_id)
    .fetch_one(state.access.pool())
    .await
    .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "SERVER_ERROR"))?;
    if !authorized {
        return Err(ApiError(StatusCode::NOT_FOUND, "PAINT_NOT_FOUND"));
    }
    let store = state.blob_store.clone();
    let bytes = tokio::task::spawn_blocking(move || store.read(&blob_id))
        .await
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "PAINT_UNAVAILABLE"))??;
    Ok((
        [(header::CONTENT_TYPE, HeaderValue::from_static("image/png"))],
        Bytes::from(bytes),
    ))
}

#[derive(Serialize)]
struct SceneResponse {
    session_id: Uuid,
    scene_id: Uuid,
    world_id: String,
    world_version: i32,
    scene_epoch: i64,
    revision: i64,
    server_time_ms: u64,
}

async fn scene(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    jar: CookieJar,
) -> Result<Json<SceneResponse>, ApiError> {
    let summary = if let Some(owner) = jar.get(OWNER_COOKIE) {
        state.access.owner_scene(owner.value(), session_id).await?
    } else if let Some(controller) = jar.get(CONTROLLER_COOKIE) {
        state
            .access
            .controller_scene(controller.value(), session_id)
            .await?
    } else {
        state
            .access
            .viewer_scene(cookie_token(&jar, VIEWER_COOKIE)?, session_id)
            .await?
            .0
    };
    Ok(Json(SceneResponse {
        session_id: summary.session_id,
        scene_id: summary.scene_id,
        world_id: summary.world_id,
        world_version: summary.world_version,
        scene_epoch: summary.scene_epoch,
        revision: summary.revision,
        server_time_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    }))
}

#[derive(Deserialize)]
struct ViewerRequest {
    interact: bool,
}

#[derive(Serialize)]
struct ViewerResponse {
    role: String,
    csrf: String,
}

async fn create_viewer(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<ViewerRequest>,
) -> Result<(CookieJar, Json<ViewerResponse>), ApiError> {
    require_origin(&headers, &state)?;
    let grant = state
        .access
        .create_viewer(
            cookie_token(&jar, OWNER_COOKIE)?,
            csrf(&headers)?,
            session_id,
            input.interact,
        )
        .await?;
    let response = ViewerResponse {
        role: grant.role,
        csrf: grant.csrf,
    };
    Ok((
        jar.add(auth_cookie(VIEWER_COOKIE, grant.token)),
        Json(response),
    ))
}

#[derive(Serialize)]
struct ViewerClaimResponse {
    claim_id: Uuid,
    code: String,
    csrf: String,
    expires_in_seconds: u32,
}

async fn request_viewer_claim(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<(CookieJar, Json<ViewerClaimResponse>), ApiError> {
    require_origin(&headers, &state)?;
    let claim = state.access.request_viewer_claim(session_id).await?;
    let cookie = Cookie::build((VIEWER_CLAIM_COOKIE, claim.claim_token))
        .path("/")
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Strict)
        .max_age(time::Duration::minutes(5))
        .build();
    Ok((
        jar.add(cookie),
        Json(ViewerClaimResponse {
            claim_id: claim.id,
            code: claim.code,
            csrf: claim.csrf,
            expires_in_seconds: 300,
        }),
    ))
}

#[derive(Deserialize)]
struct ApproveViewerRequest {
    code: String,
    interact: bool,
}

async fn approve_viewer_claim(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<ApproveViewerRequest>,
) -> Result<StatusCode, ApiError> {
    require_origin(&headers, &state)?;
    state
        .access
        .approve_viewer_claim(
            cookie_token(&jar, OWNER_COOKIE)?,
            csrf(&headers)?,
            session_id,
            &input.code,
            input.interact,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn activate_viewer_claim(
    State(state): State<AppState>,
    Path((session_id, claim_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    require_origin(&headers, &state)?;
    let token = cookie_token(&jar, VIEWER_CLAIM_COOKIE)?;
    let grant = state
        .access
        .activate_viewer_claim(session_id, claim_id, token, csrf(&headers)?)
        .await?;
    Ok((
        jar.add(auth_cookie(VIEWER_COOKIE, grant.token)),
        Json(ViewerResponse {
            role: grant.role,
            csrf: grant.csrf,
        }),
    )
        .into_response())
}

#[derive(Serialize)]
struct InvitationResponse {
    pin: String,
    qr_secret: String,
    expires_in_seconds: u32,
}

async fn open_invitation(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<Json<InvitationResponse>, ApiError> {
    require_origin(&headers, &state)?;
    let invitation = state
        .access
        .open_invitation(
            cookie_token(&jar, OWNER_COOKIE)?,
            csrf(&headers)?,
            session_id,
        )
        .await?;
    Ok(Json(InvitationResponse {
        pin: invitation.pin,
        qr_secret: invitation.qr_secret,
        expires_in_seconds: 300,
    }))
}

async fn close_invitation(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<StatusCode, ApiError> {
    require_origin(&headers, &state)?;
    state
        .access
        .close_invitation(
            cookie_token(&jar, OWNER_COOKIE)?,
            csrf(&headers)?,
            session_id,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PairRequest {
    client_key: String,
    pin: Option<String>,
    qr_secret: Option<String>,
}

#[derive(Serialize)]
struct PairResponse {
    participant_id: Uuid,
    csrf: String,
}

async fn pair(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(input): Json<PairRequest>,
) -> Result<(CookieJar, Json<PairResponse>), ApiError> {
    require_origin(&headers, &state)?;
    let code = match (input.pin.as_deref(), input.qr_secret.as_deref()) {
        (Some(pin), None) => PairCode::Pin(pin),
        (None, Some(secret)) => PairCode::Qr(secret),
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "ONE_PAIR_CODE_REQUIRED")),
    };
    let grant = state
        .access
        .pair_controller(session_id, &input.client_key, &peer.ip().to_string(), code)
        .await?;
    let response = PairResponse {
        participant_id: grant.participant_id,
        csrf: grant.csrf,
    };
    Ok((
        jar.add(auth_cookie(CONTROLLER_COOKIE, grant.token)),
        Json(response),
    ))
}
