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
use ldw_sim::{InteractionEffect, Point as SimPoint, TICKS_PER_SECOND, World, WorldCheckpoint};
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
    scene_world: &World,
) -> Option<&'static str> {
    let (world, interactions) = &*UNDERWATER_RULES;
    if access.scene.world_id != world.id || access.scene.world_version != world.version {
        return Some("UNSUPPORTED_WORLD");
    }
    if matches!(
        command.interaction_id.as_str(),
        "cancel_feed" | "cancel_boat"
    ) {
        if !command.point.x.is_finite() || !command.point.y.is_finite() {
            return Some("INVALID_COMMAND");
        }
        return if cancellation_target(command).is_some() {
            None
        } else {
            Some("INVALID_ACTION_TARGET")
        };
    }
    if command.target_action_id.is_some() {
        return Some("INVALID_ACTION_TARGET");
    }
    if scene_world.action_rule(&command.interaction_id).is_none() {
        return Some("UNKNOWN_INTERACTION");
    }
    let allowed_zone_id = scene_world
        .action_definitions()
        .iter()
        .find(|item| item.id == command.interaction_id)
        .map(|item| item.allowed_zone_id.as_str())
        .or_else(|| {
            interactions
                .iter()
                .find(|item| item.id == command.interaction_id)
                .map(|item| item.allowed_zone_id.as_str())
        });
    let Some(allowed_zone_id) = allowed_zone_id else {
        return Some("INVALID_WORLD_PACKAGE");
    };
    let Some(zone) = world.zones.iter().find(|item| item.id == allowed_zone_id) else {
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

fn cancellation_target(command: &InteractionCommand) -> Option<&str> {
    let prefix = match command.interaction_id.as_str() {
        "cancel_feed" => "feed-",
        "cancel_boat" => "boat-",
        _ => return None,
    };
    let id = command.target_action_id.as_deref()?.strip_prefix(prefix)?;
    if id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Some(id)
    } else {
        None
    }
}

fn feed_limit_code(
    state: &Value,
    world: &World,
    interaction_id: &str,
    actor_id: Uuid,
    now_ms: i64,
) -> Result<Option<&'static str>, AccessError> {
    let (pending, same_pending) = match state.get("pendingInteractions") {
        None => (0, 0),
        Some(Value::Array(entries)) => entries
            .iter()
            .filter_map(|entry| entry.get("interactionId").and_then(Value::as_str))
            .filter(|id| {
                world
                    .action_rule(id)
                    .is_some_and(|(effect, _)| effect == InteractionEffect::Attraction)
            })
            .fold((0, 0), |(total, same), id| {
                (total + 1, same + if id == interaction_id { 1 } else { 0 })
            }),
        _ => return Err(AccessError::SceneState),
    };
    let (_, rule) = world
        .action_rule(interaction_id)
        .ok_or(AccessError::SceneState)?;
    if world.feed_sources().len() + pending >= world.interaction_rules().feed.max_active {
        return Ok(Some("FEED_LIMIT"));
    }
    if world
        .feed_sources()
        .iter()
        .filter(|source| source.interaction_id == interaction_id)
        .count()
        + same_pending
        >= rule.max_active
    {
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
            .is_some_and(|last| {
                now_ms.saturating_sub(last)
                    < cooldown_ms(category_cooldown(world, InteractionEffect::Attraction))
            })
        {
            return Ok(Some("FEED_SCENE_COOLDOWN"));
        }
        let actor_key = actor_id.to_string();
        if limits
            .get("feedByActorMs")
            .and_then(|map| map.get(actor_key.as_str()))
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
    actor_id: Uuid,
    now_ms: i64,
) -> Result<(), AccessError> {
    let object = state.as_object_mut().ok_or(AccessError::SceneState)?;
    let limits = object
        .entry("interactionLimits")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(AccessError::SceneState)?;
    let by_actor = limits
        .entry("feedByActorMs")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(AccessError::SceneState)?;
    by_actor.retain(|_, value| {
        value
            .as_i64()
            .is_some_and(|last| now_ms.saturating_sub(last) < 1000)
    });
    by_actor.insert(actor_id.to_string(), json!(now_ms));
    limits.insert("lastSceneFeedMs".into(), json!(now_ms));
    Ok(())
}

