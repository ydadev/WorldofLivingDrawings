use std::{
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use axum_extra::extract::cookie::CookieJar;
use ldw_sim::{Point as SimPoint, World, WorldCheckpoint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::time::{interval_at, timeout};
use uuid::Uuid;

use crate::{
    access::{AccessError, AccessStore, GrantKind, SceneAccess},
    http::AppState,
    simulation::{self, SimulationHub},
};

const OWNER_COOKIE: &str = "__Host-ldw-owner";
const CONTROLLER_COOKIE: &str = "__Host-ldw-controller";
const VIEWER_COOKIE: &str = "__Host-ldw-viewer";
const MAX_MESSAGE_BYTES: usize = 8192;

#[derive(Deserialize)]
struct WorldRules {
    id: String,
    version: i32,
    zones: Vec<ZoneRules>,
}
#[derive(Deserialize)]
struct ZoneRules {
    id: String,
    bounds: [f64; 4],
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InteractionRules {
    id: String,
    allowed_zone_id: String,
}
static UNDERWATER_RULES: LazyLock<(WorldRules, Vec<InteractionRules>)> = LazyLock::new(|| {
    (
        serde_json::from_str(include_str!("../../../content/underwater/world.json"))
            .expect("versioned underwater world definition"),
        serde_json::from_str(include_str!(
            "../../../content/underwater/interactions.json"
        ))
        .expect("versioned underwater interaction definitions"),
    )
});

fn validate_interaction(
    access: &SceneAccess,
    command: &InteractionCommand,
) -> Option<&'static str> {
    let (world, interactions) = &*UNDERWATER_RULES;
    if access.scene.world_id != world.id || access.scene.world_version != world.version {
        return Some("UNSUPPORTED_WORLD");
    }
    let Some(definition) = interactions
        .iter()
        .find(|item| item.id == command.interaction_id)
    else {
        return Some("UNKNOWN_INTERACTION");
    };
    let Some(zone) = world
        .zones
        .iter()
        .find(|item| item.id == definition.allowed_zone_id)
    else {
        return Some("INVALID_WORLD_PACKAGE");
    };
    if !command.point.x.is_finite()
        || !command.point.y.is_finite()
        || !(zone.bounds[0]..=zone.bounds[1]).contains(&command.point.x)
        || !(zone.bounds[2]..=zone.bounds[3]).contains(&command.point.y)
    {
        return Some("OUTSIDE_WATER");
    }
    None
}

fn feed_limit_code(
    state: &Value,
    world: &World,
    grant_id: Uuid,
    now_ms: i64,
) -> Result<Option<&'static str>, AccessError> {
    let pending = match state.get("pendingInteractions") {
        None => 0,
        Some(Value::Array(entries)) => entries
            .iter()
            .filter(|entry| entry.get("interactionId").and_then(Value::as_str) == Some("feed"))
            .count(),
        _ => return Err(AccessError::SceneState),
    };
    if world.feed_sources().len() + pending >= 3 {
        return Ok(Some("FEED_LIMIT"));
    }
    let limits = state.get("interactionLimits");
    if let Some(limits) = limits {
        if !limits.is_object() {
            return Err(AccessError::SceneState);
        }
        if limits
            .get("lastSceneFeedMs")
            .and_then(Value::as_i64)
            .is_some_and(|last| now_ms.saturating_sub(last) < 500)
        {
            return Ok(Some("FEED_SCENE_COOLDOWN"));
        }
        let grant_key = grant_id.to_string();
        if limits
            .get("feedByGrantMs")
            .and_then(|map| map.get(grant_key.as_str()))
            .and_then(Value::as_i64)
            .is_some_and(|last| now_ms.saturating_sub(last) < 1000)
        {
            return Ok(Some("FEED_COOLDOWN"));
        }
    }
    Ok(None)
}

fn record_feed_acceptance(
    state: &mut Value,
    grant_id: Uuid,
    now_ms: i64,
) -> Result<(), AccessError> {
    let object = state.as_object_mut().ok_or(AccessError::SceneState)?;
    let limits = object
        .entry("interactionLimits")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(AccessError::SceneState)?;
    let by_grant = limits
        .entry("feedByGrantMs")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(AccessError::SceneState)?;
    by_grant.retain(|_, value| {
        value
            .as_i64()
            .is_some_and(|last| now_ms.saturating_sub(last) < 1000)
    });
    by_grant.insert(grant_id.to_string(), json!(now_ms));
    limits.insert("lastSceneFeedMs".into(), json!(now_ms));
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InteractionCommand {
    #[serde(rename = "type")]
    pub kind: String,
    pub command_id: Uuid,
    pub session_id: Uuid,
    pub scene_id: Uuid,
    pub scene_epoch: i64,
    pub interaction_id: String,
    pub point: Point,
    /// Unix milliseconds; a new interaction expires within ten seconds.
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub async fn websocket(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
    ws: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    if headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        != Some(state.public_origin.as_ref())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let (kind, token) = if let Some(cookie) = jar.get(OWNER_COOKIE) {
        (GrantKind::Owner, cookie.value().to_owned())
    } else if let Some(cookie) = jar.get(CONTROLLER_COOKIE) {
        (GrantKind::Controller, cookie.value().to_owned())
    } else if let Some(cookie) = jar.get(VIEWER_COOKIE) {
        (GrantKind::Viewer, cookie.value().to_owned())
    } else {
        return Err(StatusCode::FORBIDDEN);
    };
    if kind == GrantKind::Controller {
        state
            .access
            .resume_controller(&token, session_id)
            .await
            .map_err(access_status)?;
    }
    state
        .access
        .scene_access(kind, &token, session_id)
        .await
        .map_err(access_status)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            serve(
                socket,
                state.access,
                state.simulation_hub,
                kind,
                token,
                session_id,
            )
        }))
}

