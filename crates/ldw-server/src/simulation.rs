//! Persisted boundary for the server-owned simulation. The timer and broadcaster
//! are separate; this module never writes a frame to PostgreSQL.

use ldw_sim::{
    ActionDefinition, BoatBehavior, Bounds, EffectRule, FeedBehavior, Fish, FishCapabilities,
    InteractionEffect, MAX_ACTION_DEFINITIONS, Point, World, WorldCheckpoint,
    WorldInteractionRules,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::{broadcast, watch},
    task::JoinSet,
    time::{MissedTickBehavior, interval},
};
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum SimulationError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("unsupported or inconsistent scene checkpoint")]
    InvalidScene,
    #[error("invalid underwater world package")]
    InvalidPackage,
    #[error("invalid simulation checkpoint: {0}")]
    Checkpoint(#[from] serde_json::Error),
    #[error("invalid first fish publication")]
    InvalidPublication,
    #[error("three simulated sessions are already running")]
    SessionLimit,
}

#[cfg(not(test))]
const MAX_SIMULATED_SESSIONS: i64 = 3;
const ADMISSION_LOCK: i64 = i64::from_be_bytes(*b"LDWSIM03");

/// Call only inside the transaction that creates a scene's first checkpoint.
/// The transaction-level lock serializes the count and the subsequent write.
pub(crate) async fn simulation_slot_available(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<bool, sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADMISSION_LOCK)
        .execute(&mut **tx)
        .await?;
    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions s JOIN scenes c ON c.id = s.active_scene_id \
         WHERE s.status = 'running' AND c.state ? 'simulation'",
    )
    .fetch_one(&mut **tx)
    .await?;
    #[cfg(test)]
    let limit = std::env::var("LDW_TEST_SIMULATED_SESSION_LIMIT")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(1_000);
    #[cfg(not(test))]
    let limit = MAX_SIMULATED_SESSIONS;
    Ok(running < limit)
}

pub struct LoadedScene {
    pub epoch: i64,
    pub revision: i64,
    pub persisted_tick: i64,
    pub world: World,
}

#[derive(Clone)]
pub struct SimulationHub {
    sender: broadcast::Sender<PositionFrame>,
    changes: broadcast::Sender<Uuid>,
}

impl Default for SimulationHub {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(32);
        let (changes, _) = broadcast::channel(64);
        Self { sender, changes }
    }
}

impl SimulationHub {
    pub fn subscribe(&self) -> broadcast::Receiver<PositionFrame> {
        self.sender.subscribe()
    }