fn boat_limit_code(
    state: &Value,
    world: &World,
    now_ms: i64,
) -> Result<Option<&'static str>, AccessError> {
    let pending = match state.get("pendingInteractions") {
        None => false,
        Some(Value::Array(entries)) => entries.iter().any(|entry| {
            entry
                .get("interactionId")
                .and_then(Value::as_str)
                .is_some_and(|id| {
                    world
                        .action_rule(id)
                        .is_some_and(|(effect, _)| effect == InteractionEffect::Threat)
                })
        }),
        _ => return Err(AccessError::SceneState),
    };
    if world.boat().is_some() || pending {
        return Ok(Some("BOAT_LIMIT"));
    }
    if let Some(limits) = state.get("interactionLimits") {
        if !limits.is_object() {
            return Err(AccessError::SceneState);
        }
        if limits
            .get("lastSceneBoatMs")
            .and_then(Value::as_i64)
            .is_some_and(|last| {
                now_ms.saturating_sub(last)
                    < cooldown_ms(category_cooldown(world, InteractionEffect::Threat))
            })
        {
            return Ok(Some("BOAT_SCENE_COOLDOWN"));
        }
    }
    Ok(None)
}

fn record_boat_acceptance(state: &mut Value, now_ms: i64) -> Result<(), AccessError> {
    let limits = state
        .as_object_mut()
        .ok_or(AccessError::SceneState)?
        .entry("interactionLimits")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(AccessError::SceneState)?;
    limits.insert("lastSceneBoatMs".into(), json!(now_ms));
    Ok(())
}

fn cooldown_ms(ticks: u64) -> i64 {
    (ticks * 1000 / TICKS_PER_SECOND) as i64
}

fn category_cooldown(world: &World, effect: InteractionEffect) -> u64 {
    let baseline = match effect {
        InteractionEffect::Attraction => world.interaction_rules().feed.cooldown_ticks,
        InteractionEffect::Threat => world.interaction_rules().boat.cooldown_ticks,
    };
    world
        .action_definitions()
        .iter()
        .filter(|item| item.effect == effect)
        .map(|item| item.rule.cooldown_ticks)
        .max()
        .unwrap_or(baseline)
        .max(baseline)
}