fn access_status(error: AccessError) -> StatusCode {
    match error {
        AccessError::Database(_) | AccessError::Crypto => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::FORBIDDEN,
    }
}

async fn serve(
    mut socket: WebSocket,
    store: AccessStore,
    hub: SimulationHub,
    kind: GrantKind,
    token: String,
    session_id: Uuid,
) {
    let first = timeout(Duration::from_secs(5), socket.recv()).await;
    let csrf = match first {
        Ok(Some(Ok(Message::Text(text)))) if text.len() <= MAX_MESSAGE_BYTES => {
            serde_json::from_str::<Value>(&text).ok().and_then(|value| {
                if value.get("type") == Some(&Value::String("hello".into())) {
                    value.get("csrf").and_then(Value::as_str).map(str::to_owned)
                } else {
                    None
                }
            })
        }
        _ => None,
    };
    let Some(csrf) = csrf else { return };
    if store
        .check_socket_csrf(kind, &token, &csrf, session_id)
        .await
        .is_err()
    {
        return;
    }
    let Ok(mut access) = store.scene_access(kind, &token, session_id).await else {
        return;
    };
    let mut positions = hub.subscribe();
    let Ok((snapshot, mut cursor)) = load_snapshot(store.pool(), &access).await else {
        return;
    };
    if send_json(&mut socket, &snapshot).await.is_err() {
        return;
    }

    let mut poll = interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    let mut heartbeat = interval_at(
        tokio::time::Instant::now() + Duration::from_secs(5),
        Duration::from_secs(5),
    );
    let mut last_pong = Instant::now();
    loop {
        tokio::select! {
            frame = positions.recv() => {
                match frame {
                    Ok(frame) if frame.scene_id == access.scene.scene_id
                        && frame.scene_epoch == access.scene.scene_epoch
                        && frame.revision <= cursor => {
                        let Ok(value) = serde_json::to_value(frame) else { break };
                        if send_json(&mut socket, &value).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
            received = socket.recv() => {
                let Some(Ok(message)) = received else { break };
                match message {
                    Message::Pong(_) => last_pong = Instant::now(),
                    Message::Text(text) => {
                        if text.len() > MAX_MESSAGE_BYTES { break; }
                        let response = handle_message(&store, kind, &token, session_id, &text).await;
                        if send_json(&mut socket, &response).await.is_err() { break; }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(10)
                    || store.check_socket_csrf(kind, &token, &csrf, session_id).await.is_err()
                { break; }
                if kind == GrantKind::Controller
                    && store.heartbeat_controller(&token, session_id).await.is_err()
                { break; }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
            _ = poll.tick() => {
                let Ok(current) = store.scene_access(kind, &token, session_id).await else { break };
                if current.scene.scene_id != access.scene.scene_id
                    || current.scene.scene_epoch != access.scene.scene_epoch {
                    let Ok((snapshot, new_cursor)) = load_snapshot(store.pool(), &current).await else { break };
                    if send_json(&mut socket, &snapshot).await.is_err() { break; }
                    access = current;
                    cursor = new_cursor;
                    continue;
                }
                let Ok(events) = load_events(store.pool(), access.scene.scene_id, cursor).await else { break };
                let mut gap = false;
                for (revision, epoch, event) in events {
                    if revision != cursor + 1 || epoch != access.scene.scene_epoch {
                        gap = true;
                        break;
                    }
                    let upsert = if event.get("type").and_then(Value::as_str) == Some("entity_published") {
                        event.get("entity").cloned().map(|entity| json!([entity])).unwrap_or(json!([]))
                    } else {
                        json!([])
                    };
                    let delta = json!({"type":"delta", "sceneId":access.scene.scene_id,
                        "schemaVersion":1, "sceneEpoch":epoch, "revision":revision,
                        "simulationTick":event.get("simulationTick").and_then(Value::as_u64).unwrap_or(0),
                        "upsert":upsert, "remove":[], "event":event});
                    if send_json(&mut socket, &delta).await.is_err() { return; }
                    cursor = revision;
                }
                if gap {
                    let Ok((snapshot, new_cursor)) = load_snapshot(store.pool(), &current).await else { break };
                    if send_json(&mut socket, &snapshot).await.is_err() { break; }
                    cursor = new_cursor;
                }
                access = current;
            }
        }
    }
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> Result<(), axum::Error> {
    socket.send(Message::text(value.to_string())).await
}

async fn handle_message(
    store: &AccessStore,
    kind: GrantKind,
    token: &str,
    session_id: Uuid,
    text: &str,
) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return json!({"type":"error","code":"INVALID_MESSAGE"});
    };
    match value.get("type").and_then(Value::as_str) {
        Some("command") => {
            let Ok(command) = serde_json::from_value::<InteractionCommand>(value) else {
                return json!({"type":"error","code":"INVALID_COMMAND"});
            };
            match process_command(store, kind, token, session_id, &command).await {
                Ok(outcome) => outcome,
                Err(AccessError::Forbidden) => json!({"type":"error","code":"ACCESS_DENIED"}),
                Err(_) => json!({"type":"error","code":"SERVER_ERROR"}),
            }
        }
        Some("status") => {
            let Some(command_id) = value
                .get("commandId")
                .and_then(Value::as_str)
                .and_then(|text| Uuid::parse_str(text).ok())
            else {
                return json!({"type":"error","code":"INVALID_COMMAND_ID"});
            };
            match command_status(store, kind, token, session_id, command_id).await {
                Ok(Some(outcome)) => outcome,
                Ok(None) => json!({"type":"status","commandId":command_id,"known":false}),
                Err(AccessError::Forbidden) => json!({"type":"error","code":"ACCESS_DENIED"}),
                Err(_) => json!({"type":"error","code":"SERVER_ERROR"}),
            }
        }
        _ => json!({"type":"error","code":"UNKNOWN_MESSAGE"}),
    }
}

async fn load_snapshot(pool: &PgPool, access: &SceneAccess) -> Result<(Value, i64), sqlx::Error> {
    let (world_id, world_version, epoch, revision, tick, state): (
        String,
        i32,
        i64,
        i64,
        i64,
        Value,
    ) = sqlx::query_as(
        "SELECT world_id, world_version, scene_epoch, revision, simulation_tick, state \
             FROM scenes WHERE id = $1 AND session_id = $2",
    )
    .bind(access.scene.scene_id)
    .bind(access.scene.session_id)
    .fetch_one(pool)
    .await?;
    Ok((
        json!({
            "type":"snapshot", "schemaVersion":1, "sceneId":access.scene.scene_id,
            "sceneEpoch":epoch, "revision":revision, "simulationTick":tick,
            "serverTime":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64,
            "worldId":world_id, "worldVersion":world_version,
            "simulationVersion":1,
            "entities":state.get("entities").cloned().unwrap_or(json!([])),
            "activeActions":state.get("activeActions").cloned().unwrap_or(json!([])),
            "pendingInteractions":state.get("pendingInteractions").cloned().unwrap_or(json!([])),
            "resources":state.get("resources").cloned().unwrap_or(json!({})),
            "reservations":state.get("reservations").cloned().unwrap_or(json!([]))
        }),
        revision,
    ))
}

async fn load_events(
    pool: &PgPool,
    scene_id: Uuid,
    after: i64,
) -> Result<Vec<(i64, i64, Value)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT revision, scene_epoch, event FROM scene_events \
         WHERE scene_id = $1 AND revision > $2 ORDER BY revision LIMIT 100",
    )
    .bind(scene_id)
    .bind(after)
    .fetch_all(pool)
    .await
}

pub async fn command_status(
    store: &AccessStore,
    kind: GrantKind,
    token: &str,
    session_id: Uuid,
    command_id: Uuid,
) -> Result<Option<Value>, AccessError> {
    let access = store.scene_access(kind, token, session_id).await?;
    let outcome: Option<Value> = sqlx::query_scalar(
        "SELECT outcome FROM scene_commands WHERE session_id = $1 AND grant_id = $2 AND command_id = $3"
    ).bind(session_id).bind(access.grant_id).bind(command_id)
        .fetch_optional(store.pool()).await?;
    Ok(outcome)
}

pub async fn process_command(
    store: &AccessStore,
    kind: GrantKind,
    token: &str,
    session_id: Uuid,
    command: &InteractionCommand,
) -> Result<Value, AccessError> {
    let access = store.scene_access(kind, token, session_id).await?;
    let body = serde_json::to_vec(command).map_err(|_| AccessError::Crypto)?;
    let body_hash: [u8; 32] = Sha256::digest(&body).into();
    let mut tx = store.pool().begin().await?;
    let (epoch, revision, persisted_tick, mut state, status): (i64, i64, i64, Value, String) =
        sqlx::query_as(
            "SELECT c.scene_epoch, c.revision, c.simulation_tick, c.state, s.status FROM scenes c \
         JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 AND c.session_id = $2 FOR UPDATE OF c",
        )
        .bind(access.scene.scene_id)
        .bind(session_id)
        .fetch_one(&mut *tx)
        .await?;
    let previous: Option<(Vec<u8>, Value)> = sqlx::query_as(
        "SELECT body_hash, outcome FROM scene_commands \
         WHERE session_id = $1 AND grant_id = $2 AND command_id = $3",
    )
    .bind(session_id)
    .bind(access.grant_id)
    .bind(command.command_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((old_hash, outcome)) = previous {
        return Ok(if old_hash == body_hash {
            outcome
        } else {
            json!({"type":"error","code":"COMMAND_CONFLICT","commandId":command.command_id})
        });
    }
    if kind == GrantKind::Controller {
        sqlx::query("UPDATE device_grants SET last_activity_at = now() WHERE id = $1")
            .bind(access.grant_id)
            .execute(&mut *tx)
            .await?;
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AccessError::Crypto)?
        .as_millis() as i64;
    let mut initial_checkpoint = None;
    let mut code = if command.kind != "command" || command.session_id != session_id {
        Some("INVALID_COMMAND")
    } else if command.scene_id != access.scene.scene_id || command.scene_epoch != epoch {
        Some("STALE_SCENE")
    } else if command.expires_at < now_ms || command.expires_at > now_ms + 10_000 {
        Some("EXPIRED_COMMAND")
    } else if !access.may_interact() {
        Some("READ_ONLY")
    } else if status != "running" {
        Some("SCENE_NOT_RUNNING")
    } else if let Some(reason) = validate_interaction(&access, command) {
        Some(reason)
    } else {
        None
    };
    if code.is_none() && command.interaction_id == "feed" {
        let mut world = if let Some(value) = state.get("simulation") {
            let checkpoint: WorldCheckpoint =
                serde_json::from_value(value.clone()).map_err(|_| AccessError::SceneState)?;
            if checkpoint.tick != persisted_tick as u64 {
                return Err(AccessError::SceneState);
            }
            World::restore(checkpoint).map_err(|_| AccessError::SceneState)?
        } else {
            if persisted_tick != 0
                || state
                    .get("entities")
                    .and_then(Value::as_array)
                    .is_some_and(|entities| !entities.is_empty())
            {
                return Err(AccessError::SceneState);
            }
            simulation::initial_world(access.scene.scene_id).map_err(|_| AccessError::SceneState)?
        };
        code = feed_limit_code(&state, &world, access.grant_id, now_ms)?;
        if code.is_none() {
            let point = SimPoint {
                x: command.point.x as f32,
                y: command.point.y as f32,
            };
            code = match world.start_feed(&command.command_id.simple().to_string(), point) {
                Ok(()) => None,
                Err(ldw_sim::SimError::InvalidPosition) => Some("OUTSIDE_WATER"),
                Err(ldw_sim::SimError::FeedLimit) => Some("FEED_LIMIT"),
                Err(_) => return Err(AccessError::SceneState),
            };
            if code.is_none() && state.get("simulation").is_none() {
                initial_checkpoint = Some(
                    simulation::initial_world(access.scene.scene_id)
                        .map_err(|_| AccessError::SceneState)?
                        .checkpoint(),
                );
            }
        }
    }
    let new_revision = if code.is_none() {
        revision.checked_add(1).ok_or(AccessError::SceneState)?
    } else {
        revision
    };
    let outcome = json!({"type":"ack","commandId":command.command_id,
        "accepted":code.is_none(),"code":code.unwrap_or("ACCEPTED"),
        "sceneId":access.scene.scene_id,"sceneEpoch":epoch,"revision":new_revision});
    if code.is_none() {
        let event = json!({"type":"interaction_requested","commandId":command.command_id,
            "interactionId":command.interaction_id,"point":command.point});
        let object = state.as_object_mut().ok_or(AccessError::SceneState)?;
        object
            .entry("pendingInteractions")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or(AccessError::SceneState)?
            .push(event.clone());
        if let Some(checkpoint) = initial_checkpoint {
            object.insert(
                "simulation".into(),
                serde_json::to_value(checkpoint).map_err(|_| AccessError::SceneState)?,
            );
            object.entry("entities").or_insert_with(|| json!([]));
        }
        if command.interaction_id == "feed" {
            record_feed_acceptance(&mut state, access.grant_id, now_ms)?;
        }
        sqlx::query(
            "UPDATE scenes SET revision = $1, updated_at = now(), state = $3::jsonb WHERE id = $2",
        )
        .bind(new_revision)
        .bind(access.scene.scene_id)
        .bind(state)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)")
            .bind(access.scene.scene_id).bind(new_revision).bind(epoch).bind(event)
            .execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO scene_commands (session_id, grant_id, command_id, body_hash, outcome) VALUES ($1, $2, $3, $4, $5)")
        .bind(session_id).bind(access.grant_id).bind(command.command_id)
        .bind(body_hash.to_vec()).bind(&outcome)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(outcome)
}