    pub fn publish(&self, frame: PositionFrame) {
        let _ = self.sender.send(frame);
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<Uuid> {
        self.changes.subscribe()
    }

    pub fn notify_change(&self, scene_id: Uuid) {
        let _ = self.changes.send(scene_id);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionFrame {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub schema_version: u8,
    pub scene_id: Uuid,
    pub scene_epoch: i64,
    pub revision: i64,
    pub simulation_tick: u64,
    pub positions: Vec<EntityPosition>,
    pub action_positions: Vec<ActionPosition>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EntityPosition {
    pub id: String,
    pub position: ldw_sim::Point,
    pub heading: ldw_sim::Point,
    pub depth: f32,
    #[serde(rename = "headingDepth")]
    pub heading_depth: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActionPosition {
    pub id: String,
    pub position: ldw_sim::Point,
}

fn position_frame(scene_id: Uuid, scene: &LoadedScene) -> PositionFrame {
    PositionFrame {
        kind: "positions",
        schema_version: 1,
        scene_id,
        scene_epoch: scene.epoch,
        revision: scene.revision,
        simulation_tick: scene.world.tick_number(),
        positions: scene
            .world
            .fish()
            .iter()
            .map(|fish| EntityPosition {
                id: format!("fish-{:032x}", fish.id),
                position: fish.position,
                heading: fish.heading,
                depth: fish.depth,
                heading_depth: fish.heading_depth,
            })
            .collect(),
        action_positions: scene
            .world
            .boat()
            .map(|boat| {
                vec![ActionPosition {
                    id: format!("boat-{}", boat.id),
                    position: boat.position,
                }]
            })
            .unwrap_or_default(),
    }
}

fn underwater_bounds() -> Result<Bounds, SimulationError> {
    let package: Value =
        serde_json::from_str(include_str!("../../../content/underwater/world.json"))
            .map_err(|_| SimulationError::InvalidPackage)?;
    let bounds: [f32; 4] = serde_json::from_value(package["zones"][0]["bounds"].clone())
        .map_err(|_| SimulationError::InvalidPackage)?;
    Ok(Bounds {
        min_x: bounds[0],
        max_x: bounds[1],
        min_y: bounds[2],
        max_y: bounds[3],
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageInteraction {
    schema_version: u32,
    id: String,
    label: Option<String>,
    version: u32,
    effect: String,
    allowed_zone_id: String,
    required_capability: String,
    radius: f32,
    duration_ticks: u64,
    max_active: usize,
    cooldown_ticks: u64,
    priority: i32,
    behavior: Option<Vec<PackageBehaviorStep>>,
}

#[derive(Deserialize)]
#[serde(tag = "primitive", rename_all = "kebab-case", deny_unknown_fields)]
enum PackageBehaviorStep {
    FindCandidates {
        #[serde(rename = "maxCandidates")]
        max_candidates: usize,
    },
    Reserve {
        #[serde(rename = "maxPerSource")]
        max_per_source: usize,
    },
    MoveTo {
        depth: f32,
    },
    Consume {
        radius: f32,
        #[serde(rename = "depthTolerance")]
        depth_tolerance: f32,
    },
    Flee {
        #[serde(rename = "holdTicks")]
        hold_ticks: u64,
        #[serde(rename = "releaseRadiusFactor")]
        release_radius_factor: f32,
        #[serde(rename = "escapeDepth")]
        escape_depth: f32,
    },
    Timeout,
    Cleanup,
}

fn compile_feed_behavior(steps: &[PackageBehaviorStep]) -> Result<FeedBehavior, SimulationError> {
    let [
        PackageBehaviorStep::FindCandidates { max_candidates },
        PackageBehaviorStep::Reserve { max_per_source },
        PackageBehaviorStep::MoveTo { depth },
        PackageBehaviorStep::Consume {
            radius,
            depth_tolerance,
        },
        PackageBehaviorStep::Cleanup,
    ] = steps
    else {
        return Err(SimulationError::InvalidPackage);
    };
    Ok(FeedBehavior {
        candidate_limit: *max_candidates,
        reserve_limit: *max_per_source,
        target_depth: *depth,
        eating_radius: *radius,
        eating_depth_tolerance: *depth_tolerance,
    })
}

fn compile_boat_behavior(steps: &[PackageBehaviorStep]) -> Result<BoatBehavior, SimulationError> {
    let [
        PackageBehaviorStep::FindCandidates { max_candidates },
        PackageBehaviorStep::Flee {
            hold_ticks,
            release_radius_factor,
            escape_depth,
        },
        PackageBehaviorStep::Timeout,
        PackageBehaviorStep::Cleanup,
    ] = steps
    else {
        return Err(SimulationError::InvalidPackage);
    };
    Ok(BoatBehavior {
        candidate_limit: *max_candidates,
        hold_ticks: *hold_ticks,
        release_radius_factor: *release_radius_factor,
        escape_depth: *escape_depth,
    })
}

struct InteractionPackage {
    rules: WorldInteractionRules,
    catalog: Vec<ActionDefinition>,
}

#[cfg(test)]
fn parse_interaction_rules(source: &str) -> Result<WorldInteractionRules, SimulationError> {
    parse_interaction_package(source).map(|package| package.rules)
}

fn parse_interaction_package(source: &str) -> Result<InteractionPackage, SimulationError> {
    let definitions: Vec<PackageInteraction> =
        serde_json::from_str(source).map_err(|_| SimulationError::InvalidPackage)?;
    if !(2..=MAX_ACTION_DEFINITIONS).contains(&definitions.len()) {
        return Err(SimulationError::InvalidPackage);
    }
    let mut feed = None;
    let mut boat = None;
    let mut feed_behavior = None;
    let mut boat_behavior = None;
    let mut catalog = Vec::with_capacity(definitions.len());
    let mut ids = HashSet::with_capacity(definitions.len());
    for definition in definitions {
        if !matches!(definition.schema_version, 1 | 2)
            || definition.version == 0
            || definition.allowed_zone_id != "water"
            || (definition.schema_version == 1 && definition.label.is_some())
            || !ids.insert(definition.id.clone())
        {
            return Err(SimulationError::InvalidPackage);
        }
        let behavior = match (definition.schema_version, definition.behavior.as_deref()) {
            (1, None) => None,
            (2, Some(steps)) => Some(steps),
            _ => return Err(SimulationError::InvalidPackage),
        };
        let rule = EffectRule {
            definition_version: definition.version,
            radius: definition.radius,
            duration_ticks: definition.duration_ticks,
            max_active: definition.max_active,
            cooldown_ticks: definition.cooldown_ticks,
            priority: definition.priority,
        };
        let (effect, compiled_feed, compiled_boat) = match (
            definition.effect.as_str(),
            definition.required_capability.as_str(),
        ) {
            ("attraction", "consume-food") => (
                InteractionEffect::Attraction,
                Some(match behavior {
                    Some(steps) => compile_feed_behavior(steps)?,
                    None => FeedBehavior::default(),
                }),
                None,
            ),
            ("threat", "avoid-threat") => (
                InteractionEffect::Threat,
                None,
                Some(match behavior {
                    Some(steps) => compile_boat_behavior(steps)?,
                    None => BoatBehavior::default(),
                }),
            ),
            _ => return Err(SimulationError::InvalidPackage),
        };
        match definition.id.as_str() {
            "feed" if effect == InteractionEffect::Attraction => {
                feed = Some(rule);
                feed_behavior = compiled_feed;
            }
            "boat" if effect == InteractionEffect::Threat => {
                boat = Some(rule);
                boat_behavior = compiled_boat;
            }
            "feed" | "boat" => return Err(SimulationError::InvalidPackage),
            _ => {}
        }
        let label = definition
            .label
            .or_else(|| match definition.id.as_str() {
                "feed" => Some("Корм".into()),
                "boat" => Some("Лодка".into()),
                _ => None,
            })
            .ok_or(SimulationError::InvalidPackage)?;
        catalog.push(ActionDefinition {
            id: definition.id,
            label,
            allowed_zone_id: definition.allowed_zone_id,
            effect,
            rule,
            feed_behavior: compiled_feed,
            boat_behavior: compiled_boat,
        });
    }
    let rules = WorldInteractionRules {
        feed: feed.ok_or(SimulationError::InvalidPackage)?,
        boat: boat.ok_or(SimulationError::InvalidPackage)?,
        feed_behavior: feed_behavior.ok_or(SimulationError::InvalidPackage)?,
        boat_behavior: boat_behavior.ok_or(SimulationError::InvalidPackage)?,
    };
    World::new_with_catalog(underwater_bounds()?, 0, rules, catalog.clone())
        .map_err(|_| SimulationError::InvalidPackage)?;
    Ok(InteractionPackage { rules, catalog })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageEntity {
    schema_version: u32,
    id: String,
    version: u32,
    model_asset_id: String,
    paint_template_id: String,
    paint_template_version: u32,
    capabilities: Vec<String>,
}

#[derive(Clone, Copy)]
struct FishDefinition {
    version: u32,
    capabilities: FishCapabilities,
}

fn parse_entity_definitions(
    source: &str,
) -> Result<HashMap<String, FishDefinition>, SimulationError> {
    let definitions: Vec<PackageEntity> =
        serde_json::from_str(source).map_err(|_| SimulationError::InvalidPackage)?;
    if definitions.len() != 2 {
        return Err(SimulationError::InvalidPackage);
    }
    let mut result = HashMap::with_capacity(2);
    for definition in definitions {
        let (model, template) = match definition.id.as_str() {
            "coral-fish" => ("coral-model", "coral"),
            "stream-fish" => ("stream-model", "stream"),
            _ => return Err(SimulationError::InvalidPackage),
        };
        if definition.schema_version != 1
            || definition.version != 1
            || definition.model_asset_id != model
            || definition.paint_template_id != template
            || definition.paint_template_version != 1
        {
            return Err(SimulationError::InvalidPackage);
        }
        let mut capabilities = FishCapabilities {
            consume_food: false,
            avoid_threat: false,
        };
        for capability in definition.capabilities {
            match capability.as_str() {
                "consume-food" if !capabilities.consume_food => capabilities.consume_food = true,
                "avoid-threat" if !capabilities.avoid_threat => capabilities.avoid_threat = true,
                _ => return Err(SimulationError::InvalidPackage),
            }
        }
        if result
            .insert(
                definition.id,
                FishDefinition {
                    version: definition.version,
                    capabilities,
                },
            )
            .is_some()
        {
            return Err(SimulationError::InvalidPackage);
        }
    }
    Ok(result)
}

fn underwater_entity_definitions() -> Result<HashMap<String, FishDefinition>, SimulationError> {
    parse_entity_definitions(include_str!("../../../content/underwater/entities.json"))
}

fn underwater_fish_definition(id: &str) -> Result<FishDefinition, SimulationError> {
    underwater_entity_definitions()?
        .get(id)
        .copied()
        .ok_or(SimulationError::InvalidPackage)
}

pub(crate) fn initial_world(scene_id: Uuid) -> Result<World, SimulationError> {
    initial_world_with_package(
        scene_id,
        include_str!("../../../content/underwater/interactions.json"),
    )
}

pub(crate) fn initial_world_with_package(
    scene_id: Uuid,
    interactions: &str,
) -> Result<World, SimulationError> {
    underwater_entity_definitions()?;
    let seed = (scene_id.as_u128() as u64) ^ ((scene_id.as_u128() >> 64) as u64);
    let package = parse_interaction_package(interactions)?;
    World::new_with_catalog(underwater_bounds()?, seed, package.rules, package.catalog)
        .map_err(|_| SimulationError::InvalidPackage)
}

/// A new scene freezes its package before its first fish or command. Legacy
/// scenes without this field retain the historical lazy-initialization path.
pub(crate) fn unstarted_world(scene_id: Uuid, state: &Value) -> Result<World, SimulationError> {
    let Some(value) = state.get("initialSimulation") else {
        return initial_world(scene_id);
    };
    let checkpoint: WorldCheckpoint = serde_json::from_value(value.clone())?;
    if checkpoint.tick != 0 || checkpoint.bounds != underwater_bounds()? {
        return Err(SimulationError::InvalidScene);
    }
    World::restore(checkpoint).map_err(|_| SimulationError::InvalidScene)
}

fn active_actions(world: &World) -> Value {
    let mut actions: Vec<Value> = world
        .feed_sources()
        .iter()
        .map(|source| {
            json!({
                "id":format!("feed-{}", source.id),
                "interactionId":source.interaction_id, "point":source.position,
                "remaining":source.remaining, "expiresAtTick":source.expires_at_tick,
            })
        })
        .collect();
    if let Some(boat) = world.boat() {
        actions.push(json!({
            "id":format!("boat-{}", boat.id), "interactionId":boat.interaction_id,
            "point":boat.via, "position":boat.position,
            "entry":boat.entry, "exit":boat.exit,
            "expiresAtTick":boat.expires_at_tick,
        }));
    }
    json!(actions)
}

fn interaction_state_event(world: &World, applied: &[Uuid]) -> Value {
    json!({"type":"interaction_state", "activeActions":active_actions(world),
        "appliedCommandIds":applied, "simulationTick":world.tick_number()})
}

fn valid_paint_blob_id(paint_blob_id: &str) -> bool {
    (paint_blob_id.len() == 64
        && paint_blob_id
            .bytes()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()))
        || ((2..=64).contains(&paint_blob_id.len())
            && paint_blob_id
                .bytes()
                .next()
                .is_some_and(|ch| ch.is_ascii_lowercase())
            && paint_blob_id
                .bytes()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-'))
}

/// Trusted publication boundary: the caller must already own and validate the
/// PaintResult blob. This transaction establishes the first Entity and tick-0
/// checkpoint together; the supervisor discovers the new scene on its next scan.
pub(crate) async fn publish_first_fish(
    pool: &PgPool,
    scene_id: Uuid,
    fish_id: Uuid,
    definition_id: &str,
    paint_blob_id: &str,
    position: Point,
) -> Result<Value, SimulationError> {
    let mut tx = pool.begin().await?;
    let event = publish_first_fish_tx(
        &mut tx,
        scene_id,
        fish_id,
        definition_id,
        paint_blob_id,
        position,
    )
    .await?;
    tx.commit().await?;
    Ok(event)
}

pub(crate) async fn publish_first_fish_tx(
    tx: &mut Transaction<'_, Postgres>,
    scene_id: Uuid,
    fish_id: Uuid,
    definition_id: &str,
    paint_blob_id: &str,
    position: Point,
) -> Result<Value, SimulationError> {
    let definition = underwater_fish_definition(definition_id)
        .map_err(|_| SimulationError::InvalidPublication)?;
    if !valid_paint_blob_id(paint_blob_id) {
        return Err(SimulationError::InvalidPublication);
    }
    let row: Option<(String, i32, i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.scene_epoch, c.revision, c.simulation_tick, \
         c.state, s.status, s.active_scene_id FROM scenes c JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((world_id, world_version, epoch, revision, tick, state, status, active_scene)) = row
    else {
        return Err(SimulationError::InvalidScene);
    };
    if world_id != "underwater"
        || world_version != 1
        || status != "running"
        || active_scene != Some(scene_id)
        || tick != 0
        || state.get("simulation").is_some()
        || state
            .get("entities")
            .is_some_and(|value| !matches!(value, Value::Array(entities) if entities.is_empty()))
    {
        return Err(SimulationError::InvalidScene);
    }
    if !simulation_slot_available(tx).await? {
        return Err(SimulationError::SessionLimit);
    }
    let mut world = unstarted_world(scene_id, &state)?;
    world
        .spawn_fish_with_capabilities(fish_id.as_u128(), position, 1.2, definition.capabilities)
        .map_err(|_| SimulationError::InvalidPublication)?;
    let entity = json!({
        "id": format!("fish-{:032x}", fish_id.as_u128()),
        "definitionId": definition_id,
        "definitionVersion": definition.version,
        "paintBlobId": paint_blob_id,
        "position": position,
    });
    let event = json!({"type":"entity_published", "entity":entity});
    let new_revision = revision
        .checked_add(1)
        .ok_or(SimulationError::InvalidScene)?;
    sqlx::query(
        "UPDATE scenes SET revision = $2, updated_at = now(), \
         state = jsonb_set(jsonb_set(state, '{simulation}', $3::jsonb, true), \
         '{entities}', $4::jsonb, true) WHERE id = $1",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(serde_json::to_value(world.checkpoint())?)
    .bind(json!([entity]))
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(epoch)
    .bind(&event)
    .execute(&mut **tx)
    .await?;
    Ok(event)
}

/// Caller must validate blob ownership and the PaintResult before this boundary.
/// A queued fish is not yet visible; only the scene worker can publish it.
pub(crate) async fn queue_fish(
    pool: &PgPool,
    scene_id: Uuid,
    fish_id: Uuid,
    definition_id: &str,
    paint_blob_id: &str,
    position: Point,
) -> Result<(), SimulationError> {
    let mut tx = pool.begin().await?;
    queue_fish_tx(
        &mut tx,
        scene_id,
        fish_id,
        definition_id,
        paint_blob_id,
        position,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn queue_fish_tx(
    tx: &mut Transaction<'_, Postgres>,
    scene_id: Uuid,
    fish_id: Uuid,
    definition_id: &str,
    paint_blob_id: &str,
    position: Point,
) -> Result<(), SimulationError> {
    let definition = underwater_fish_definition(definition_id)
        .map_err(|_| SimulationError::InvalidPublication)?;
    if !valid_paint_blob_id(paint_blob_id) {
        return Err(SimulationError::InvalidPublication);
    }
    let row: Option<(String, i32, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.simulation_tick, c.state, s.status, s.active_scene_id \
         FROM scenes c JOIN sessions s ON s.id = c.session_id WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((world_id, world_version, tick, state, status, active_scene)) = row else {
        return Err(SimulationError::InvalidScene);
    };
    if world_id != "underwater"
        || world_version != 1
        || status != "running"
        || active_scene != Some(scene_id)
        || tick < 0
    {
        return Err(SimulationError::InvalidScene);
    }
    let checkpoint: WorldCheckpoint = serde_json::from_value(
        state
            .get("simulation")
            .cloned()
            .ok_or(SimulationError::InvalidScene)?,
    )?;
    if checkpoint.tick != tick as u64 || checkpoint.bounds != underwater_bounds()? {
        return Err(SimulationError::InvalidScene);
    }
    let mut world = World::restore(checkpoint).map_err(|_| SimulationError::InvalidScene)?;
    let entities = state
        .get("entities")
        .and_then(Value::as_array)
        .ok_or(SimulationError::InvalidScene)?;
    if entities.len() != world.fish().len() {
        return Err(SimulationError::InvalidScene);
    }
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM fish_publications WHERE scene_id = $1")
            .bind(scene_id)
            .fetch_one(&mut **tx)
            .await?;
    let reserved: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM upload_intents WHERE scene_id = $1 \
         AND status <> 'finalized' AND reservation_until > now()",
    )
    .bind(scene_id)
    .fetch_one(&mut **tx)
    .await?;
    let returning: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM fish_mutations WHERE scene_id = $1 AND operation = 'restore'",
    )
    .bind(scene_id)
    .fetch_one(&mut **tx)
    .await?;
    if entities.len() + queued as usize + reserved as usize + returning as usize
        >= ldw_sim::MAX_FISH
    {
        return Err(SimulationError::InvalidPublication);
    }
    world
        .spawn_fish_with_capabilities(fish_id.as_u128(), position, 1.2, definition.capabilities)
        .map_err(|_| SimulationError::InvalidPublication)?;
    sqlx::query(
        "INSERT INTO fish_publications (scene_id, fish_id, definition_id, definition_version, \
         capabilities, paint_blob_id, position_x, position_y) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(scene_id)
    .bind(fish_id)
    .bind(definition_id)
    .bind(definition.version as i32)
    .bind(serde_json::to_value(definition.capabilities)?)
    .bind(paint_blob_id)
    .bind(position.x)
    .bind(position.y)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn load_scene(pool: &PgPool, scene_id: Uuid) -> Result<LoadedScene, SimulationError> {
    let (world_id, world_version, epoch, revision, tick, state): (
        String,
        i32,
        i64,
        i64,
        i64,
        Value,
    ) = sqlx::query_as(
        "SELECT world_id, world_version, scene_epoch, revision, simulation_tick, state \
             FROM scenes WHERE id = $1",
    )
    .bind(scene_id)
    .fetch_one(pool)
    .await?;
    if world_id != "underwater" || world_version != 1 || tick < 0 {
        return Err(SimulationError::InvalidScene);
    }
    let bounds = underwater_bounds()?;
    let world = if let Some(value) = state.get("simulation") {
        let checkpoint: WorldCheckpoint = serde_json::from_value(value.clone())?;
        if checkpoint.tick != tick as u64 || checkpoint.bounds != bounds {
            return Err(SimulationError::InvalidScene);
        }
        World::restore(checkpoint).map_err(|_| SimulationError::InvalidScene)?
    } else {
        if tick != 0
            || state
                .get("entities")
                .and_then(Value::as_array)
                .is_some_and(|entities| !entities.is_empty())
        {
            return Err(SimulationError::InvalidScene);
        }
        unstarted_world(scene_id, &state)?
    };
    Ok(LoadedScene {
        epoch,
        revision,
        persisted_tick: tick,
        world,
    })
}

/// Apply the durable inbox to the worker-owned world. PostgreSQL receives the
/// matching checkpoint, Entity list, ordered events and queue deletion before
/// the in-memory world changes; a crash leaves either all or none of the batch.
async fn apply_pending_fish(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, i32, i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.scene_epoch, c.revision, c.simulation_tick, \
         c.state, s.status, s.active_scene_id FROM scenes c JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((world_id, world_version, epoch, revision, persisted_tick, state, status, active)) =
        row
    else {
        return Ok(false);
    };
    if world_id != "underwater"
        || world_version != 1
        || epoch != scene.epoch
        || persisted_tick != scene.persisted_tick
        || status != "running"
        || active != Some(scene_id)
    {
        return Ok(false);
    }
    let pending: Vec<(Uuid, String, i32, Value, String, f32, f32)> = sqlx::query_as(
        "SELECT fish_id, definition_id, definition_version, capabilities, paint_blob_id, position_x, position_y \
         FROM fish_publications WHERE scene_id = $1 ORDER BY created_at, fish_id LIMIT 100",
    )
    .bind(scene_id)
    .fetch_all(&mut *tx)
    .await?;
    if pending.is_empty() {
        return Ok(true);
    }
    let mut candidate = scene.world.clone();
    let mut entities = state
        .get("entities")
        .and_then(Value::as_array)
        .cloned()
        .ok_or(SimulationError::InvalidScene)?;
    if entities.len() != candidate.fish().len() {
        return Err(SimulationError::InvalidScene);
    }
    let mut new_revision = revision;
    let mut events = Vec::with_capacity(pending.len());
    let definitions = underwater_entity_definitions()?;
    for (fish_id, definition_id, definition_version, capabilities, paint_blob_id, x, y) in &pending
    {
        if !definitions.contains_key(definition_id)
            || *definition_version != 1
            || !valid_paint_blob_id(paint_blob_id)
        {
            return Err(SimulationError::InvalidPublication);
        }
        let capabilities: FishCapabilities = serde_json::from_value(capabilities.clone())
            .map_err(|_| SimulationError::InvalidPublication)?;
        let position = Point { x: *x, y: *y };
        candidate
            .spawn_fish_with_capabilities(fish_id.as_u128(), position, 1.2, capabilities)
            .map_err(|_| SimulationError::InvalidPublication)?;
        let entity = json!({
            "id": format!("fish-{:032x}", fish_id.as_u128()),
            "definitionId": definition_id,
            "definitionVersion": definition_version,
            "paintBlobId": paint_blob_id,
            "position": position,
        });
        entities.push(entity.clone());
        new_revision = new_revision
            .checked_add(1)
            .ok_or(SimulationError::InvalidScene)?;
        events.push((
            new_revision,
            json!({"type":"entity_published", "entity":entity}),
        ));
    }
    let tick = i64::try_from(candidate.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    let updated = sqlx::query(
        "UPDATE scenes SET state = jsonb_set(jsonb_set(state, '{simulation}', $1::jsonb, true), \
         '{entities}', $2::jsonb, true), simulation_tick = $3, revision = $4, updated_at = now() \
         WHERE id = $5 AND scene_epoch = $6 AND simulation_tick = $7",
    )
    .bind(serde_json::to_value(candidate.checkpoint())?)
    .bind(json!(entities))
    .bind(tick)
    .bind(new_revision)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    for (revision, event) in events {
        sqlx::query(
            "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
        )
        .bind(scene_id)
        .bind(revision)
        .bind(scene.epoch)
        .bind(event)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM fish_publications WHERE scene_id = $1")
        .bind(scene_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    scene.world = candidate;
    scene.persisted_tick = tick;
    scene.revision = new_revision;
    Ok(true)
}

/// Structural fish changes use the same scene-row lock and checkpoint boundary
/// as publication. The inbox row disappears only with the committed projection
/// and event, so a process restart cannot apply the request twice.
pub(crate) async fn apply_pending_fish_mutations(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let mut tx = pool.begin().await?;
    let row: Option<(i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.scene_epoch, c.revision, c.simulation_tick, c.state, s.status, s.active_scene_id \
         FROM scenes c JOIN sessions s ON s.id = c.session_id WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((epoch, revision, persisted_tick, mut state, status, active)) = row else {
        return Ok(false);
    };
    if epoch != scene.epoch
        || persisted_tick != scene.persisted_tick
        || !matches!(status.as_str(), "running" | "paused")
        || active != Some(scene_id)
    {
        return Ok(false);
    }
    let pending: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT fish_id, command_id, operation FROM fish_mutations \
         WHERE scene_id = $1 ORDER BY accepted_at, command_id LIMIT 100",
    )
    .bind(scene_id)
    .fetch_all(&mut *tx)
    .await?;
    if pending.is_empty() {
        return Ok(true);
    }
    let mut candidate = scene.world.clone();
    let mut entities = state
        .get("entities")
        .and_then(Value::as_array)
        .cloned()
        .ok_or(SimulationError::InvalidScene)?;
    if entities.len() != candidate.fish().len() {
        return Err(SimulationError::InvalidScene);
    }
    let mut new_revision = revision;
    let mut events = Vec::with_capacity(pending.len());
    for (fish_id, command_id, operation) in &pending {
        let id = format!("fish-{:032x}", fish_id.as_u128());
        let event = match operation.as_str() {
            "delete" => {
                let index = entities
                    .iter()
                    .position(|entity| entity.get("id").and_then(Value::as_str) == Some(&id))
                    .ok_or(SimulationError::InvalidScene)?;
                let removed = candidate
                    .remove_fish(fish_id.as_u128())
                    .map_err(|_| SimulationError::InvalidScene)?;
                let mut entity = entities.remove(index);
                entity["position"] = serde_json::to_value(removed.position)?;
                let blob_id = entity
                    .get("paintBlobId")
                    .and_then(Value::as_str)
                    .filter(|value| valid_paint_blob_id(value))
                    .ok_or(SimulationError::InvalidScene)?;
                sqlx::query(
                    "INSERT INTO fish_trash \
                     (scene_id, fish_id, entity, fish_state, paint_blob_id) VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(scene_id)
                .bind(fish_id)
                .bind(&entity)
                .bind(serde_json::to_value(removed)?)
                .bind(blob_id)
                .execute(&mut *tx)
                .await?;
                json!({"type":"entity_removed", "entityId":id, "commandId":command_id})
            }
            "restore" => {
                let row: Option<(Value, Value)> = sqlx::query_as(
                    "SELECT entity, fish_state FROM fish_trash WHERE scene_id = $1 AND fish_id = $2 FOR UPDATE",
                )
                .bind(scene_id)
                .bind(fish_id)
                .fetch_optional(&mut *tx)
                .await?;
                let (entity, fish_state) = row.ok_or(SimulationError::InvalidScene)?;
                let fish: Fish = serde_json::from_value(fish_state)?;
                if fish.id != fish_id.as_u128()
                    || entity.get("id").and_then(Value::as_str) != Some(id.as_str())
                    || entities.iter().any(|current| {
                        current.get("id").and_then(Value::as_str) == Some(id.as_str())
                    })
                {
                    return Err(SimulationError::InvalidScene);
                }
                candidate
                    .spawn_fish_with_capabilities(
                        fish.id,
                        fish.position,
                        fish.speed,
                        fish.capabilities,
                    )
                    .map_err(|_| SimulationError::InvalidScene)?;
                entities.push(entity.clone());
                sqlx::query("DELETE FROM fish_trash WHERE scene_id = $1 AND fish_id = $2")
                    .bind(scene_id)
                    .bind(fish_id)
                    .execute(&mut *tx)
                    .await?;
                json!({"type":"entity_restored", "entity":entity, "commandId":command_id})
            }
            _ => return Err(SimulationError::InvalidScene),
        };
        new_revision = new_revision
            .checked_add(1)
            .ok_or(SimulationError::InvalidScene)?;
        events.push((new_revision, event));
    }
    let tick = i64::try_from(candidate.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    let object = state.as_object_mut().ok_or(SimulationError::InvalidScene)?;
    object.insert("simulation".into(), serde_json::to_value(candidate.checkpoint())?);
    object.insert("entities".into(), json!(entities));
    let updated = sqlx::query(
        "UPDATE scenes SET state = $1::jsonb, simulation_tick = $2, revision = $3, updated_at = now() \
         WHERE id = $4 AND scene_epoch = $5 AND simulation_tick = $6",
    )
    .bind(state)
    .bind(tick)
    .bind(new_revision)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    for (revision, event) in events {
        sqlx::query(
            "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
        )
        .bind(scene_id)
        .bind(revision)
        .bind(scene.epoch)
        .bind(event)
        .execute(&mut *tx)
        .await?;
    }
    for (fish_id, _, _) in pending {
        sqlx::query("DELETE FROM fish_mutations WHERE scene_id = $1 AND fish_id = $2")
            .bind(scene_id)
            .bind(fish_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    scene.world = candidate;
    scene.persisted_tick = tick;
    scene.revision = new_revision;
    Ok(true)
}

/// Accepted commands remain in the scene until the worker commits their effect.
/// The checkpoint, projection, queue removal and revisioned event are atomic.
async fn apply_pending_interactions(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, i32, i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.scene_epoch, c.revision, c.simulation_tick, \
         c.state, s.status, s.active_scene_id FROM scenes c JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((world_id, world_version, epoch, revision, persisted_tick, mut state, status, active)) =
        row
    else {
        return Ok(false);
    };
    if world_id != "underwater"
        || world_version != 1
        || epoch != scene.epoch
        || persisted_tick != scene.persisted_tick
        || status != "running"
        || active != Some(scene_id)
    {
        return Ok(false);
    }
    let pending = state
        .get("pendingInteractions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut candidate = scene.world.clone();
    let mut remaining = Vec::new();
    let mut applied = Vec::new();
    for entry in pending {
        if entry.get("type").and_then(Value::as_str) != Some("interaction_requested") {
            remaining.push(entry);
            continue;
        }
        let interaction = entry.get("interactionId").and_then(Value::as_str);
        if !interaction.is_some_and(|id| {
            matches!(id, "cancel_feed" | "cancel_boat") || candidate.action_rule(id).is_some()
        }) {
            remaining.push(entry);
            continue;
        }
        let id = entry
            .get("commandId")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or(SimulationError::InvalidScene)?;
        if matches!(interaction, Some("cancel_feed" | "cancel_boat")) {
            let prefix = if interaction == Some("cancel_feed") {
                "feed-"
            } else {
                "boat-"
            };
            let target = entry
                .get("targetActionId")
                .and_then(Value::as_str)
                .and_then(|value| value.strip_prefix(prefix))
                .ok_or(SimulationError::InvalidScene)?;
            if interaction == Some("cancel_feed") {
                candidate.cancel_feed(target);
            } else {
                candidate.cancel_boat(target);
            }
        } else {
            let point: Point = serde_json::from_value(
                entry
                    .get("point")
                    .cloned()
                    .ok_or(SimulationError::InvalidScene)?,
            )?;
            candidate
                .start_action(
                    interaction.ok_or(SimulationError::InvalidScene)?,
                    &id.simple().to_string(),
                    point,
                )
                .map_err(|_| SimulationError::InvalidScene)?;
        }
        applied.push(id);
    }
    if applied.is_empty() {
        return Ok(true);
    }
    let tick = i64::try_from(candidate.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    let new_revision = revision
        .checked_add(1)
        .ok_or(SimulationError::InvalidScene)?;
    let event = interaction_state_event(&candidate, &applied);
    let object = state.as_object_mut().ok_or(SimulationError::InvalidScene)?;
    object.insert(
        "simulation".into(),
        serde_json::to_value(candidate.checkpoint())?,
    );
    object.insert("pendingInteractions".into(), json!(remaining));
    object.insert("activeActions".into(), active_actions(&candidate));
    let updated = sqlx::query(
        "UPDATE scenes SET state = $1::jsonb, simulation_tick = $2, revision = $3, updated_at = now() \
         WHERE id = $4 AND scene_epoch = $5 AND simulation_tick = $6",
    )
    .bind(state)
    .bind(tick)
    .bind(new_revision)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(scene.epoch)
    .bind(event)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    scene.world = candidate;
    scene.persisted_tick = tick;
    scene.revision = new_revision;
    Ok(true)
}

/// Resource consumption and expiry are durable transitions. They get one
/// checkpoint and one revision, so a restart cannot replay an eaten portion.
async fn save_interaction_state(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let mut tx = pool.begin().await?;
    let row: Option<(i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.scene_epoch, c.revision, c.simulation_tick, c.state, s.status, s.active_scene_id \
         FROM scenes c JOIN sessions s ON s.id = c.session_id WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((epoch, revision, persisted_tick, mut state, status, active)) = row else {
        return Ok(false);
    };
    if epoch != scene.epoch
        || persisted_tick != scene.persisted_tick
        || status != "running"
        || active != Some(scene_id)
    {
        return Ok(false);
    }
    let tick =
        i64::try_from(scene.world.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    let new_revision = revision
        .checked_add(1)
        .ok_or(SimulationError::InvalidScene)?;
    let event = interaction_state_event(&scene.world, &[]);
    let object = state.as_object_mut().ok_or(SimulationError::InvalidScene)?;
    object.insert(
        "simulation".into(),
        serde_json::to_value(scene.world.checkpoint())?,
    );
    object.insert("activeActions".into(), active_actions(&scene.world));
    let updated = sqlx::query(
        "UPDATE scenes SET state = $1::jsonb, simulation_tick = $2, revision = $3, updated_at = now() \
         WHERE id = $4 AND scene_epoch = $5 AND simulation_tick = $6",
    )
    .bind(state)
    .bind(tick)
    .bind(new_revision)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(scene.epoch)
    .bind(event)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    scene.persisted_tick = tick;
    scene.revision = new_revision;
    Ok(true)
}

/// Save only after a complete 100-tick (5-second) interval or controlled stop.
/// Epoch and previous tick make an older worker unable to overwrite a newer run.
pub async fn save_checkpoint(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let tick =
        i64::try_from(scene.world.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    if tick <= scene.persisted_tick {
        return Err(SimulationError::InvalidScene);
    }
    let checkpoint = serde_json::to_value(scene.world.checkpoint())?;
    let saved = sqlx::query(
        "UPDATE scenes SET state = jsonb_set(state, '{simulation}', $1::jsonb, true), \
         simulation_tick = $2, updated_at = now() \
         WHERE id = $3 AND scene_epoch = $4 AND simulation_tick = $5",
    )
    .bind(checkpoint)
    .bind(tick)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(pool)
    .await?;
    if saved.rows_affected() == 1 {
        scene.persisted_tick = tick;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Run active scenes with a checkpoint. A newly created empty scene has no work
/// until a fish is published with its first checkpoint by the creation flow.
pub async fn run(
    pool: PgPool,
    mut shutdown: watch::Receiver<bool>,
    hub: SimulationHub,
) -> Result<(), SimulationError> {
    let mut scan = interval(Duration::from_secs(1));
    scan.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut workers = JoinSet::new();
    let mut active = HashSet::new();
    loop {
        tokio::select! {
            _ = scan.tick() => {
                while let Some(result) = workers.try_join_next() {
                    if let Ok((scene_id, outcome)) = result {
                        active.remove(&scene_id);
                        if let Err(error) = outcome {
                            eprintln!("simulation scene {scene_id}: {error}");
                        }
                    }
                }
                let scenes: Vec<Uuid> = match sqlx::query_scalar(
                    "SELECT c.id FROM scenes c JOIN sessions s ON s.active_scene_id = c.id \
                     WHERE (s.status = 'running' AND c.state ? 'simulation') \
                        OR (s.status = 'paused' AND EXISTS \
                            (SELECT 1 FROM fish_mutations m WHERE m.scene_id = c.id)) \
                     ORDER BY c.id LIMIT 10",
                )
                .fetch_all(&pool)
                .await {
                    Ok(scenes) => scenes,
                    Err(error) => {
                        eprintln!("simulation scan: {error}");
                        continue;
                    }
                };
                for scene_id in scenes {
                    if active.insert(scene_id) {
                        let pool = pool.clone();
                        let shutdown = shutdown.clone();
                        let hub = hub.clone();
                        workers.spawn(async move {
                            (scene_id, run_scene(pool, scene_id, shutdown, hub, Duration::from_millis(50)).await)
                        });
                    }
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
            }
        }
    }
    while let Some(result) = workers.join_next().await {
        if let Ok((scene_id, outcome)) = result {
            if let Err(error) = outcome {
                eprintln!("simulation scene {scene_id}: {error}");
            }
        }
    }
    Ok(())
}

async fn run_scene(
    pool: PgPool,
    scene_id: Uuid,
    mut shutdown: watch::Receiver<bool>,
    hub: SimulationHub,
    period: Duration,
) -> Result<(), SimulationError> {
    let mut scene = load_scene(&pool, scene_id).await?;
    let mut changes = hub.subscribe_changes();
    let mut pending_wakeup = true;
    let mut timer = interval(period);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = timer.tick() => {
                if scene.world.tick_number() % 20 == 0 || pending_wakeup {
                    pending_wakeup = false;
                    let status: Option<String> = sqlx::query_scalar(
                        "SELECT s.status FROM scenes c JOIN sessions s \
                         ON s.active_scene_id = c.id WHERE c.id = $1 AND c.scene_epoch = $2",
                    )
                    .bind(scene_id)
                    .bind(scene.epoch)
                    .fetch_optional(&pool)
                    .await?;
                    if !matches!(status.as_deref(), Some("running" | "paused")) { break; }
                    let prior_revision = scene.revision;
                    if !apply_pending_fish_mutations(&pool, scene_id, &mut scene).await? {
                        return Ok(());
                    }
                    if scene.revision != prior_revision {
                        hub.notify_change(scene_id);
                    }
                    if status.as_deref() == Some("paused") { break; }
                    if !apply_pending_fish(&pool, scene_id, &mut scene).await? {
                        return Ok(());
                    }
                    let prior_revision = scene.revision;
                    if !apply_pending_interactions(&pool, scene_id, &mut scene).await? {
                        return Ok(());
                    }
                    if scene.revision != prior_revision {
                        hub.notify_change(scene_id);
                    }
                }
                let food_before = scene.world.feed_sources().to_vec();
                let boat_before = scene.world.boat().is_some();
                scene.world.step();
                if (scene.world.feed_sources() != food_before
                    || scene.world.boat().is_some() != boat_before)
                    && !save_interaction_state(&pool, scene_id, &mut scene).await?
                {
                    return Ok(());
                }
                if scene.world.feed_sources() != food_before
                    || scene.world.boat().is_some() != boat_before {
                    hub.notify_change(scene_id);
                }
                if scene.world.tick_number() % 10 == 0 {
                    hub.publish(position_frame(scene_id, &scene));
                }
                if scene.world.tick_number() % 100 == 0
                    && scene.world.tick_number() > scene.persisted_tick as u64
                    && !save_checkpoint(&pool, scene_id, &mut scene).await?
                {
                    return Ok(());
                }
            }
            changed = changes.recv() => {
                match changed {
                    Ok(changed_scene) if changed_scene == scene_id => pending_wakeup = true,
                    Err(broadcast::error::RecvError::Lagged(_)) => pending_wakeup = true,
                    _ => {}
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
            }
        }
    }
    if scene.world.tick_number() > scene.persisted_tick as u64 {
        let _ = save_checkpoint(&pool, scene_id, &mut scene).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        access::{AccessStore, GrantKind},
        blob_store::BlobStore,
        http::{self, AppState},
        realtime::{self, InteractionCommand},
        upload::{self, UploadIntentRequest},
    };
    use futures_util::{SinkExt, StreamExt};
    use ldw_sim::Point;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use tokio::time::{sleep, timeout};
    use tokio_tungstenite::tungstenite::{Message as ClientMessage, client::IntoClientRequest};

    const INITIAL_LOAD_FISH: usize = ldw_sim::MAX_FISH - 2;

    #[test]
    fn new_underwater_scene_uses_versioned_interaction_definitions() {
        let world = initial_world(Uuid::new_v4()).unwrap();
        let rules = world.interaction_rules();
        assert_eq!(rules.feed.radius, 4.0);
        assert_eq!(rules.feed.definition_version, 2);
        assert_eq!(rules.feed.duration_ticks, 300);
        assert_eq!(rules.feed.max_active, 3);
        assert_eq!(rules.feed.cooldown_ticks, 10);
        assert_eq!(rules.boat.radius, 3.2);
        assert_eq!(rules.boat.duration_ticks, 600);
        assert_eq!(rules.boat.cooldown_ticks, 200);
        assert!(rules.boat.priority > rules.feed.priority);
        assert_eq!(rules.feed_behavior, FeedBehavior::default());
        assert_eq!(rules.boat_behavior, BoatBehavior::default());
        assert_eq!(world.action_definitions().len(), 2);
        assert_eq!(world.action_definitions()[0].label, "Корм");
        assert_eq!(
            World::restore(world.checkpoint())
                .unwrap()
                .interaction_rules(),
            rules
        );
    }

    #[test]
    fn package_adds_same_effect_action_without_new_parser_branch() {
        let mut package: Value = serde_json::from_str(include_str!(
            "../../../content/underwater/interactions.json"
        ))
        .unwrap();
        let mut extra = package[0].clone();
        extra["id"] = json!("feed-slow");
        extra["label"] = json!("Медленный корм");
        extra["radius"] = json!(2.0);
        extra["durationTicks"] = json!(40);
        extra["behavior"][0]["maxCandidates"] = json!(1);
        extra["behavior"][2]["depth"] = json!(-0.8);
        package.as_array_mut().unwrap().push(extra);
        let parsed = parse_interaction_package(&package.to_string()).unwrap();
        assert_eq!(parsed.catalog.len(), 3);
        let mut world = World::new_with_catalog(
            underwater_bounds().unwrap(),
            9,
            parsed.rules,
            parsed.catalog,
        )
        .unwrap();
        world.spawn_fish(1, Point { x: -0.5, y: 0.0 }, 1.0).unwrap();
        world.spawn_fish(2, Point { x: 0.5, y: 0.0 }, 1.0).unwrap();
        world
            .start_action(
                "feed-slow",
                "00000000000000000000000000000001",
                Point { x: 0.0, y: 0.0 },
            )
            .unwrap();
        world.step();
        assert_eq!(world.feed_sources()[0].interaction_id, "feed-slow");
        assert_eq!(
            world
                .fish()
                .iter()
                .filter(|fish| fish.feeding.is_some())
                .count(),
            1
        );
        assert_eq!(
            world
                .fish()
                .iter()
                .find(|fish| fish.feeding.is_some())
                .unwrap()
                .depth_target,
            -0.8
        );
        assert_eq!(
            World::restore(world.checkpoint())
                .unwrap()
                .action_definitions()[2]
                .id,
            "feed-slow"
        );
        let original = package.clone();
        package[2].as_object_mut().unwrap().remove("label");
        assert!(parse_interaction_package(&package.to_string()).is_err());
        package = original.clone();
        package[2]["id"] = json!("feed");
        assert!(parse_interaction_package(&package.to_string()).is_err());
        package = original.clone();
        package[2]["requiredCapability"] = json!("avoid-threat");
        assert!(parse_interaction_package(&package.to_string()).is_err());
        package = original;
        package[2]["priority"] = json!(10);
        assert!(parse_interaction_package(&package.to_string()).is_err());
    }

    #[tokio::test]
    async fn durable_queue_preserves_extra_action_id_through_start_and_cancel() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        let command_id = Uuid::new_v4();
        let mut package: Value = serde_json::from_str(include_str!(
            "../../../content/underwater/interactions.json"
        ))
        .unwrap();
        let mut extra = package[0].clone();
        extra["id"] = json!("feed-slow");
        extra["label"] = json!("Медленный корм");
        extra["behavior"][0]["maxCandidates"] = json!(1);
        extra["behavior"][2]["depth"] = json!(-0.8);
        package.as_array_mut().unwrap().push(extra);
        let parsed = parse_interaction_package(&package.to_string()).unwrap();
        let mut world = World::new_with_catalog(
            underwater_bounds().unwrap(),
            9,
            parsed.rules,
            parsed.catalog,
        )
        .unwrap();
        world.spawn_fish(1, Point { x: -0.5, y: 0.0 }, 1.0).unwrap();
        let state = json!({
            "simulation":world.checkpoint(), "entities":[], "activeActions":[],
            "pendingInteractions":[{"type":"interaction_requested",
                "commandId":command_id,"interactionId":"feed-slow",
                "point":{"x":0.0,"y":0.0}}],
            "resources":{}, "reservations":[],
        });
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) \
                     VALUES ($1, $2, 'owner', 'test-hash')",
        )
        .bind(owner)
        .bind(format!("catalog-{owner}"))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scenes (id, session_id, world_id, world_version, state) \
                     VALUES ($1, $2, 'underwater', 1, $3::jsonb)",
        )
        .bind(scene_id)
        .bind(session)
        .bind(state)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let mut scene = load_scene(&pool, scene_id).await.unwrap();
        assert!(
            apply_pending_interactions(&pool, scene_id, &mut scene)
                .await
                .unwrap()
        );
        assert_eq!(scene.world.feed_sources()[0].interaction_id, "feed-slow");
        let stored: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored["activeActions"][0]["interactionId"], "feed-slow");
        assert_eq!(stored["pendingInteractions"], json!([]));
        let mut restored = load_scene(&pool, scene_id).await.unwrap();
        let mut response = restored.world.clone();
        response.step();
        assert_eq!(response.fish()[0].depth_target, -0.8);
        let cancel_id = Uuid::new_v4();
        let target = format!("feed-{}", command_id.simple());
        let mut state = stored;
        state["pendingInteractions"] = json!([{"type":"interaction_requested",
            "commandId":cancel_id,"interactionId":"cancel_feed",
            "point":{"x":0.0,"y":0.0},"targetActionId":target}]);
        sqlx::query("UPDATE scenes SET state = $1::jsonb WHERE id = $2")
            .bind(state)
            .bind(scene_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            apply_pending_interactions(&pool, scene_id, &mut restored)
                .await
                .unwrap()
        );
        assert!(restored.world.feed_sources().is_empty());
        let after_cancel = load_scene(&pool, scene_id).await.unwrap();
        assert!(after_cancel.world.feed_sources().is_empty());
        assert_eq!(after_cancel.world.action_definitions().len(), 3);
    }

    #[test]
    fn versioned_package_can_change_effect_parameters_without_server_code() {
        let mut package: Value = serde_json::from_str(include_str!(
            "../../../content/underwater/interactions.json"
        ))
        .unwrap();
        package[0]["version"] = json!(3);
        package[0]["radius"] = json!(4.5);
        package[0]["durationTicks"] = json!(120);
        package[0]["behavior"][0]["maxCandidates"] = json!(1);
        package[0]["behavior"][2]["depth"] = json!(-0.5);
        let rules = parse_interaction_rules(&package.to_string()).unwrap();
        let world = World::new_with_rules(underwater_bounds().unwrap(), 1, rules).unwrap();
        assert_eq!(world.interaction_rules().feed.definition_version, 3);
        assert_eq!(world.interaction_rules().feed.radius, 4.5);
        assert_eq!(world.interaction_rules().feed.duration_ticks, 120);
        assert_eq!(rules.feed_behavior.candidate_limit, 1);
        let mut world = world;
        world.spawn_fish(1, Point { x: -1.0, y: 0.0 }, 1.0).unwrap();
        world.spawn_fish(2, Point { x: 2.0, y: 0.0 }, 1.0).unwrap();
        world
            .start_feed("00000000000000000000000000000001", Point { x: 0.0, y: 0.0 })
            .unwrap();
        world.step();
        assert!(world.fish()[0].feeding.is_some());
        assert_eq!(world.fish()[0].depth_target, -0.5);
        assert!(world.fish()[1].feeding.is_none());
        package[0]["effect"] = json!("threat");
        assert!(parse_interaction_rules(&package.to_string()).is_err());
    }

    #[test]
    fn behavior_v2_rejects_unsupported_or_unbounded_chains_and_accepts_v1() {
        let mut package: Value = serde_json::from_str(include_str!(
            "../../../content/underwater/interactions.json"
        ))
        .unwrap();
        let original = package.clone();
        package[0]["behavior"][1]["primitive"] = json!("flee");
        assert!(parse_interaction_rules(&package.to_string()).is_err());
        package = original.clone();
        package[0]["behavior"][0]["maxCandidates"] = json!(101);
        assert!(parse_interaction_rules(&package.to_string()).is_err());
        package = original.clone();
        package[1]["behavior"][1]["holdTicks"] = json!(121);
        assert!(parse_interaction_rules(&package.to_string()).is_err());
        package = original.clone();
        package[0].as_object_mut().unwrap().remove("behavior");
        assert!(parse_interaction_rules(&package.to_string()).is_err());
        package = original.clone();
        for definition in package.as_array_mut().unwrap() {
            definition["schemaVersion"] = json!(1);
            definition["version"] = json!(1);
            definition.as_object_mut().unwrap().remove("behavior");
            definition.as_object_mut().unwrap().remove("label");
        }
        let legacy = parse_interaction_rules(&package.to_string()).unwrap();
        assert_eq!(legacy.feed_behavior, FeedBehavior::default());
        assert_eq!(legacy.boat_behavior, BoatBehavior::default());
        package[0]["behavior"] = original[0]["behavior"].clone();
        assert!(parse_interaction_rules(&package.to_string()).is_err());
    }

    #[test]
    fn entity_capabilities_are_validated_from_the_package() {
        let definitions = underwater_entity_definitions().unwrap();
        assert!(definitions["coral-fish"].capabilities.consume_food);
        assert!(definitions["stream-fish"].capabilities.avoid_threat);
        let mut package: Value =
            serde_json::from_str(include_str!("../../../content/underwater/entities.json"))
                .unwrap();
        package[1]["capabilities"] = json!(["avoid-threat"]);
        let changed = parse_entity_definitions(&package.to_string()).unwrap();
        assert!(!changed["stream-fish"].capabilities.consume_food);
        assert!(changed["stream-fish"].capabilities.avoid_threat);
        package[1]["capabilities"] = json!(["avoid-threat", "avoid-threat"]);
        assert!(parse_entity_definitions(&package.to_string()).is_err());
        package[1]["capabilities"] = json!(["arbitrary-code"]);
        assert!(parse_entity_definitions(&package.to_string()).is_err());
    }

    fn load_paint_png(value: u8) -> Vec<u8> {
        let mut raw = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut raw, 512, 512);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let mut pixels = Vec::with_capacity(512 * 512 * 4);
            for _ in 0..512 * 512 {
                pixels.extend_from_slice(&[value, 90, 255 - value, 255]);
            }
            writer.write_image_data(&pixels).unwrap();
        }
        crate::paint_image::normalize_png(&raw).unwrap()
    }

    #[tokio::test]
    #[ignore = "run against a fresh PostgreSQL database to measure three runners, 30 Controllers, 3 Viewers and six uploads"]
    async fn three_hundred_fish_progress_across_three_real_time_scenes() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) \
             VALUES ($1, $2, 'owner', 'test-hash')",
        )
        .bind(owner)
        .bind(format!("three-scenes-{owner}"))
        .execute(&pool)
        .await
        .unwrap();
        let owner_token = format!("load-owner-{}", Uuid::new_v4());
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
             VALUES ($1, $2, $3, $4, now() + interval '1 hour')",
        )
        .bind(Uuid::new_v4())
        .bind(owner)
        .bind(crate::access::hash_token(&owner_token).to_vec())
        .bind(crate::access::hash_token("load-owner-csrf").to_vec())
        .execute(&pool)
        .await
        .unwrap();
        let mut scenes = Vec::new();
        for scene_number in 0..3 {
            let session = Uuid::new_v4();
            let scene_id = Uuid::new_v4();
            sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
                .bind(session)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO scenes (id, session_id, world_id, world_version) \
                 VALUES ($1, $2, 'underwater', 1)",
            )
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
                .bind(scene_id)
                .bind(session)
                .execute(&pool)
                .await
                .unwrap();

            let mut world = initial_world(scene_id).unwrap();
            let mut entities = Vec::new();
            for fish in 0..INITIAL_LOAD_FISH {
                let fish_id = Uuid::new_v4();
                let position = Point {
                    x: -6.3 + (fish % 10) as f32 * 1.4,
                    y: -3.3 + (fish / 10) as f32 * 0.73,
                };
                world.spawn_fish(fish_id.as_u128(), position, 1.2).unwrap();
                entities.push(json!({
                    "id": format!("fish-{:032x}", fish_id.as_u128()),
                    "definitionId": if fish % 2 == 0 { "coral-fish" } else { "stream-fish" },
                    "definitionVersion": 1,
                    "paintBlobId": "paint-load-fixture",
                    "position": position,
                }));
            }
            let state = json!({
                "entities": entities,
                "simulation": world.checkpoint(),
                "activeActions": [],
                "pendingInteractions": [],
            });
            sqlx::query("UPDATE scenes SET state = $1::jsonb WHERE id = $2")
                .bind(state)
                .bind(scene_id)
                .execute(&pool)
                .await
                .unwrap();
            scenes.push((scene_number, session, scene_id));
        }

        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut frames = hub.subscribe();
        let store = AccessStore::new(pool.clone(), [7u8; 32]);
        let blob_root = std::env::temp_dir().join(format!("ldw-load-blobs-{}", Uuid::new_v4()));
        let blob_store = BlobStore::create(blob_root).unwrap();
        let app = http::router(AppState {
            access: store.clone(),
            blob_store: blob_store.clone(),
            public_origin: Arc::from("https://world.example.test"),
            simulation_hub: hub.clone(),
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
        let mut controllers = tokio::task::JoinSet::new();
        let mut viewers = tokio::task::JoinSet::new();
        let client_start = Arc::new(tokio::sync::Barrier::new(34));
        let command_times = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            Uuid,
            tokio::time::Instant,
        >::new()));
        for &(scene_number, session_id, scene_id) in &scenes {
            let feed_command_id = Uuid::new_v4();
            let boat_command_id = Uuid::new_v4();
            let url = format!("ws://{address}/api/sessions/{session_id}/ws");
            for controller_number in 0..10 {
                let token = format!("load-controller-{}", Uuid::new_v4());
                let csrf = format!("load-csrf-{}", Uuid::new_v4());
                sqlx::query(
                    "INSERT INTO device_grants (id, session_id, participant_id, role, \
                     token_hash, csrf_hash, expires_at, last_heartbeat_at) \
                     VALUES ($1, $2, $3, 'controller', $4, $5, now() + interval '1 hour', now())",
                )
                .bind(Uuid::new_v4())
                .bind(session_id)
                .bind(Uuid::new_v4())
                .bind(crate::access::hash_token(&token).to_vec())
                .bind(crate::access::hash_token(&csrf).to_vec())
                .execute(&pool)
                .await
                .unwrap();
                let mut request = url.as_str().into_client_request().unwrap();
                request
                    .headers_mut()
                    .insert("origin", "https://world.example.test".parse().unwrap());
                request.headers_mut().insert(
                    "cookie",
                    format!("__Host-ldw-controller={token}").parse().unwrap(),
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
                        json!({"type":"hello","csrf":csrf.clone()}).to_string(),
                    ))
                    .await
                    .unwrap();
                let snapshot: Value = serde_json::from_str(
                    timeout(Duration::from_secs(3), socket.next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap()
                        .to_text()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(snapshot["type"], "snapshot");
                assert_eq!(snapshot["sceneId"], scene_id.to_string());
                assert_eq!(
                    snapshot["entities"].as_array().unwrap().len(),
                    INITIAL_LOAD_FISH
                );
                let client_start = client_start.clone();
                let command_times = command_times.clone();
                let reconnect_url = url.clone();
                controllers.spawn(async move {
                    client_start.wait().await;
                    let mut count = 0;
                    let mut last_tick = 0;
                    let mut accepted = 0;
                    let mut reconnected = false;
                    let mut saw_full_scene = false;
                    let mut published = std::collections::HashSet::new();
                    let mut action_latencies = std::collections::HashMap::new();
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
                    let reconnect_at = tokio::time::Instant::now() + Duration::from_secs(6);
                    if controller_number == 0 {
                        sleep(Duration::from_secs(1)).await;
                        for (command_id, interaction_id, point) in [
                            (feed_command_id, "feed", realtime::Point { x: 0.0, y: 0.0 }),
                            (boat_command_id, "boat", realtime::Point { x: 1.0, y: 2.0 }),
                        ] {
                            let command = InteractionCommand {
                                kind: "command".into(),
                                command_id,
                                session_id,
                                scene_id,
                                scene_epoch: 1,
                                interaction_id: interaction_id.into(),
                                point,
                                target_action_id: None,
                                expires_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as i64 + 8_000,
                            };
                            command_times.lock().unwrap().insert(command_id, tokio::time::Instant::now());
                            socket.send(ClientMessage::text(serde_json::to_string(&command).unwrap()))
                                .await.unwrap();
                        }
                    }
                    while let Some(remaining) =
                        deadline.checked_duration_since(tokio::time::Instant::now())
                    {
                        if controller_number == 9
                            && !reconnected
                            && tokio::time::Instant::now() >= reconnect_at
                        {
                            assert_eq!(action_latencies.len(), 2);
                            socket.close(None).await.unwrap();
                            sleep(Duration::from_millis(300)).await;
                            let mut request = reconnect_url.as_str().into_client_request().unwrap();
                            request.headers_mut().insert("origin", "https://world.example.test".parse().unwrap());
                            request.headers_mut().insert(
                                "cookie",
                                format!("__Host-ldw-controller={token}").parse().unwrap(),
                            );
                            let (replacement, _) = timeout(
                                Duration::from_secs(3),
                                tokio_tungstenite::connect_async(request),
                            ).await.unwrap().unwrap();
                            socket = replacement;
                            socket.send(ClientMessage::text(
                                json!({"type":"hello","csrf":csrf}).to_string(),
                            )).await.unwrap();
                            let snapshot: Value = serde_json::from_str(
                                timeout(Duration::from_secs(3), socket.next())
                                    .await.unwrap().unwrap().unwrap().to_text().unwrap(),
                            ).unwrap();
                            assert_eq!(snapshot["type"], "snapshot");
                            assert_eq!(snapshot["sceneId"], scene_id.to_string());
                            assert_eq!(snapshot["sceneEpoch"], 1);
                            assert!(snapshot["revision"].as_i64().unwrap() >= 3);
                            let fish_count = snapshot["entities"].as_array().unwrap().len();
                            assert!((INITIAL_LOAD_FISH..=ldw_sim::MAX_FISH).contains(&fish_count));
                            saw_full_scene |= fish_count == ldw_sim::MAX_FISH;
                            assert!(snapshot["pendingInteractions"].as_array().unwrap().is_empty());
                            let actions = snapshot["activeActions"].as_array().unwrap();
                            assert!(actions.iter().any(|action| action["interactionId"] == "boat"));
                            last_tick = snapshot["simulationTick"].as_u64().unwrap();
                            reconnected = true;
                            continue;
                        }
                        let message = match timeout(remaining, socket.next()).await {
                            Ok(Some(Ok(message))) => message,
                            Ok(other) => panic!(
                                "scene {scene_number} Controller {controller_number} closed: {other:?}"
                            ),
                            Err(_) => break,
                        };
                        match message {
                            ClientMessage::Text(text) => {
                                let value: Value = serde_json::from_str(&text).unwrap();
                                if value["type"] == "positions" {
                                    assert_eq!(value["sceneId"], scene_id.to_string());
                                    let fish_count = value["positions"].as_array().unwrap().len();
                                    assert!((INITIAL_LOAD_FISH..=ldw_sim::MAX_FISH).contains(&fish_count));
                                    saw_full_scene |= fish_count == ldw_sim::MAX_FISH;
                                    let tick = value["simulationTick"].as_u64().unwrap();
                                    assert!(tick > last_tick);
                                    last_tick = tick;
                                    count += 1;
                                } else if value["type"] == "ack" && controller_number == 0 {
                                    assert_eq!(value["accepted"], true, "{value}");
                                    accepted += 1;
                                } else if value["type"] == "delta" {
                                    assert_eq!(value["sceneId"], scene_id.to_string());
                                    if value["event"]["type"] == "entity_published" {
                                        published.insert(value["event"]["entity"]["id"].as_str().unwrap().to_owned());
                                    }
                                    if let Some(applied) = value["event"]["appliedCommandIds"].as_array() {
                                        for command_id in [feed_command_id, boat_command_id] {
                                            if applied.iter().any(|id| id == &command_id.to_string()) {
                                                let sent = command_times.lock().unwrap()[&command_id];
                                                action_latencies.entry(command_id).or_insert(sent.elapsed());
                                            }
                                        }
                                    }
                                }
                            }
                            ClientMessage::Ping(payload) => {
                                socket.send(ClientMessage::Pong(payload)).await.unwrap();
                            }
                            ClientMessage::Close(_) => panic!(
                                "scene {scene_number} Controller {controller_number} closed early"
                            ),
                            _ => {}
                        }
                    }
                    (scene_number, controller_number, count, last_tick, accepted, reconnected,
                        saw_full_scene, published.len(), action_latencies)
                });
            }
            let viewer_token = format!("load-viewer-{}", Uuid::new_v4());
            let viewer_csrf = format!("load-viewer-csrf-{}", Uuid::new_v4());
            sqlx::query(
                "INSERT INTO device_grants (id, session_id, role, token_hash, csrf_hash, expires_at) \
                 VALUES ($1, $2, 'viewer', $3, $4, now() + interval '1 hour')",
            )
            .bind(Uuid::new_v4())
            .bind(session_id)
            .bind(crate::access::hash_token(&viewer_token).to_vec())
            .bind(crate::access::hash_token(&viewer_csrf).to_vec())
            .execute(&pool)
            .await
            .unwrap();
            let mut request = url.as_str().into_client_request().unwrap();
            request
                .headers_mut()
                .insert("origin", "https://world.example.test".parse().unwrap());
            request.headers_mut().insert(
                "cookie",
                format!("__Host-ldw-viewer={viewer_token}").parse().unwrap(),
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
                    json!({"type":"hello","csrf":viewer_csrf}).to_string(),
                ))
                .await
                .unwrap();
            let snapshot: Value = serde_json::from_str(
                timeout(Duration::from_secs(3), socket.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .to_text()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(snapshot["type"], "snapshot");
            assert_eq!(snapshot["sceneId"], scene_id.to_string());
            assert_eq!(
                snapshot["entities"].as_array().unwrap().len(),
                INITIAL_LOAD_FISH
            );
            let client_start = client_start.clone();
            viewers.spawn(async move {
                client_start.wait().await;
                let command = InteractionCommand {
                    kind: "command".into(),
                    command_id: Uuid::new_v4(),
                    session_id,
                    scene_id,
                    scene_epoch: 1,
                    interaction_id: "feed".into(),
                    point: realtime::Point { x: 0.0, y: 0.0 },
                    target_action_id: None,
                    expires_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as i64
                        + 8_000,
                };
                socket
                    .send(ClientMessage::text(
                        serde_json::to_string(&command).unwrap(),
                    ))
                    .await
                    .unwrap();
                let mut count = 0;
                let mut last_tick = 0;
                let mut read_only = false;
                let mut actions = std::collections::HashSet::new();
                let mut published = std::collections::HashSet::new();
                let mut saw_full_scene = false;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
                while let Some(remaining) =
                    deadline.checked_duration_since(tokio::time::Instant::now())
                {
                    let message = match timeout(remaining, socket.next()).await {
                        Ok(Some(Ok(message))) => message,
                        Ok(other) => panic!("scene {scene_number} Viewer closed: {other:?}"),
                        Err(_) => break,
                    };
                    match message {
                        ClientMessage::Text(text) => {
                            let value: Value = serde_json::from_str(&text).unwrap();
                            if value["type"] == "positions" {
                                assert_eq!(value["sceneId"], scene_id.to_string());
                                let fish_count = value["positions"].as_array().unwrap().len();
                                assert!(
                                    (INITIAL_LOAD_FISH..=ldw_sim::MAX_FISH).contains(&fish_count)
                                );
                                saw_full_scene |= fish_count == ldw_sim::MAX_FISH;
                                let tick = value["simulationTick"].as_u64().unwrap();
                                assert!(tick > last_tick);
                                last_tick = tick;
                                count += 1;
                            } else if value["type"] == "ack" {
                                assert_eq!(value["accepted"], false, "{value}");
                                assert_eq!(value["code"], "READ_ONLY", "{value}");
                                read_only = true;
                            } else if value["type"] == "delta" {
                                assert_eq!(value["sceneId"], scene_id.to_string());
                                if value["event"]["type"] == "entity_published" {
                                    published.insert(
                                        value["event"]["entity"]["id"].as_str().unwrap().to_owned(),
                                    );
                                }
                                if let Some(applied) =
                                    value["event"]["appliedCommandIds"].as_array()
                                {
                                    for command_id in [feed_command_id, boat_command_id] {
                                        if applied.iter().any(|id| id == &command_id.to_string()) {
                                            actions.insert(command_id);
                                        }
                                    }
                                }
                            }
                        }
                        ClientMessage::Ping(payload) => {
                            socket.send(ClientMessage::Pong(payload)).await.unwrap();
                        }
                        ClientMessage::Close(_) => {
                            panic!("scene {scene_number} Viewer closed early")
                        }
                        _ => {}
                    }
                }
                (
                    scene_number,
                    count,
                    last_tick,
                    read_only,
                    actions.len(),
                    saw_full_scene,
                    published.len(),
                )
            });
        }
        client_start.wait().await;
        let started = tokio::time::Instant::now();
        let supervisor = tokio::spawn(run(pool.clone(), receiver, hub));
        let mut uploads = tokio::task::JoinSet::new();
        for &(scene_number, session_id, _) in &scenes {
            let pool = pool.clone();
            let store = store.clone();
            let blob_store = blob_store.clone();
            let owner_token = owner_token.clone();
            uploads.spawn(async move {
                sleep(Duration::from_secs(3)).await;
                let access = store
                    .scene_access(GrantKind::Owner, &owner_token, session_id)
                    .await
                    .unwrap();
                let mut published = Vec::new();
                for (index, (template, source_kind, position)) in [
                    ("coral", "browser", Point { x: -5.5, y: 2.0 }),
                    ("stream", "paper", Point { x: 5.5, y: 2.0 }),
                ]
                .into_iter()
                .enumerate()
                {
                    let layout: Value = serde_json::from_str(match template {
                        "coral" => {
                            include_str!("../../../content/underwater/assets/coral.layout.json")
                        }
                        _ => include_str!("../../../content/underwater/assets/stream.layout.json"),
                    })
                    .unwrap();
                    let request = UploadIntentRequest {
                        request_id: Uuid::new_v4(),
                        scene_epoch: access.scene.scene_epoch,
                        definition_id: format!("{template}-fish"),
                        template_id: template.into(),
                        template_version: 1,
                        layout_hash: layout["contentHash"].as_str().unwrap().into(),
                        source_kind: source_kind.into(),
                        color_space: "sRGB".into(),
                        position,
                    };
                    let intent =
                        upload::create_upload_intent(&pool, GrantKind::Owner, &access, &request)
                            .await
                            .unwrap();
                    let png = load_paint_png(40 + scene_number as u8 * 30 + index as u8 * 10);
                    upload::store_paint(
                        &pool,
                        GrantKind::Owner,
                        &access,
                        intent.intent_id,
                        png.clone(),
                    )
                    .await
                    .unwrap();
                    let expires_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as i64
                        + 30_000;
                    let finalized = upload::finalize_upload(
                        &pool,
                        &blob_store,
                        GrantKind::Owner,
                        &access,
                        intent.intent_id,
                        expires_at,
                    )
                    .await
                    .unwrap();
                    assert_eq!(blob_store.read(&finalized.paint_blob_id).unwrap(), png);
                    published.push(finalized.fish_id);
                }
                (scene_number, published)
            });
        }
        let deadline = started + Duration::from_secs(12);
        let mut frame_counts = std::collections::HashMap::<Uuid, usize>::new();
        let mut last_ticks = std::collections::HashMap::<Uuid, u64>::new();
        while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
            match timeout(remaining, frames.recv()).await {
                Ok(Ok(frame)) => {
                    assert!(
                        (INITIAL_LOAD_FISH..=ldw_sim::MAX_FISH).contains(&frame.positions.len())
                    );
                    *frame_counts.entry(frame.scene_id).or_default() += 1;
                    last_ticks.insert(frame.scene_id, frame.simulation_tick);
                }
                Ok(Err(error)) => panic!("lost a position frame: {error}"),
                Err(_) => break,
            }
        }
        stop.send(true).unwrap();
        timeout(Duration::from_secs(5), supervisor)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        while let Some(result) = uploads.join_next().await {
            let (scene_number, published) = result.unwrap();
            assert_eq!(
                published.len(),
                2,
                "scene {scene_number} did not finalize both uploads"
            );
        }
        let mut client_frames = 0;
        let mut action_latencies = Vec::new();
        while let Some(result) = controllers.join_next().await {
            let (
                scene_number,
                controller_number,
                count,
                last_tick,
                accepted,
                reconnected,
                saw_full_scene,
                published,
                observed,
            ) = result.unwrap();
            assert!(
                count >= 18 && last_tick >= 180,
                "scene {scene_number} Controller {controller_number} received {count} frames, last tick {last_tick}"
            );
            client_frames += count;
            assert_eq!(
                observed.len(),
                2,
                "scene {scene_number} Controller {controller_number} missed an applied action"
            );
            if controller_number == 0 {
                assert_eq!(
                    accepted, 2,
                    "scene {scene_number} did not accept both commands"
                );
            }
            if controller_number == 9 {
                assert!(
                    reconnected,
                    "scene {scene_number} missed Controller reconnect"
                );
            }
            assert!(
                saw_full_scene,
                "scene {scene_number} Controller {controller_number} never saw 100 fish"
            );
            assert_eq!(
                published, 2,
                "scene {scene_number} Controller {controller_number} missed a fish publication"
            );
            action_latencies.extend(observed.into_values());
        }
        let mut viewer_frames = 0;
        while let Some(result) = viewers.join_next().await {
            let (scene_number, count, last_tick, read_only, actions, saw_full_scene, published) =
                result.unwrap();
            assert!(
                count >= 18 && last_tick >= 180,
                "scene {scene_number} Viewer received {count} frames, last tick {last_tick}"
            );
            assert!(read_only, "scene {scene_number} Viewer accepted a command");
            assert_eq!(
                actions, 2,
                "scene {scene_number} Viewer missed an applied action"
            );
            assert!(
                saw_full_scene,
                "scene {scene_number} Viewer never saw 100 fish"
            );
            assert_eq!(
                published, 2,
                "scene {scene_number} Viewer missed a fish publication"
            );
            viewer_frames += count;
        }
        server.abort();
        println!("30 Controllers received {client_frames} position frames");
        println!("3 read-only Viewers received {viewer_frames} position frames");
        action_latencies.sort_unstable();
        assert_eq!(action_latencies.len(), 60);
        let p95 = action_latencies[(action_latencies.len() * 95).div_ceil(100) - 1];
        println!(
            "60 feed/boat application deliveries: p95={p95:?}, max={:?}",
            action_latencies.last().unwrap()
        );
        assert!(
            p95 <= Duration::from_millis(500),
            "local action delivery exceeded the p95 target"
        );
        for (scene_number, _, scene_id) in scenes {
            let count = frame_counts.get(&scene_id).copied().unwrap_or_default();
            let last = last_ticks.get(&scene_id).copied().unwrap_or_default();
            let restored = load_scene(&pool, scene_id).await.unwrap();
            println!(
                "scene={scene_number} frames={count} last_frame_tick={last} persisted_tick={} fish={}",
                restored.persisted_tick,
                restored.world.fish().len()
            );
            assert!(count >= 18, "scene {scene_number} stopped sending frames");
            assert!(last >= 180, "scene {scene_number} fell behind real time");
            assert!(restored.world.tick_number() >= last);
            assert_eq!(restored.world.fish().len(), ldw_sim::MAX_FISH);
            assert!(
                restored.revision >= 5,
                "actions or uploaded fish were not applied"
            );
            let (browser, paper): (i64, i64) = sqlx::query_as(
                "SELECT count(*) FILTER (WHERE source_kind = 'browser'), \
                 count(*) FILTER (WHERE source_kind = 'paper') \
                 FROM upload_intents WHERE scene_id = $1 AND status = 'finalized'",
            )
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!((browser, paper), (1, 1));
            let references: i64 =
                sqlx::query_scalar("SELECT count(*) FROM scene_paint_blobs WHERE scene_id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(references, 2);
            let pending: i64 =
                sqlx::query_scalar("SELECT count(*) FROM fish_publications WHERE scene_id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(pending, 0);
        }
    }

    #[tokio::test]
    #[ignore = "run against a fresh database with LDW_TEST_SIMULATED_SESSION_LIMIT=3"]
    async fn simulated_session_limit_rejects_fourth_concurrent_start() {
        assert_eq!(
            std::env::var("LDW_TEST_SIMULATED_SESSION_LIMIT").as_deref(),
            Ok("3")
        );
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) \
             VALUES ($1, $2, 'owner', 'test-hash')",
        )
        .bind(owner)
        .bind(format!("capacity-{owner}"))
        .execute(&pool)
        .await
        .unwrap();
        let mut sessions = Vec::new();
        let mut scenes = Vec::new();
        for _ in 0..4 {
            let session = Uuid::new_v4();
            let scene = Uuid::new_v4();
            sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
                .bind(session)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO scenes (id, session_id, world_id, world_version) \
                 VALUES ($1, $2, 'underwater', 1)",
            )
            .bind(scene)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
                .bind(scene)
                .bind(session)
                .execute(&pool)
                .await
                .unwrap();
            sessions.push(session);
            scenes.push(scene);
        }
        let tasks = scenes
            .iter()
            .copied()
            .enumerate()
            .map(|(index, scene)| {
                let pool = pool.clone();
                tokio::spawn(async move {
                    let result = publish_first_fish(
                        &pool,
                        scene,
                        Uuid::new_v4(),
                        "coral-fish",
                        "paint-capacity",
                        Point { x: 0.0, y: 0.0 },
                    )
                    .await;
                    (index, result)
                })
            })
            .collect::<Vec<_>>();
        let mut winners = Vec::new();
        let mut loser = None;
        for task in tasks {
            let (index, result) = task.await.unwrap();
            match result {
                Ok(_) => winners.push(index),
                Err(SimulationError::SessionLimit) => {
                    assert!(loser.replace(index).is_none());
                }
                Err(error) => panic!("unexpected publication error: {error}"),
            }
        }
        assert_eq!(winners.len(), 3);
        let loser = loser.unwrap();
        let running: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sessions s JOIN scenes c ON c.id = s.active_scene_id \
             WHERE s.status = 'running' AND c.state ? 'simulation'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(running, 3);
        let loser_state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scenes[loser])
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(loser_state.get("simulation").is_none());

        let token = Uuid::new_v4().to_string();
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
             VALUES ($1, $2, $3, $3, now() + interval '1 day')",
        )
        .bind(Uuid::new_v4())
        .bind(owner)
        .bind(token_hash.to_vec())
        .execute(&pool)
        .await
        .unwrap();
        let store = AccessStore::new(pool.clone(), [5; 32]);
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 8_000;
        let command = InteractionCommand {
            kind: "command".into(),
            command_id: Uuid::new_v4(),
            session_id: sessions[loser],
            scene_id: scenes[loser],
            scene_epoch: 1,
            interaction_id: "feed".into(),
            point: realtime::Point { x: 0.0, y: 0.0 },
            target_action_id: None,
            expires_at,
        };
        let rejected =
            realtime::process_command(&store, GrantKind::Owner, &token, sessions[loser], &command)
                .await
                .unwrap();
        assert_eq!(rejected["accepted"], false);
        assert_eq!(rejected["code"], "SIMULATED_SESSION_LIMIT");
        sqlx::query("UPDATE sessions SET status = 'paused' WHERE id = $1")
            .bind(sessions[winners[0]])
            .execute(&pool)
            .await
            .unwrap();
        let replay =
            realtime::process_command(&store, GrantKind::Owner, &token, sessions[loser], &command)
                .await
                .unwrap();
        assert_eq!(replay, rejected);
        let mut next_command = command;
        next_command.command_id = Uuid::new_v4();
        let accepted = realtime::process_command(
            &store,
            GrantKind::Owner,
            &token,
            sessions[loser],
            &next_command,
        )
        .await
        .unwrap();
        assert_eq!(accepted["accepted"], true);
        let new_state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scenes[loser])
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(new_state.get("simulation").is_some());
        let running: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sessions s JOIN scenes c ON c.id = s.active_scene_id \
             WHERE s.status = 'running' AND c.state ? 'simulation'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(running, 3);
    }

    #[tokio::test]
    async fn boat_command_moves_threat_and_finishes_durably() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let grant = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        let token = Uuid::new_v4().to_string();
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("boat-{owner}")).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
                    VALUES ($1, $2, $3, $3, now() + interval '1 day')",
        )
        .bind(grant)
        .bind(owner)
        .bind(token_hash.to_vec())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let fish_id = Uuid::new_v4();
        publish_first_fish(
            &pool,
            scene_id,
            fish_id,
            "coral-fish",
            "paint-boat",
            Point { x: -5.0, y: 0.0 },
        )
        .await
        .unwrap();
        let store = AccessStore::new(pool.clone(), [5; 32]);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let command = InteractionCommand {
            kind: "command".into(),
            command_id: Uuid::new_v4(),
            session_id: session,
            scene_id,
            scene_epoch: 1,
            interaction_id: "boat".into(),
            point: realtime::Point { x: 1.0, y: 0.0 },
            target_action_id: None,
            expires_at: now_ms + 8_000,
        };
        let ack = realtime::process_command(&store, GrantKind::Owner, &token, session, &command)
            .await
            .unwrap();
        assert_eq!(ack["accepted"], true);
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &command)
                .await
                .unwrap(),
            ack
        );
        let next = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..command.clone()
        };
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &next)
                .await
                .unwrap()["code"],
            "BOAT_LIMIT"
        );
        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut frames = hub.subscribe();
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver,
            hub,
            Duration::from_millis(5),
        ));
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["activeActions"]
                    .as_array()
                    .is_some_and(|actions| actions.len() == 1)
                {
                    assert!(state["pendingInteractions"].as_array().unwrap().is_empty());
                    assert_eq!(state["activeActions"][0]["interactionId"], "boat");
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        let mut moving_frame = false;
        for _ in 0..8 {
            let frame = timeout(Duration::from_secs(5), frames.recv())
                .await
                .unwrap()
                .unwrap();
            if frame
                .action_positions
                .first()
                .is_some_and(|action| action.position.x > -6.0)
            {
                moving_frame = true;
                break;
            }
        }
        assert!(
            moving_frame,
            "boat must advance in transient position frames"
        );
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["activeActions"]
                    .as_array()
                    .is_some_and(|actions| actions.is_empty())
                {
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let restored = load_scene(&pool, scene_id).await.unwrap();
        assert!(restored.world.boat().is_none());
        assert!(!restored.world.fish()[0].fleeing);
        let transitions: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scene_events \
            WHERE scene_id = $1 AND event->>'type' = 'interaction_state'",
        )
        .bind(scene_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(transitions, 2, "start and exit each need one durable event");
        let after_exit = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..command
        };
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &after_exit)
                .await
                .unwrap()["code"],
            "BOAT_SCENE_COOLDOWN"
        );
    }

    #[tokio::test]
    async fn feed_boat_cancel_restores_feeding_after_checkpoint_reload() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        let token = Uuid::new_v4().to_string();
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("priority-{owner}")).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
             VALUES ($1, $2, $3, $3, now() + interval '1 day')",
        )
        .bind(Uuid::new_v4())
        .bind(owner)
        .bind(token_hash.to_vec())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        publish_first_fish(
            &pool,
            scene_id,
            Uuid::new_v4(),
            "coral-fish",
            "paint-priority",
            Point { x: -5.0, y: 0.0 },
        )
        .await
        .unwrap();
        let store = AccessStore::new(pool.clone(), [9; 32]);
        let command = |interaction_id: &str, point: realtime::Point| InteractionCommand {
            kind: "command".into(),
            command_id: Uuid::new_v4(),
            session_id: session,
            scene_id,
            scene_epoch: 1,
            interaction_id: interaction_id.into(),
            point,
            target_action_id: None,
            expires_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
                + 8_000,
        };
        let feed = command("feed", realtime::Point { x: -4.0, y: 0.0 });
        let feed_id = feed.command_id.simple().to_string();
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &feed)
                .await
                .unwrap()["accepted"],
            true
        );
        let mut scene = load_scene(&pool, scene_id).await.unwrap();
        assert!(
            apply_pending_interactions(&pool, scene_id, &mut scene)
                .await
                .unwrap()
        );
        scene.world.step();
        assert_eq!(
            scene.world.fish()[0].feeding.as_deref(),
            Some(feed_id.as_str())
        );
        assert!(!scene.world.fish()[0].fleeing);
        assert!(save_checkpoint(&pool, scene_id, &mut scene).await.unwrap());

        let boat = command("boat", realtime::Point { x: 1.0, y: 0.0 });
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &boat)
                .await
                .unwrap()["accepted"],
            true
        );
        let mut restored = load_scene(&pool, scene_id).await.unwrap();
        assert!(
            apply_pending_interactions(&pool, scene_id, &mut restored)
                .await
                .unwrap()
        );
        restored.world.step();
        assert!(restored.world.fish()[0].fleeing);
        assert!(restored.world.fish()[0].feeding.is_none());
        assert!(
            save_checkpoint(&pool, scene_id, &mut restored)
                .await
                .unwrap()
        );

        let mut cancel = command("cancel_boat", realtime::Point { x: 0.0, y: 0.0 });
        cancel.target_action_id = Some(format!("boat-{}", boat.command_id.simple()));
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &cancel)
                .await
                .unwrap()["accepted"],
            true
        );
        let mut restored = load_scene(&pool, scene_id).await.unwrap();
        assert!(
            apply_pending_interactions(&pool, scene_id, &mut restored)
                .await
                .unwrap()
        );
        assert!(restored.world.boat().is_none());
        assert!(!restored.world.fish()[0].fleeing);
        assert_eq!(
            restored.world.fish()[0].target,
            restored.world.fish()[0].position
        );
        restored.world.step();
        assert_eq!(
            restored.world.fish()[0].feeding.as_deref(),
            Some(feed_id.as_str())
        );
        assert!(
            save_checkpoint(&pool, scene_id, &mut restored)
                .await
                .unwrap()
        );
        let after_restart = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(
            after_restart.world.checkpoint(),
            restored.world.checkpoint()
        );
        assert_eq!(after_restart.world.feed_sources()[0].remaining, 10);
    }

    #[tokio::test]
    async fn owner_cancels_pending_feed_once_and_controller_cannot_cancel_it() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        let owner_token = Uuid::new_v4().to_string();
        let owner_hash: [u8; 32] = Sha256::digest(owner_token.as_bytes()).into();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("cancel-{owner}")).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
                     VALUES ($1, $2, $3, $3, now() + interval '1 day')",
        )
        .bind(Uuid::new_v4())
        .bind(owner)
        .bind(owner_hash.to_vec())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let controller_token = Uuid::new_v4().to_string();
        let controller_hash: [u8; 32] = Sha256::digest(controller_token.as_bytes()).into();
        sqlx::query(
            "INSERT INTO device_grants \
            (id, session_id, participant_id, role, token_hash, csrf_hash, expires_at) \
            VALUES ($1, $2, $3, 'controller', $4, $4, now() + interval '1 day')",
        )
        .bind(Uuid::new_v4())
        .bind(session)
        .bind(Uuid::new_v4())
        .bind(controller_hash.to_vec())
        .execute(&pool)
        .await
        .unwrap();
        let store = AccessStore::new(pool.clone(), [31; 32]);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let start = InteractionCommand {
            kind: "command".into(),
            command_id: Uuid::new_v4(),
            session_id: session,
            scene_id,
            scene_epoch: 1,
            interaction_id: "feed".into(),
            point: realtime::Point { x: 0.0, y: 0.0 },
            target_action_id: None,
            expires_at: now_ms + 8_000,
        };
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &owner_token, session, &start)
                .await
                .unwrap()["accepted"],
            true
        );
        let target = format!("feed-{}", start.command_id.simple());
        let cancel = InteractionCommand {
            command_id: Uuid::new_v4(),
            interaction_id: "cancel_feed".into(),
            target_action_id: Some(target.clone()),
            ..start.clone()
        };
        assert_eq!(
            realtime::process_command(
                &store,
                GrantKind::Controller,
                &controller_token,
                session,
                &cancel
            )
            .await
            .unwrap()["code"],
            "OWNER_REQUIRED"
        );
        let accepted =
            realtime::process_command(&store, GrantKind::Owner, &owner_token, session, &cancel)
                .await
                .unwrap();
        assert_eq!(accepted["accepted"], true);
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &owner_token, session, &cancel)
                .await
                .unwrap(),
            accepted
        );
        let second_cancel = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..cancel.clone()
        };
        assert_eq!(
            realtime::process_command(
                &store,
                GrantKind::Owner,
                &owner_token,
                session,
                &second_cancel
            )
            .await
            .unwrap()["code"],
            "ACTION_NOT_ACTIVE"
        );
        let (stop, receiver) = watch::channel(false);
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver,
            SimulationHub::default(),
            Duration::from_millis(5),
        ));
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["pendingInteractions"]
                    .as_array()
                    .is_some_and(|pending| pending.is_empty())
                {
                    assert!(state["activeActions"].as_array().unwrap().is_empty());
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        assert!(
            load_scene(&pool, scene_id)
                .await
                .unwrap()
                .world
                .feed_sources()
                .is_empty()
        );
        assert_eq!(
            realtime::process_command(
                &store,
                GrantKind::Owner,
                &owner_token,
                session,
                &second_cancel
            )
            .await
            .unwrap()["code"],
            "ACTION_NOT_ACTIVE"
        );
    }

    #[tokio::test]
    async fn accepted_feed_is_applied_once_consumed_and_restored() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let grant = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        let token = Uuid::new_v4().to_string();
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("feed-{owner}")).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
                     VALUES ($1, $2, $3, $3, now() + interval '1 day')",
        )
        .bind(grant)
        .bind(owner)
        .bind(token_hash.to_vec())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let store = AccessStore::new(pool.clone(), [3; 32]);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let command = InteractionCommand {
            kind: "command".into(),
            command_id: Uuid::new_v4(),
            session_id: session,
            scene_id,
            scene_epoch: 1,
            interaction_id: "feed".into(),
            point: realtime::Point { x: 0.0, y: 0.0 },
            target_action_id: None,
            expires_at: now_ms + 8_000,
        };
        let outside = InteractionCommand {
            command_id: Uuid::new_v4(),
            point: realtime::Point { x: 7.49, y: 0.0 },
            ..command.clone()
        };
        let invalid =
            realtime::process_command(&store, GrantKind::Owner, &token, session, &outside)
                .await
                .unwrap();
        assert_eq!(invalid["code"], "OUTSIDE_WATER");
        let ack = realtime::process_command(&store, GrantKind::Owner, &token, session, &command)
            .await
            .unwrap();
        assert_eq!(ack["accepted"], true);
        assert_eq!(ack["revision"], 1);
        let pending: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(pending["pendingInteractions"].as_array().unwrap().len(), 1);
        assert_eq!(pending["simulation"]["tick"], 0);
        let repeat = realtime::process_command(&store, GrantKind::Owner, &token, session, &command)
            .await
            .unwrap();
        assert_eq!(repeat, ack);
        let immediate = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..command.clone()
        };
        let rejected =
            realtime::process_command(&store, GrantKind::Owner, &token, session, &immediate)
                .await
                .unwrap();
        assert_eq!(rejected["accepted"], false);
        assert!(rejected["code"] == "FEED_SCENE_COOLDOWN" || rejected["code"] == "FEED_COOLDOWN");

        let (stop, receiver) = watch::channel(false);
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver,
            SimulationHub::default(),
            Duration::from_millis(5),
        ));
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["activeActions"]
                    .as_array()
                    .is_some_and(|actions| actions.len() == 1)
                {
                    assert!(state["pendingInteractions"].as_array().unwrap().is_empty());
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        queue_fish(
            &pool,
            scene_id,
            Uuid::new_v4(),
            "coral-fish",
            "paint-fed",
            Point { x: 0.5, y: 0.0 },
        )
        .await
        .unwrap();
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["activeActions"][0]["remaining"] == 9 {
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let restored = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(restored.world.feed_sources()[0].remaining, 9);
        assert_eq!(restored.world.feed_sources()[0].fed_fish.len(), 1);
        let consumed_at = restored.world.tick_number();
        let (stop_again, receiver_again) = watch::channel(false);
        let worker_again = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver_again,
            SimulationHub::default(),
            Duration::from_millis(5),
        ));
        timeout(Duration::from_secs(5), async {
            loop {
                let loaded = load_scene(&pool, scene_id).await.unwrap();
                if loaded.world.tick_number() >= consumed_at + 40 {
                    break loaded;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        stop_again.send(true).unwrap();
        worker_again.await.unwrap().unwrap();
        let after_restart = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(
            after_restart.world.feed_sources()[0].remaining,
            9,
            "restored fish must not eat the same portion twice"
        );
        let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state["activeActions"][0]["remaining"], 9);
        let (stop_final, receiver_final) = watch::channel(false);
        let worker_final = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver_final,
            SimulationHub::default(),
            Duration::from_millis(5),
        ));
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["activeActions"]
                    .as_array()
                    .is_some_and(|actions| actions.is_empty())
                {
                    assert!(
                        state["simulation"]["feed_sources"]
                            .as_array()
                            .unwrap()
                            .is_empty()
                    );
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        stop_final.send(true).unwrap();
        worker_final.await.unwrap().unwrap();
        let transitions: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scene_events \
            WHERE scene_id = $1 AND event->>'type' = 'interaction_state'",
        )
        .bind(scene_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            transitions >= 3,
            "start, consumption and expiry need revisioned events"
        );

        let participant = Uuid::new_v4();
        let first_controller = Uuid::new_v4().to_string();
        let second_controller = Uuid::new_v4().to_string();
        for token in [&first_controller, &second_controller] {
            let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            sqlx::query(
                "INSERT INTO device_grants \
                (id, session_id, participant_id, role, token_hash, csrf_hash, expires_at) \
                VALUES ($1, $2, $3, 'controller', $4, $4, now() + interval '1 day')",
            )
            .bind(Uuid::new_v4())
            .bind(session)
            .bind(participant)
            .bind(token_hash.to_vec())
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "UPDATE scenes SET state = jsonb_set(state, \
            '{interactionLimits,lastSceneFeedMs}', '0'::jsonb, true) WHERE id = $1",
        )
        .bind(scene_id)
        .execute(&pool)
        .await
        .unwrap();
        let fresh_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let first_controller_command = InteractionCommand {
            command_id: Uuid::new_v4(),
            expires_at: fresh_ms + 8_000,
            ..command.clone()
        };
        let accepted = realtime::process_command(
            &store,
            GrantKind::Controller,
            &first_controller,
            session,
            &first_controller_command,
        )
        .await
        .unwrap();
        assert_eq!(accepted["accepted"], true);
        sqlx::query(
            "UPDATE scenes SET state = jsonb_set(state, \
            '{interactionLimits,lastSceneFeedMs}', '0'::jsonb, true) WHERE id = $1",
        )
        .bind(scene_id)
        .execute(&pool)
        .await
        .unwrap();
        let second_controller_command = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..first_controller_command
        };
        let cooldown = realtime::process_command(
            &store,
            GrantKind::Controller,
            &second_controller,
            session,
            &second_controller_command,
        )
        .await
        .unwrap();
        assert_eq!(
            cooldown["code"], "FEED_COOLDOWN",
            "a new grant for the same participant must not bypass cooldown"
        );
    }

    #[tokio::test]
    async fn queued_fish_join_running_world_and_survive_restart() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("live-{owner}")).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let first = Uuid::new_v4();
        publish_first_fish(
            &pool,
            scene_id,
            first,
            "coral-fish",
            "paint-first",
            Point { x: -2.0, y: 0.0 },
        )
        .await
        .unwrap();
        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut frames = hub.subscribe();
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver,
            hub,
            Duration::from_millis(5),
        ));
        let initial = timeout(Duration::from_secs(5), frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(initial.positions.len(), 1);
        let second = Uuid::new_v4();
        let before: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before["entities"].as_array().unwrap().len(), 1);
        assert!(
            queue_fish(
                &pool,
                scene_id,
                Uuid::new_v4(),
                "stream-fish",
                "paint-bad",
                Point { x: 99.0, y: 0.0 }
            )
            .await
            .is_err()
        );
        queue_fish(
            &pool,
            scene_id,
            second,
            "stream-fish",
            "paint-second",
            Point { x: 2.0, y: 0.0 },
        )
        .await
        .unwrap();
        assert!(
            queue_fish(
                &pool,
                scene_id,
                second,
                "stream-fish",
                "paint-second",
                Point { x: 2.0, y: 0.0 }
            )
            .await
            .is_err()
        );
        let applied = timeout(Duration::from_secs(5), async {
            loop {
                let frame = frames.recv().await.unwrap();
                if frame.positions.len() == 2 {
                    break frame;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(applied.revision, 2);
        assert!(
            applied
                .positions
                .iter()
                .any(|fish| fish.id == format!("fish-{:032x}", second.as_u128()))
        );
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let restored = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(restored.world.fish().len(), 2);
        assert!(restored.persisted_tick >= 20);
        let second_event: Value = sqlx::query_scalar(
            "SELECT event FROM scene_events WHERE scene_id = $1 AND revision = 2",
        )
        .bind(scene_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(second_event["entity"]["paintBlobId"], "paint-second");

        let third = Uuid::new_v4();
        queue_fish(
            &pool,
            scene_id,
            third,
            "coral-fish",
            "paint-third",
            Point { x: 0.0, y: 2.0 },
        )
        .await
        .unwrap();
        let fourth = Uuid::new_v4();
        queue_fish(
            &pool,
            scene_id,
            fourth,
            "stream-fish",
            "paint-fourth",
            Point { x: 3.0, y: -2.0 },
        )
        .await
        .unwrap();
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM fish_publications WHERE scene_id = $1")
                .bind(scene_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(queued, 2);
        let accepted: (i32, Value) = sqlx::query_as(
            "SELECT definition_version, capabilities FROM fish_publications WHERE scene_id = $1 AND fish_id = $2",
        )
        .bind(scene_id)
        .bind(fourth)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(accepted.0, 1);
        assert_eq!(accepted.1["consume_food"], true);
        // Simulate a queued v1 fish whose accepted capabilities differ from
        // the currently installed package: the runner must use the queue row.
        sqlx::query("UPDATE fish_publications SET capabilities = $3::jsonb WHERE scene_id = $1 AND fish_id = $2")
            .bind(scene_id).bind(fourth)
            .bind(json!({"consume_food":false,"avoid_threat":true}))
            .execute(&pool).await.unwrap();
        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut frames = hub.subscribe();
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver,
            hub,
            Duration::from_millis(5),
        ));
        let resumed = timeout(Duration::from_secs(5), async {
            loop {
                let frame = frames.recv().await.unwrap();
                if frame.positions.len() == 4 {
                    break frame;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(resumed.revision, 4);
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let after = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(after.world.fish().len(), 4);
        assert!(
            !after
                .world
                .fish()
                .iter()
                .find(|fish| fish.id == fourth.as_u128())
                .unwrap()
                .capabilities
                .consume_food
        );
        let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state["entities"].as_array().unwrap().len(), 4);
        let revisions: Vec<i64> = sqlx::query_scalar(
            "SELECT revision FROM scene_events WHERE scene_id = $1 ORDER BY revision",
        )
        .bind(scene_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(revisions, [1, 2, 3, 4]);
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM fish_publications WHERE scene_id = $1")
                .bind(scene_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(queued, 0);
    }

    #[tokio::test]
    async fn checkpoint_survives_database_roundtrip_and_rejects_stale_worker() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("sim-{owner}")).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();

        let mut worker = load_scene(&pool, scene_id).await.unwrap();
        let mut stale = load_scene(&pool, scene_id).await.unwrap();
        sqlx::query("UPDATE scenes SET state = '{\"pendingInteractions\":[{\"type\":\"interaction_requested\"}]}'::jsonb WHERE id = $1")
            .bind(scene_id).execute(&pool).await.unwrap();
        worker
            .world
            .spawn_fish(scene_id.as_u128(), Point { x: -3.0, y: 0.0 }, 1.2)
            .unwrap();
        stale
            .world
            .spawn_fish(scene_id.as_u128(), Point { x: 3.0, y: 0.0 }, 1.2)
            .unwrap();
        for _ in 0..100 {
            worker.world.step();
            stale.world.step();
        }
        assert!(save_checkpoint(&pool, scene_id, &mut worker).await.unwrap());
        assert!(!save_checkpoint(&pool, scene_id, &mut stale).await.unwrap());
        let restored = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(restored.world.checkpoint(), worker.world.checkpoint());
        assert_eq!(restored.persisted_tick, 100);
        let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            state["simulation"]["fish"][0]["id"],
            format!("{:032x}", scene_id.as_u128())
        );
        assert_eq!(state["pendingInteractions"].as_array().unwrap().len(), 1);

        sqlx::query("UPDATE scenes SET scene_epoch = scene_epoch + 1 WHERE id = $1")
            .bind(scene_id)
            .execute(&pool)
            .await
            .unwrap();
        worker.world.step();
        assert!(!save_checkpoint(&pool, scene_id, &mut worker).await.unwrap());

        let incomplete = Uuid::new_v4();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version, state) VALUES ($1, $2, 'underwater', 1, '{\"entities\":[{\"id\":\"fish-1\"}]}'::jsonb)")
            .bind(incomplete).bind(session).execute(&pool).await.unwrap();
        assert!(matches!(
            load_scene(&pool, incomplete).await,
            Err(SimulationError::InvalidScene)
        ));

        let ticking = Uuid::new_v4();
        let mut initial = World::new(underwater_bounds().unwrap(), 42).unwrap();
        initial
            .spawn_fish(ticking.as_u128(), Point { x: -3.0, y: 0.0 }, 1.2)
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version, state) VALUES ($1, $2, 'underwater', 1, $3)")
            .bind(ticking)
            .bind(session)
            .bind(serde_json::json!({"simulation": initial.checkpoint()}))
            .execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(ticking)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut positions = hub.subscribe();
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            ticking,
            receiver,
            hub,
            Duration::from_millis(1),
        ));
        let first_frame = timeout(Duration::from_secs(5), positions.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_frame.scene_id, ticking);
        assert_eq!(first_frame.simulation_tick, 10);
        assert_eq!(
            first_frame.positions[0].id,
            format!("fish-{:032x}", ticking.as_u128())
        );
        timeout(Duration::from_secs(5), async {
            loop {
                let tick: i64 =
                    sqlx::query_scalar("SELECT simulation_tick FROM scenes WHERE id = $1")
                        .bind(ticking)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                if tick >= 100 {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("100 logical ticks should save one checkpoint");
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let restored = load_scene(&pool, ticking).await.unwrap();
        assert!(restored.persisted_tick >= 100);
        assert_eq!(restored.world.tick_number(), restored.persisted_tick as u64);
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM scenes WHERE id = $1")
            .bind(ticking)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            revision, 0,
            "position checkpoints must not create durable revisions"
        );
    }
}