fn restored_scene_world(scene_id: Uuid, tick: i64, state: &Value) -> Result<World, AccessError> {
    if tick < 0 {
        return Err(AccessError::SceneState);
    }
    if let Some(value) = state.get("simulation") {
        let checkpoint: WorldCheckpoint =
            serde_json::from_value(value.clone()).map_err(|_| AccessError::SceneState)?;
        if checkpoint.tick != tick as u64 {
            return Err(AccessError::SceneState);
        }
        return World::restore(checkpoint).map_err(|_| AccessError::SceneState);
    }
    if tick != 0
        || state
            .get("entities")
            .and_then(Value::as_array)
            .is_some_and(|entities| !entities.is_empty())
    {
        return Err(AccessError::SceneState);
    }
    simulation::unstarted_world(scene_id, state).map_err(|_| AccessError::SceneState)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_action_id: Option<String>,
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
    let mut changes = hub.subscribe_changes();
    let Ok((snapshot, mut cursor)) = load_snapshot(store.pool(), &access).await else {
        return;
    };
    let mut wire_catalog = snapshot.get("actionCatalog").cloned();
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
                        let response = handle_message(&store, &hub, kind, &token, session_id, &text).await;
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
            _ = async {
                loop {
                    tokio::select! {
                        _ = poll.tick() => break,
                        change = changes.recv() => match change {
                            Ok(scene_id) if scene_id == access.scene.scene_id => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            _ => {}
                        }
                    }
                }
            } => {
                let Ok(current) = store.scene_access(kind, &token, session_id).await else { break };
                if current.scene.scene_id != access.scene.scene_id
                    || current.scene.scene_epoch != access.scene.scene_epoch {
                    let Ok((snapshot, new_cursor)) = load_snapshot(store.pool(), &current).await else { break };
                    if send_json(&mut socket, &snapshot).await.is_err() { break; }
                    wire_catalog = snapshot.get("actionCatalog").cloned();
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
                    let event = if let Some(catalog) = &wire_catalog {
                        let Ok(event) = public_event(&event, catalog) else { return };
                        event
                    } else { event };
                    let delta = json!({"type":"delta", "sceneId":access.scene.scene_id,
                        "schemaVersion":if wire_catalog.is_some() { 2 } else { 1 },
                        "sceneEpoch":epoch, "revision":revision,
                        "simulationTick":event.get("simulationTick").and_then(Value::as_u64).unwrap_or(0),
                        "upsert":upsert, "remove":[], "event":event});
                    if send_json(&mut socket, &delta).await.is_err() { return; }
                    cursor = revision;
                }
                if gap {
                    let Ok((snapshot, new_cursor)) = load_snapshot(store.pool(), &current).await else { break };
                    if send_json(&mut socket, &snapshot).await.is_err() { break; }
                    wire_catalog = snapshot.get("actionCatalog").cloned();
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
    hub: &SimulationHub,
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
                Ok(outcome) => {
                    if outcome.get("accepted").and_then(Value::as_bool) == Some(true) {
                        hub.notify_change(command.scene_id);
                    }
                    outcome
                }
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

fn public_catalog(world: &World) -> Option<Value> {
    (world.action_definitions().len() > 2).then(|| {
        json!(
            world
                .action_definitions()
                .iter()
                .map(
                    |definition| json!({"id":definition.id, "effect":definition.effect,
            "label":definition.label, "allowedZoneId":definition.allowed_zone_id})
                )
                .collect::<Vec<_>>()
        )
    })
}

fn public_actions(actions: &Value, catalog: &Value) -> Result<Value, AccessError> {
    let entries = actions.as_array().ok_or(AccessError::SceneState)?;
    let definitions = catalog.as_array().ok_or(AccessError::SceneState)?;
    let mut result = Vec::with_capacity(entries.len());
    for action in entries {
        let id = action
            .get("interactionId")
            .and_then(Value::as_str)
            .ok_or(AccessError::SceneState)?;
        let effect = definitions
            .iter()
            .find(|definition| definition.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|definition| definition.get("effect"))
            .ok_or(AccessError::SceneState)?;
        let mut action = action.clone();
        action
            .as_object_mut()
            .ok_or(AccessError::SceneState)?
            .insert("effect".into(), effect.clone());
        result.push(action);
    }
    Ok(json!(result))
}

fn public_event(event: &Value, catalog: &Value) -> Result<Value, AccessError> {
    let mut event = event.clone();
    if event.get("type").and_then(Value::as_str) == Some("interaction_state") {
        let actions = public_actions(
            event.get("activeActions").ok_or(AccessError::SceneState)?,
            catalog,
        )?;
        event
            .as_object_mut()
            .ok_or(AccessError::SceneState)?
            .insert("activeActions".into(), actions);
    }
    Ok(event)
}

async fn load_snapshot(pool: &PgPool, access: &SceneAccess) -> Result<(Value, i64), AccessError> {
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
    let world = restored_scene_world(access.scene.scene_id, tick, &state)?;
    let catalog = public_catalog(&world);
    let actions = state.get("activeActions").cloned().unwrap_or(json!([]));
    let actions = if let Some(catalog) = &catalog {
        public_actions(&actions, catalog)?
    } else {
        actions
    };
    let mut snapshot = json!({
        "type":"snapshot", "schemaVersion":if catalog.is_some() { 2 } else { 1 },
        "sceneId":access.scene.scene_id,
        "sceneEpoch":epoch, "revision":revision, "simulationTick":tick,
        "serverTime":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64,
        "worldId":world_id, "worldVersion":world_version,
        "simulationVersion":1,
        "entities":state.get("entities").cloned().unwrap_or(json!([])),
        "activeActions":actions,
        "pendingInteractions":state.get("pendingInteractions").cloned().unwrap_or(json!([])),
        "resources":state.get("resources").cloned().unwrap_or(json!({})),
        "reservations":state.get("reservations").cloned().unwrap_or(json!([]))
    });
    if let Some(catalog) = catalog {
        snapshot["actionCatalog"] = catalog;
    }
    Ok((snapshot, revision))
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
    let actor_id: Uuid = match kind {
        GrantKind::Owner => {
            sqlx::query_scalar("SELECT account_id FROM owner_grants WHERE id = $1")
                .bind(access.grant_id)
                .fetch_one(&mut *tx)
                .await?
        }
        GrantKind::Controller => {
            sqlx::query_scalar("SELECT participant_id FROM device_grants WHERE id = $1")
                .bind(access.grant_id)
                .fetch_one(&mut *tx)
                .await?
        }
        GrantKind::Viewer => access.grant_id,
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AccessError::Crypto)?
        .as_millis() as i64;
    let world = restored_scene_world(access.scene.scene_id, persisted_tick, &state)?;
    let mut initial_checkpoint = None;
    let cancelling = matches!(
        command.interaction_id.as_str(),
        "cancel_feed" | "cancel_boat"
    );
    let mut code = if command.kind != "command" || command.session_id != session_id {
        Some("INVALID_COMMAND")
    } else if command.scene_id != access.scene.scene_id || command.scene_epoch != epoch {
        Some("STALE_SCENE")
    } else if command.expires_at < now_ms || command.expires_at > now_ms + 10_000 {
        Some("EXPIRED_COMMAND")
    } else if !access.may_interact() {
        Some("READ_ONLY")
    } else if cancelling && kind != GrantKind::Owner {
        Some("OWNER_REQUIRED")
    } else if status != "running" {
        Some("SCENE_NOT_RUNNING")
    } else if let Some(reason) = validate_interaction(&access, command, &world) {
        Some(reason)
    } else {
        None
    };
    if code.is_none() && cancelling {
        let id = cancellation_target(command).ok_or(AccessError::SceneState)?;
        let target = command
            .target_action_id
            .as_deref()
            .ok_or(AccessError::SceneState)?;
        let active = if command.interaction_id == "cancel_feed" {
            world.feed_sources().iter().any(|source| source.id == id)
        } else {
            world.boat().is_some_and(|boat| boat.id == id)
        };
        let pending = match state.get("pendingInteractions") {
            None => &[][..],
            Some(Value::Array(entries)) => entries.as_slice(),
            _ => return Err(AccessError::SceneState),
        };
        let start_effect = if command.interaction_id == "cancel_feed" {
            InteractionEffect::Attraction
        } else {
            InteractionEffect::Threat
        };
        let awaiting_start = pending.iter().any(|entry| {
            entry
                .get("interactionId")
                .and_then(Value::as_str)
                .is_some_and(|id| {
                    world
                        .action_rule(id)
                        .is_some_and(|(effect, _)| effect == start_effect)
                })
                && entry
                    .get("commandId")
                    .and_then(Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .is_some_and(|value| value.simple().to_string() == id)
        });
        let already_cancelling = pending.iter().any(|entry| {
            entry.get("targetActionId").and_then(Value::as_str) == Some(target)
                && entry.get("interactionId").and_then(Value::as_str)
                    == Some(command.interaction_id.as_str())
        });
        if (!active && !awaiting_start) || already_cancelling {
            code = Some("ACTION_NOT_ACTIVE");
        }
    }
    if code.is_none() && !cancelling {
        let mut candidate = world.clone();
        let (effect, _) = world
            .action_rule(&command.interaction_id)
            .ok_or(AccessError::SceneState)?;
        code = match effect {
            InteractionEffect::Attraction => {
                feed_limit_code(&state, &world, &command.interaction_id, actor_id, now_ms)?
            }
            InteractionEffect::Threat => boat_limit_code(&state, &world, now_ms)?,
        };
        if code.is_none() {
            let point = SimPoint {
                x: command.point.x as f32,
                y: command.point.y as f32,
            };
            code = match candidate.start_action(
                &command.interaction_id,
                &command.command_id.simple().to_string(),
                point,
            ) {
                Ok(()) => None,
                Err(ldw_sim::SimError::InvalidPosition) => Some("OUTSIDE_WATER"),
                Err(ldw_sim::SimError::FeedLimit) => Some("FEED_LIMIT"),
                Err(ldw_sim::SimError::InvalidBoatRoute) => Some("INVALID_BOAT_ROUTE"),
                Err(ldw_sim::SimError::BoatLimit) => Some("BOAT_LIMIT"),
                Err(_) => return Err(AccessError::SceneState),
            };
            if code.is_none() && state.get("simulation").is_none() {
                if simulation::simulation_slot_available(&mut tx).await? {
                    initial_checkpoint = Some(world.checkpoint());
                } else {
                    code = Some("SIMULATED_SESSION_LIMIT");
                }
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
        let mut event = json!({"type":"interaction_requested","commandId":command.command_id,
            "interactionId":command.interaction_id,"point":command.point});
        if let Some(target) = &command.target_action_id {
            event["targetActionId"] = json!(target);
        }
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
        if !cancelling {
            match world
                .action_rule(&command.interaction_id)
                .ok_or(AccessError::SceneState)?
                .0
            {
                InteractionEffect::Attraction => {
                    record_feed_acceptance(&mut state, actor_id, now_ms)?
                }
                InteractionEffect::Threat => record_boat_acceptance(&mut state, now_ms)?,
            }
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

#[cfg(test)]
mod rule_tests {
    use super::*;
    use crate::{blob_store::BlobStore, http};
    use futures_util::{SinkExt, StreamExt};
    use ldw_sim::{Bounds, WorldInteractionRules};
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::{Message as ClientMessage, client::IntoClientRequest};

    #[tokio::test]
    async fn frozen_catalog_accepts_extra_action_and_emits_v2() {
        let pool = crate::test_pool().await;
        let store = AccessStore::new(pool.clone(), [7u8; 32]);
        let owner = Uuid::new_v4();
        let token = format!("catalog-owner-{owner}");
        let csrf = "catalog-owner-csrf";
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("catalog-{owner}"))
            .execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
            VALUES ($1, $2, $3, $4, now() + interval '1 hour')",
        )
        .bind(Uuid::new_v4())
        .bind(owner)
        .bind(crate::access::hash_token(&token).to_vec())
        .bind(crate::access::hash_token(csrf).to_vec())
        .execute(&pool)
        .await
        .unwrap();
        let ids = store.create_session(&token, csrf).await.unwrap();
        let stored: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(ids.scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let frozen: WorldCheckpoint =
            serde_json::from_value(stored.get("initialSimulation").cloned().unwrap()).unwrap();
        assert_eq!(frozen.action_definitions.len(), 2);
        let mut package: Value = serde_json::from_str(include_str!(
            "../../../content/underwater/interactions.json"
        ))
        .unwrap();
        let mut extra = package[0].clone();
        extra["id"] = json!("feed-slow");
        extra["label"] = json!("Медленный корм");
        extra["cooldownTicks"] = json!(40);
        extra["behavior"][2]["depth"] = json!(-0.8);
        package.as_array_mut().unwrap().push(extra);
        let initial = simulation::initial_world_with_package(ids.scene_id, &package.to_string())
            .unwrap()
            .checkpoint();
        assert_eq!(initial.action_definitions[2].id, "feed-slow");
        sqlx::query("UPDATE scenes SET state = $1::jsonb WHERE id = $2")
            .bind(json!({"initialSimulation":initial}))
            .bind(ids.scene_id)
            .execute(&pool)
            .await
            .unwrap();
        let access = store
            .scene_access(GrantKind::Owner, &token, ids.session_id)
            .await
            .unwrap();
        let (snapshot, revision) = load_snapshot(&pool, &access).await.unwrap();
        assert_eq!(revision, 0);
        assert_eq!(snapshot["schemaVersion"], 2);
        assert_eq!(snapshot["actionCatalog"][2]["id"], "feed-slow");
        let blob_root = std::env::temp_dir().join(format!("ldw-catalog-blobs-{}", Uuid::new_v4()));
        let hub = SimulationHub::default();
        let app = http::router(AppState {
            access: store.clone(),
            blob_store: BlobStore::create(blob_root.clone()).unwrap(),
            public_origin: Arc::from("https://world.example.test"),
            simulation_hub: hub,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let url = format!("ws://{address}/api/sessions/{}/ws", ids.session_id);
        let mut request = url.as_str().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "https://world.example.test".parse().unwrap());
        request.headers_mut().insert(
            "cookie",
            format!("__Host-ldw-owner={token}").parse().unwrap(),
        );
        let (mut socket, _) = timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .unwrap()
        .unwrap();
        socket
            .send(ClientMessage::text(
                json!({"type":"hello","csrf":csrf}).to_string(),
            ))
            .await
            .unwrap();
        let wire_snapshot: Value = serde_json::from_str(
            timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(wire_snapshot["type"], "snapshot");
        assert_eq!(wire_snapshot["schemaVersion"], 2);
        assert_eq!(wire_snapshot["actionCatalog"][2]["id"], "feed-slow");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let command = InteractionCommand {
            kind: "command".into(),
            command_id: Uuid::new_v4(),
            session_id: ids.session_id,
            scene_id: ids.scene_id,
            scene_epoch: 1,
            interaction_id: "feed-slow".into(),
            point: Point { x: 0.0, y: 0.0 },
            target_action_id: None,
            expires_at: now + 10_000,
        };
        socket
            .send(ClientMessage::text(
                serde_json::to_string(&command).unwrap(),
            ))
            .await
            .unwrap();
        let accepted: Value = serde_json::from_str(
            timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(accepted["accepted"], true);
        let delta: Value = serde_json::from_str(
            timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(delta["type"], "delta");
        assert_eq!(delta["schemaVersion"], 2);
        assert_eq!(delta["event"]["interactionId"], "feed-slow");
        assert_eq!(
            process_command(&store, GrantKind::Owner, &token, ids.session_id, &command)
                .await
                .unwrap(),
            accepted
        );
        let (after, _) = load_snapshot(&pool, &access).await.unwrap();
        assert_eq!(after["schemaVersion"], 2);
        assert_eq!(
            after["pendingInteractions"][0]["interactionId"],
            "feed-slow"
        );
        let mut second = InteractionCommand {
            command_id: Uuid::new_v4(),
            interaction_id: "feed".into(),
            ..command.clone()
        };
        let cooldown = process_command(&store, GrantKind::Owner, &token, ids.session_id, &second)
            .await
            .unwrap();
        assert_eq!(cooldown["code"], "FEED_SCENE_COOLDOWN");
        second.command_id = Uuid::new_v4();
        second.interaction_id = "unknown".into();
        let unknown = process_command(&store, GrantKind::Owner, &token, ids.session_id, &second)
            .await
            .unwrap();
        assert_eq!(unknown["code"], "UNKNOWN_INTERACTION");
        let target = format!("feed-{}", command.command_id.simple());
        second.command_id = Uuid::new_v4();
        second.interaction_id = "cancel_feed".into();
        second.target_action_id = Some(target.clone());
        let cancelled = process_command(&store, GrantKind::Owner, &token, ids.session_id, &second)
            .await
            .unwrap();
        assert_eq!(cancelled["accepted"], true);
        let event = json!({"type":"interaction_state","activeActions":[{
            "id":target,"interactionId":"feed-slow","point":{"x":0,"y":0},
            "remaining":10,"expiresAtTick":300}],"appliedCommandIds":[],"simulationTick":0});
        let wire = public_event(&event, &snapshot["actionCatalog"]).unwrap();
        assert_eq!(wire["activeActions"][0]["effect"], "attraction");
        assert!(event["activeActions"][0].get("effect").is_none());
        socket.close(None).await.unwrap();
        server.abort();
        std::fs::remove_dir_all(blob_root).unwrap();
    }

    #[test]
    fn scene_limits_follow_checkpointed_rules() {
        let mut rules = WorldInteractionRules::default();
        rules.feed.max_active = 1;
        rules.feed.cooldown_ticks = 4;
        rules.boat.cooldown_ticks = 40;
        let mut world = World::new_with_rules(
            Bounds {
                min_x: -8.0,
                max_x: 8.0,
                min_y: -4.0,
                max_y: 4.0,
            },
            1,
            rules,
        )
        .unwrap();
        let actor = Uuid::new_v4();
        let state = json!({"interactionLimits": {"lastSceneFeedMs": 1000,
            "lastSceneBoatMs": 1000}});
        assert_eq!(
            feed_limit_code(&state, &world, "feed", actor, 1199).unwrap(),
            Some("FEED_SCENE_COOLDOWN")
        );
        assert_eq!(
            feed_limit_code(&state, &world, "feed", actor, 1200).unwrap(),
            None
        );
        assert_eq!(
            boat_limit_code(&state, &world, 2999).unwrap(),
            Some("BOAT_SCENE_COOLDOWN")
        );
        assert_eq!(boat_limit_code(&state, &world, 3000).unwrap(), None);
        world
            .start_feed(
                "00000000000000000000000000000001",
                SimPoint { x: 0.0, y: 0.0 },
            )
            .unwrap();
        assert_eq!(
            feed_limit_code(&state, &world, "feed", actor, 3000).unwrap(),
            Some("FEED_LIMIT")
        );
        let restored = World::restore(world.checkpoint()).unwrap();
        assert_eq!(
            feed_limit_code(&state, &restored, "feed", actor, 3000).unwrap(),
            Some("FEED_LIMIT")
        );
    }
}
