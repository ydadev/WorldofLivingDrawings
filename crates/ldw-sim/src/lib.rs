//! Deterministic, renderer-independent movement core for the fixed side-view world.
//! Positions are logical world units; callers own persistence and authoritative ticks.

use serde::{Deserialize, Serialize};

pub const MAX_FISH: usize = 100;
pub const MAX_OBSTACLES: usize = 16;
pub const MAX_ACTION_DEFINITIONS: usize = 8;
pub const TICKS_PER_SECOND: u64 = 20;
const FISH_RADIUS: f32 = 0.18;
const BODY_HALF_LENGTH: f32 = 1.05;
const BODY_CLEARANCE: f32 = 0.88;
pub const MIN_DEPTH: f32 = -1.5;
pub const MAX_DEPTH: f32 = 1.5;
const DEPTH_SPEED: f32 = 0.55;
const BOAT_RADIUS: f32 = 0.45;
const BOAT_SPEED: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectRule {
    pub definition_version: u32,
    pub radius: f32,
    pub duration_ticks: u64,
    pub max_active: usize,
    pub cooldown_ticks: u64,
    pub priority: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldInteractionRules {
    pub feed: EffectRule,
    pub boat: EffectRule,
    #[serde(default)]
    pub feed_behavior: FeedBehavior,
    #[serde(default)]
    pub boat_behavior: BoatBehavior,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InteractionEffect {
    Attraction,
    Threat,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionDefinition {
    pub id: String,
    pub label: String,
    pub allowed_zone_id: String,
    pub effect: InteractionEffect,
    pub rule: EffectRule,
    pub feed_behavior: Option<FeedBehavior>,
    pub boat_behavior: Option<BoatBehavior>,
}

impl ActionDefinition {
    fn valid(&self, bounds: Bounds) -> bool {
        let width = bounds.max_x - bounds.min_x;
        let safe_label = !self.label.is_empty()
            && self.label.chars().count() <= 64
            && !self.label.chars().any(char::is_control);
        valid_definition_id(&self.id)
            && safe_label
            && self.allowed_zone_id == "water"
            && self.rule.definition_version > 0
            && self.rule.radius.is_finite()
            && self.rule.radius > 0.0
            && self.rule.radius < width / 2.0
            && (1..=1200).contains(&self.rule.duration_ticks)
            && self.rule.cooldown_ticks <= 1200
            && match self.effect {
                InteractionEffect::Attraction => {
                    self.boat_behavior.is_none()
                        && (1..=3).contains(&self.rule.max_active)
                        && self.feed_behavior.is_some_and(FeedBehavior::valid)
                }
                InteractionEffect::Threat => {
                    self.feed_behavior.is_none()
                        && self.rule.max_active == 1
                        && self
                            .boat_behavior
                            .is_some_and(|behavior| behavior.valid(bounds, self.rule.radius))
                }
            }
    }
}

// The bounded v2 behavior chain is compiled to these parameters before a scene
// starts. Defaults preserve the exact behavior of v1 checkpoints.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedBehavior {
    pub candidate_limit: usize,
    pub reserve_limit: usize,
    pub target_depth: f32,
    pub eating_radius: f32,
    pub eating_depth_tolerance: f32,
}

impl Default for FeedBehavior {
    fn default() -> Self {
        Self {
            candidate_limit: MAX_FISH,
            reserve_limit: 10,
            target_depth: 0.0,
            eating_radius: 0.3,
            eating_depth_tolerance: 0.35,
        }
    }
}

impl FeedBehavior {
    fn valid(self) -> bool {
        (1..=MAX_FISH).contains(&self.candidate_limit)
            && (1..=10).contains(&self.reserve_limit)
            && self.target_depth.is_finite()
            && (MIN_DEPTH..=MAX_DEPTH).contains(&self.target_depth)
            && self.eating_radius.is_finite()
            && (0.1..=0.6).contains(&self.eating_radius)
            && self.eating_depth_tolerance.is_finite()
            && (0.1..=0.6).contains(&self.eating_depth_tolerance)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoatBehavior {
    pub candidate_limit: usize,
    pub hold_ticks: u64,
    pub release_radius_factor: f32,
    pub escape_depth: f32,
}

impl Default for BoatBehavior {
    fn default() -> Self {
        Self {
            candidate_limit: MAX_FISH,
            hold_ticks: 20,
            release_radius_factor: 1.25,
            escape_depth: MAX_DEPTH - 0.2,
        }
    }
}

impl BoatBehavior {
    fn valid(self, bounds: Bounds, radius: f32) -> bool {
        let width = bounds.max_x - bounds.min_x;
        (1..=MAX_FISH).contains(&self.candidate_limit)
            && self.hold_ticks <= 120
            && self.release_radius_factor.is_finite()
            && (1.0..=2.0).contains(&self.release_radius_factor)
            && radius * self.release_radius_factor < width / 2.0
            && self.escape_depth.is_finite()
            && (MIN_DEPTH..=MAX_DEPTH).contains(&self.escape_depth)
    }
}

impl Default for WorldInteractionRules {
    fn default() -> Self {
        // Old checkpoints predate this field and must keep their old behavior.
        Self {
            feed: EffectRule {
                definition_version: 1,
                radius: 3.75,
                duration_ticks: 300,
                max_active: 3,
                cooldown_ticks: 10,
                priority: 1,
            },
            boat: EffectRule {
                definition_version: 1,
                radius: 3.0,
                duration_ticks: 600,
                max_active: 1,
                cooldown_ticks: 200,
                priority: 10,
            },
            feed_behavior: FeedBehavior::default(),
            boat_behavior: BoatBehavior::default(),
        }
    }
}

impl WorldInteractionRules {
    pub fn valid(self, bounds: Bounds) -> bool {
        let width = bounds.max_x - bounds.min_x;
        [self.feed, self.boat].iter().all(|rule| {
            rule.definition_version > 0
                && rule.radius.is_finite()
                && rule.radius > 0.0
                && rule.radius < width / 2.0
                && (1..=1200).contains(&rule.duration_ticks)
                && rule.cooldown_ticks <= 1200
        }) && (1..=3).contains(&self.feed.max_active)
            && self.boat.max_active == 1
            && self.boat.priority > self.feed.priority
            && self.feed_behavior.valid()
            && self.boat_behavior.valid(bounds, self.boat.radius)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    fn distance_squared(self, other: Self) -> f32 {
        (self.x - other.x).powi(2) + (self.y - other.y).powi(2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
}

impl Bounds {
    pub fn contains(self, point: Point, margin: f32) -> bool {
        point.x >= self.min_x + margin
            && point.x <= self.max_x - margin
            && point.y >= self.min_y + margin
            && point.y <= self.max_y - margin
    }
    pub fn valid(self) -> bool {
        [self.min_x, self.max_x, self.min_y, self.max_y]
            .iter()
            .all(|v| v.is_finite())
            && self.max_x - self.min_x > 2.0 * FISH_RADIUS
            && self.max_y - self.min_y > 2.0 * FISH_RADIUS
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Circle {
    pub center: Point,
    pub radius: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fish {
    #[serde(with = "hex_u128")]
    pub id: u128,
    pub position: Point,
    pub target: Point,
    /// World units per second, before the fixed 20 Hz tick.
    pub speed: f32,
    #[serde(default)]
    pub capabilities: FishCapabilities,
    pub heading: Point,
    /// Signed distance from the center of the aquarium; negative is nearer the camera.
    #[serde(default)]
    pub depth: f32,
    #[serde(default)]
    pub depth_target: f32,
    #[serde(default)]
    pub heading_depth: f32,
    waypoint: Option<Point>,
    target_generation: u32,
    stuck_ticks: u16,
    #[serde(default)]
    pub feeding: Option<String>,
    #[serde(default)]
    pub fleeing: bool,
    #[serde(default)]
    threat_hold_until_tick: u64,
    #[serde(default)]
    pub ambient: Option<AmbientBehavior>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AmbientBehavior {
    Explore {
        until_tick: u64,
        rest_until_tick: u64,
        resume_target: Point,
        resume_depth: f32,
    },
    Approach {
        until_tick: u64,
        peer_id: String,
        resume_target: Point,
        resume_depth: f32,
    },
    Startled {
        until_tick: u64,
        resume_target: Point,
        resume_depth: f32,
    },
}

impl AmbientBehavior {
    fn until_tick(&self) -> u64 {
        match self {
            Self::Explore { until_tick, .. }
            | Self::Approach { until_tick, .. }
            | Self::Startled { until_tick, .. } => *until_tick,
        }
    }

    fn resume(&self) -> (Point, f32) {
        match self {
            Self::Explore {
                resume_target,
                resume_depth,
                ..
            }
            | Self::Approach {
                resume_target,
                resume_depth,
                ..
            }
            | Self::Startled {
                resume_target,
                resume_depth,
                ..
            } => (*resume_target, *resume_depth),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FishCapabilities {
    pub consume_food: bool,
    pub avoid_threat: bool,
}

impl Default for FishCapabilities {
    fn default() -> Self {
        // Historical fish could both eat and flee; preserve old checkpoints.
        Self {
            consume_food: true,
            avoid_threat: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedSource {
    pub id: String,
    #[serde(default = "default_feed_interaction_id")]
    pub interaction_id: String,
    pub position: Point,
    pub remaining: u8,
    pub expires_at_tick: u64,
    /// Each fish can consume at most one portion from this source.
    pub fed_fish: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Boat {
    pub id: String,
    #[serde(default = "default_boat_interaction_id")]
    pub interaction_id: String,
    pub position: Point,
    pub entry: Point,
    pub via: Point,
    pub exit: Point,
    pub expires_at_tick: u64,
    phase: u8,
}

mod hex_u128 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(id: &u128, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{id:032x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u128, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(D::Error::custom("expected 32 hexadecimal digits"));
        }
        u128::from_str_radix(&value, 16).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimError {
    InvalidBounds,
    InvalidPosition,
    InvalidSpeed,
    DuplicateId,
    FishLimit,
    ObstacleLimit,
    InvalidObstacle,
    UnknownFish,
    InvalidCheckpoint,
    InvalidFeedId,
    FeedLimit,
    DuplicateFeed,
    InvalidBoatId,
    BoatLimit,
    InvalidBoatRoute,
    InvalidInteractionRules,
    UnknownInteraction,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldCheckpoint {
    pub schema_version: u32,
    pub bounds: Bounds,
    pub fish: Vec<Fish>,
    pub obstacles: Vec<Circle>,
    pub tick: u64,
    pub seed: u64,
    #[serde(default)]
    pub interaction_rules: WorldInteractionRules,
    #[serde(default)]
    pub action_definitions: Vec<ActionDefinition>,
    #[serde(default)]
    pub feed_sources: Vec<FeedSource>,
    #[serde(default)]
    pub boat: Option<Boat>,
}

#[derive(Clone, Debug)]
pub struct World {
    bounds: Bounds,
    fish: Vec<Fish>,
    obstacles: Vec<Circle>,
    tick: u64,
    seed: u64,
    interaction_rules: WorldInteractionRules,
    action_definitions: Vec<ActionDefinition>,
    feed_sources: Vec<FeedSource>,
    boat: Option<Boat>,
}

impl World {
    pub fn new(bounds: Bounds, seed: u64) -> Result<Self, SimError> {
        Self::new_with_rules(bounds, seed, WorldInteractionRules::default())
    }

    pub fn new_with_rules(
        bounds: Bounds,
        seed: u64,
        interaction_rules: WorldInteractionRules,
    ) -> Result<Self, SimError> {
        if !bounds.valid() {
            return Err(SimError::InvalidBounds);
        }
        if !interaction_rules.valid(bounds) {
            return Err(SimError::InvalidInteractionRules);
        }
        Ok(Self {
            bounds,
            fish: Vec::new(),
            obstacles: Vec::new(),
            tick: 0,
            seed,
            interaction_rules,
            action_definitions: Vec::new(),
            feed_sources: Vec::new(),
            boat: None,
        })
    }

    pub fn new_with_catalog(
        bounds: Bounds,
        seed: u64,
        interaction_rules: WorldInteractionRules,
        action_definitions: Vec<ActionDefinition>,
    ) -> Result<Self, SimError> {
        let mut world = Self::new_with_rules(bounds, seed, interaction_rules)?;
        if !valid_action_catalog(&action_definitions, interaction_rules, bounds) {
            return Err(SimError::InvalidInteractionRules);
        }
        world.action_definitions = action_definitions;
        Ok(world)
    }

    pub fn tick_number(&self) -> u64 {
        self.tick
    }

    pub fn interaction_rules(&self) -> WorldInteractionRules {
        self.interaction_rules
    }

    pub fn action_definitions(&self) -> &[ActionDefinition] {
        &self.action_definitions
    }

    pub fn action_rule(&self, interaction_id: &str) -> Option<(InteractionEffect, EffectRule)> {
        self.feed_policy(interaction_id)
            .map(|(rule, _)| (InteractionEffect::Attraction, rule))
            .or_else(|| {
                self.boat_policy(interaction_id)
                    .map(|(rule, _)| (InteractionEffect::Threat, rule))
            })
    }

    pub fn checkpoint(&self) -> WorldCheckpoint {
        WorldCheckpoint {
            schema_version: 1,
            bounds: self.bounds,
            fish: self.fish.clone(),
            obstacles: self.obstacles.clone(),
            tick: self.tick,
            seed: self.seed,
            interaction_rules: self.interaction_rules,
            action_definitions: self.action_definitions.clone(),
            feed_sources: self.feed_sources.clone(),
            boat: self.boat.clone(),
        }
    }

    pub fn restore(checkpoint: WorldCheckpoint) -> Result<Self, SimError> {
        if checkpoint.schema_version != 1
            || !checkpoint.bounds.valid()
            || !checkpoint.interaction_rules.valid(checkpoint.bounds)
            || !valid_action_catalog(
                &checkpoint.action_definitions,
                checkpoint.interaction_rules,
                checkpoint.bounds,
            )
            || checkpoint.fish.len() > MAX_FISH
            || checkpoint.obstacles.len() > MAX_OBSTACLES
            || checkpoint.feed_sources.len() > checkpoint.interaction_rules.feed.max_active
        {
            return Err(SimError::InvalidCheckpoint);
        }
        for obstacle in &checkpoint.obstacles {
            if !obstacle.radius.is_finite()
                || obstacle.radius <= 0.0
                || !checkpoint.bounds.contains(obstacle.center, obstacle.radius)
            {
                return Err(SimError::InvalidCheckpoint);
            }
        }
        let mut seen = std::collections::HashSet::with_capacity(checkpoint.fish.len());
        let mut feed_ids = std::collections::HashSet::new();
        for source in &checkpoint.feed_sources {
            let mut fed = std::collections::HashSet::new();
            if !valid_feed_id(&source.id)
                || !valid_action_for_effect(
                    &checkpoint.action_definitions,
                    &source.interaction_id,
                    InteractionEffect::Attraction,
                )
                || !feed_ids.insert(source.id.as_str())
                || !valid_point(source.position, checkpoint.bounds, &checkpoint.obstacles)
                || source.remaining == 0
                || source.remaining > 10
                || source.expires_at_tick <= checkpoint.tick
                || source.fed_fish.len() + source.remaining as usize != 10
                || source
                    .fed_fish
                    .iter()
                    .any(|id| !valid_feed_id(id) || !fed.insert(id))
            {
                return Err(SimError::InvalidCheckpoint);
            }
        }
        if let Some(boat) = &checkpoint.boat {
            if !valid_action_id(&boat.id)
                || !valid_action_for_effect(
                    &checkpoint.action_definitions,
                    &boat.interaction_id,
                    InteractionEffect::Threat,
                )
                || boat.phase > 1
                || boat.expires_at_tick <= checkpoint.tick
                || !boat_route_valid(boat, checkpoint.bounds, &checkpoint.obstacles)
                || !valid_boat_point(boat.position, checkpoint.bounds, &checkpoint.obstacles)
            {
                return Err(SimError::InvalidCheckpoint);
            }
        }
        for fish in &checkpoint.fish {
            if !seen.insert(fish.id)
                || !fish.speed.is_finite()
                || !(0.1..=3.0).contains(&fish.speed)
                || !valid_point(fish.position, checkpoint.bounds, &checkpoint.obstacles)
                || !valid_point(fish.target, checkpoint.bounds, &checkpoint.obstacles)
                || fish.waypoint.is_some_and(|point| {
                    !valid_point(point, checkpoint.bounds, &checkpoint.obstacles)
                })
                || !fish.heading.x.is_finite()
                || !fish.heading.y.is_finite()
                || !fish.depth.is_finite()
                || !(MIN_DEPTH..=MAX_DEPTH).contains(&fish.depth)
                || !fish.depth_target.is_finite()
                || !(MIN_DEPTH..=MAX_DEPTH).contains(&fish.depth_target)
                || !fish.heading_depth.is_finite()
                || !(-1.0..=1.0).contains(&fish.heading_depth)
                || fish
                    .feeding
                    .as_ref()
                    .is_some_and(|id| !feed_ids.contains(id.as_str()))
                || (fish.feeding.is_some() && !fish.capabilities.consume_food)
                || (fish.fleeing && checkpoint.boat.is_none())
                || (fish.fleeing && !fish.capabilities.avoid_threat)
                || fish.ambient.as_ref().is_some_and(|ambient| {
                    let (resume_target, resume_depth) = ambient.resume();
                    !valid_point(resume_target, checkpoint.bounds, &checkpoint.obstacles)
                        || !resume_depth.is_finite()
                        || !(MIN_DEPTH..=MAX_DEPTH).contains(&resume_depth)
                        || ambient.until_tick() < checkpoint.tick
                        || fish.feeding.is_some()
                        || fish.fleeing
                        || matches!(ambient, AmbientBehavior::Explore { rest_until_tick, until_tick, .. }
                            if rest_until_tick > until_tick)
                        || matches!(ambient, AmbientBehavior::Approach { peer_id, .. }
                            if peer_id.len() != 32 || u128::from_str_radix(peer_id, 16).is_err())
                })
            {
                return Err(SimError::InvalidCheckpoint);
            }
        }
        Ok(Self {
            bounds: checkpoint.bounds,
            fish: checkpoint.fish,
            obstacles: checkpoint.obstacles,
            tick: checkpoint.tick,
            seed: checkpoint.seed,
            interaction_rules: checkpoint.interaction_rules,
            action_definitions: checkpoint.action_definitions,
            feed_sources: checkpoint.feed_sources,
            boat: checkpoint.boat,
        })
    }
    pub fn fish(&self) -> &[Fish] {
        &self.fish
    }
    pub fn obstacles(&self) -> &[Circle] {
        &self.obstacles
    }
    pub fn feed_sources(&self) -> &[FeedSource] {
        &self.feed_sources
    }
    pub fn boat(&self) -> Option<&Boat> {
        self.boat.as_ref()
    }

    pub fn start_boat(&mut self, id: &str, via: Point) -> Result<(), SimError> {
        self.start_action("boat", id, via)
    }

    pub fn start_feed(&mut self, id: &str, position: Point) -> Result<(), SimError> {
        self.start_action("feed", id, position)
    }

    pub fn start_action(
        &mut self,
        interaction_id: &str,
        id: &str,
        point: Point,
    ) -> Result<(), SimError> {
        if let Some((rule, _)) = self.feed_policy(interaction_id) {
            return self.start_feed_for(interaction_id, id, point, rule);
        }
        if let Some((rule, _)) = self.boat_policy(interaction_id) {
            return self.start_boat_for(interaction_id, id, point, rule);
        }
        Err(SimError::UnknownInteraction)
    }

    fn feed_policy(&self, interaction_id: &str) -> Option<(EffectRule, FeedBehavior)> {
        if self.action_definitions.is_empty() {
            return (interaction_id == "feed").then_some((
                self.interaction_rules.feed,
                self.interaction_rules.feed_behavior,
            ));
        }
        self.action_definitions
            .iter()
            .find(|definition| {
                definition.id == interaction_id
                    && definition.effect == InteractionEffect::Attraction
            })
            .and_then(|definition| {
                definition
                    .feed_behavior
                    .map(|behavior| (definition.rule, behavior))
            })
    }

    fn boat_policy(&self, interaction_id: &str) -> Option<(EffectRule, BoatBehavior)> {
        if self.action_definitions.is_empty() {
            return (interaction_id == "boat").then_some((
                self.interaction_rules.boat,
                self.interaction_rules.boat_behavior,
            ));
        }
        self.action_definitions
            .iter()
            .find(|definition| {
                definition.id == interaction_id && definition.effect == InteractionEffect::Threat
            })
            .and_then(|definition| {
                definition
                    .boat_behavior
                    .map(|behavior| (definition.rule, behavior))
            })
    }

    fn start_boat_for(
        &mut self,
        interaction_id: &str,
        id: &str,
        via: Point,
        rule: EffectRule,
    ) -> Result<(), SimError> {
        if !valid_action_id(id) {
            return Err(SimError::InvalidBoatId);
        }
        if self.boat.is_some() {
            return Err(SimError::BoatLimit);
        }
        if !valid_boat_point(via, self.bounds, &self.obstacles) {
            return Err(SimError::InvalidBoatRoute);
        }
        let left = Point {
            x: self.bounds.min_x + BOAT_RADIUS,
            y: via.y,
        };
        let right = Point {
            x: self.bounds.max_x - BOAT_RADIUS,
            y: via.y,
        };
        let (entry, exit) = if via.x >= (self.bounds.min_x + self.bounds.max_x) / 2.0 {
            (left, right)
        } else {
            (right, left)
        };
        let boat = Boat {
            id: id.to_owned(),
            interaction_id: interaction_id.to_owned(),
            position: entry,
            entry,
            via,
            exit,
            expires_at_tick: self.tick.saturating_add(rule.duration_ticks),
            phase: 0,
        };
        if !boat_route_valid(&boat, self.bounds, &self.obstacles) {
            return Err(SimError::InvalidBoatRoute);
        }
        self.boat = Some(boat);
        Ok(())
    }

    pub fn cancel_boat(&mut self, id: &str) -> bool {
        if self.boat.as_ref().is_none_or(|boat| boat.id != id) {
            return false;
        }
        self.boat = None;
        for fish in &mut self.fish {
            if fish.fleeing {
                clear_action_target(fish);
            }
            fish.fleeing = false;
            fish.threat_hold_until_tick = 0;
        }
        true
    }

    fn start_feed_for(
        &mut self,
        interaction_id: &str,
        id: &str,
        position: Point,
        rule: EffectRule,
    ) -> Result<(), SimError> {
        if !valid_feed_id(id) {
            return Err(SimError::InvalidFeedId);
        }
        if !valid_point(position, self.bounds, &self.obstacles) {
            return Err(SimError::InvalidPosition);
        }
        if self.feed_sources.iter().any(|source| source.id == id) {
            return Err(SimError::DuplicateFeed);
        }
        if self.feed_sources.len() >= self.interaction_rules.feed.max_active
            || self
                .feed_sources
                .iter()
                .filter(|source| source.interaction_id == interaction_id)
                .count()
                >= rule.max_active
        {
            return Err(SimError::FeedLimit);
        }
        self.feed_sources.push(FeedSource {
            id: id.to_owned(),
            interaction_id: interaction_id.to_owned(),
            position,
            remaining: 10,
            expires_at_tick: self.tick.saturating_add(rule.duration_ticks),
            fed_fish: Vec::new(),
        });
        Ok(())
    }

    pub fn cancel_feed(&mut self, id: &str) -> bool {
        let before = self.feed_sources.len();
        self.feed_sources.retain(|source| source.id != id);
        if self.feed_sources.len() == before {
            return false;
        }
        for fish in &mut self.fish {
            if fish.feeding.as_deref() == Some(id) {
                fish.feeding = None;
                if !fish.fleeing {
                    clear_action_target(fish);
                }
            }
        }
        true
    }

    pub fn add_obstacle(&mut self, obstacle: Circle) -> Result<(), SimError> {
        if self.obstacles.len() >= MAX_OBSTACLES {
            return Err(SimError::ObstacleLimit);
        }
        if !obstacle.radius.is_finite()
            || obstacle.radius <= 0.0
            || !self.bounds.contains(obstacle.center, obstacle.radius)
        {
            return Err(SimError::InvalidObstacle);
        }
        if self
            .fish
            .iter()
            .any(|fish| point_blocked(fish.position, &[obstacle]))
        {
            return Err(SimError::InvalidObstacle);
        }
        self.obstacles.push(obstacle);
        Ok(())
    }

    pub fn spawn_fish(&mut self, id: u128, position: Point, speed: f32) -> Result<(), SimError> {
        self.spawn_fish_with_capabilities(id, position, speed, FishCapabilities::default())
    }

    pub fn spawn_fish_with_capabilities(
        &mut self,
        id: u128,
        position: Point,
        speed: f32,
        capabilities: FishCapabilities,
    ) -> Result<(), SimError> {
        if self.fish.len() >= MAX_FISH {
            return Err(SimError::FishLimit);
        }
        if self.fish.iter().any(|fish| fish.id == id) {
            return Err(SimError::DuplicateId);
        }
        if !speed.is_finite() || !(0.1..=3.0).contains(&speed) {
            return Err(SimError::InvalidSpeed);
        }
        if !position.x.is_finite()
            || !position.y.is_finite()
            || !self.bounds.contains(position, FISH_RADIUS)
            || point_blocked(position, &self.obstacles)
        {
            return Err(SimError::InvalidPosition);
        }
        let target =
            choose_target(self.bounds, &self.obstacles, self.seed, id, 0).unwrap_or(position);
        self.fish.push(Fish {
            id,
            position,
            target,
            speed,
            capabilities,
            heading: Point { x: 1.0, y: 0.0 },
            depth: 0.0,
            depth_target: choose_depth(self.seed, id, 0),
            heading_depth: 0.0,
            waypoint: None,
            target_generation: 0,
            stuck_ticks: 0,
            feeding: None,
            fleeing: false,
            threat_hold_until_tick: 0,
            ambient: None,
        });
        Ok(())
    }

    /// Remove one participant while preserving the world's logical clock and
    /// active action resources. The returned position and capabilities can be
    /// used for an explicit restore, which starts with fresh behavior state.
    pub fn remove_fish(&mut self, id: u128) -> Result<Fish, SimError> {
        let index = self
            .fish
            .iter()
            .position(|fish| fish.id == id)
            .ok_or(SimError::UnknownFish)?;
        // A source's fed_fish is consumed-portion history, not an assignment.
        // Keep it so restoring the same fish during that source cannot eat a
        // second portion. Current feeding/fleeing assignments leave with Fish.
        let removed = self.fish.remove(index);
        let removed_id = fish_id(id);
        for fish in &mut self.fish {
            if matches!(&fish.ambient, Some(AmbientBehavior::Approach { peer_id, .. })
                if peer_id == &removed_id)
            {
                finish_ambient(fish);
            }
        }
        Ok(removed)
    }

    pub fn set_target(&mut self, id: u128, target: Point) -> Result<(), SimError> {
        if !target.x.is_finite()
            || !target.y.is_finite()
            || !self.bounds.contains(target, FISH_RADIUS)
            || point_blocked(target, &self.obstacles)
        {
            return Err(SimError::InvalidPosition);
        }
        let fish = self
            .fish
            .iter_mut()
            .find(|fish| fish.id == id)
            .ok_or(SimError::UnknownFish)?;
        fish.target = target;
        fish.ambient = None;
        fish.waypoint = None;
        fish.stuck_ticks = 0;
        Ok(())
    }

    fn update_ambient_behavior(&mut self) {
        for fish in &mut self.fish {
            if fish.feeding.is_some() || fish.fleeing {
                fish.ambient = None;
            } else if fish
                .ambient
                .as_ref()
                .is_some_and(|state| state.until_tick() <= self.tick)
            {
                finish_ambient(fish);
            }
        }
        let peers: Vec<_> = self
            .fish
            .iter()
            .map(|fish| {
                (
                    fish.id,
                    fish.position,
                    fish.depth,
                    fish.feeding.is_none() && !fish.fleeing && fish.ambient.is_none(),
                )
            })
            .collect();
        for fish in &mut self.fish {
            let Some(AmbientBehavior::Approach { peer_id, .. }) = &fish.ambient else {
                continue;
            };
            let id = u128::from_str_radix(peer_id, 16).expect("validated peer ID");
            if let Some((_, position, depth, true)) = peers.iter().find(|peer| peer.0 == id) {
                fish.target = *position;
                fish.depth_target = *depth;
                fish.waypoint = None;
            } else {
                finish_ambient(fish);
            }
        }
        if self.fish.len() > 1 && self.tick.wrapping_add(self.seed) % 120 == 0 {
            let available: Vec<_> = self
                .fish
                .iter()
                .enumerate()
                .filter(|(_, fish)| {
                    fish.ambient.is_none() && fish.feeding.is_none() && !fish.fleeing
                })
                .map(|(index, _)| index)
                .collect();
            if !available.is_empty() {
                let choice = xorshift(self.seed ^ self.tick) as usize % available.len();
                for step in 0..available.len() {
                    let initiator = available[(choice + step) % available.len()];
                    let fish = &self.fish[initiator];
                    let peer = available
                        .iter()
                        .copied()
                        .filter(|index| {
                            *index != initiator
                                && self.fish[*index].capabilities.avoid_threat
                                && (self.fish[*index].depth - fish.depth).abs() < 1.0
                                && self.fish[*index].position.distance_squared(fish.position) < 9.0
                        })
                        .min_by(|left, right| {
                            self.fish[*left]
                                .position
                                .distance_squared(fish.position)
                                .total_cmp(
                                    &self.fish[*right].position.distance_squared(fish.position),
                                )
                                .then(self.fish[*left].id.cmp(&self.fish[*right].id))
                        });
                    if let Some(peer) = peer {
                        let target = self.fish[peer].position;
                        let target_depth = self.fish[peer].depth;
                        let peer_id = fish_id(self.fish[peer].id);
                        let fish = &mut self.fish[initiator];
                        fish.ambient = Some(AmbientBehavior::Approach {
                            until_tick: self.tick.saturating_add(100),
                            peer_id,
                            resume_target: fish.target,
                            resume_depth: fish.depth_target,
                        });
                        fish.target = target;
                        fish.depth_target = target_depth;
                        fish.waypoint = None;
                        break;
                    }
                }
            }
        }
        let pursued: Vec<_> = self
            .fish
            .iter()
            .filter_map(|fish| {
                if let Some(AmbientBehavior::Approach { peer_id, .. }) = &fish.ambient {
                    u128::from_str_radix(peer_id, 16).ok()
                } else {
                    None
                }
            })
            .collect();
        for fish in &mut self.fish {
            if fish.ambient.is_some() || fish.feeding.is_some() || fish.fleeing {
                continue;
            }
            if pursued.contains(&fish.id) {
                continue;
            }
            let identity = xorshift(self.seed ^ folded_fish_id(fish.id));
            let period = 160 + identity % 120;
            let offset = (identity >> 16) % period;
            if self.tick.wrapping_add(offset) % period != 0
                || xorshift(identity ^ self.tick) % 4 != 0
            {
                continue;
            }
            let surface = xorshift(identity ^ self.tick ^ 0xa0761d6478bd642f) & 1 == 0;
            if let Some(destination) = exploration_target(
                fish.position,
                surface,
                self.bounds,
                &self.obstacles,
                fish.id,
            ) {
                fish.ambient = Some(AmbientBehavior::Explore {
                    until_tick: self.tick.saturating_add(260),
                    rest_until_tick: self.tick.saturating_add(12),
                    resume_target: fish.target,
                    resume_depth: fish.depth_target,
                });
                fish.target = destination;
                fish.depth_target = if surface { -0.7 } else { 0.7 };
                fish.waypoint = None;
            }
        }
    }

    fn separate_bodies(&mut self) {
        let bounds = self.bounds;
        let obstacles = &self.obstacles;
        let mut order: Vec<_> = (0..self.fish.len()).collect();
        order.sort_by(|left, right| {
            self.fish[*left]
                .position
                .x
                .total_cmp(&self.fish[*right].position.x)
                .then(self.fish[*left].id.cmp(&self.fish[*right].id))
        });
        for _ in 0..4 {
            let mut changed = false;
            for left_slot in 0..order.len() {
                for right_slot in left_slot + 1..order.len() {
                    let a = order[left_slot];
                    let b = order[right_slot];
                    if self.fish[b].position.x - self.fish[a].position.x
                        > 2.0 * BODY_HALF_LENGTH + BODY_CLEARANCE
                    {
                        break;
                    }
                    if (self.fish[a].position.y - self.fish[b].position.y).abs() > BODY_CLEARANCE {
                        continue;
                    }
                    let (left_index, right_index) = (a.min(b), a.max(b));
                    let (earlier, later) = self.fish.split_at_mut(right_index);
                    let left = &mut earlier[left_index];
                    let right = &mut later[0];
                    let gap = body_gap(BodyPose::from(&*left), BodyPose::from(&*right));
                    if gap >= -0.15 {
                        continue;
                    }
                    let sign = if left.position.y >= right.position.y {
                        1.0
                    } else {
                        -1.0
                    };
                    let push = ((-0.15 - gap) * 0.5).min(0.12);
                    let left_next = Point {
                        x: left.position.x,
                        y: left.position.y + sign * push,
                    };
                    let right_next = Point {
                        x: right.position.x,
                        y: right.position.y - sign * push,
                    };
                    if bounds.contains(left_next, FISH_RADIUS)
                        && segment_clear(left.position, left_next, obstacles)
                    {
                        left.position = left_next;
                        changed = true;
                    }
                    if bounds.contains(right_next, FISH_RADIUS)
                        && segment_clear(right.position, right_next, obstacles)
                    {
                        right.position = right_next;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn resolve_social_encounter(&mut self) {
        let encounter = self.fish.iter().enumerate().find_map(|(initiator, fish)| {
            let Some(AmbientBehavior::Approach { peer_id, .. }) = &fish.ambient else {
                return None;
            };
            let id = u128::from_str_radix(peer_id, 16).ok()?;
            let peer = self.fish.iter().position(|other| other.id == id)?;
            let other = &self.fish[peer];
            let close = body_gap(BodyPose::from(fish), BodyPose::from(other)) < 0.2;
            close.then_some((initiator, peer))
        });
        let Some((initiator, peer)) = encounter else {
            return;
        };
        let source = self.fish[initiator].position;
        let source_depth = self.fish[initiator].depth;
        let recipient = &self.fish[peer];
        let escape = if recipient.ambient.is_none()
            && recipient.feeding.is_none()
            && !recipient.fleeing
            && recipient.capabilities.avoid_threat
        {
            escape_target(
                recipient.position,
                source,
                recipient.id,
                self.bounds,
                &self.obstacles,
            )
        } else {
            None
        };
        finish_ambient(&mut self.fish[initiator]);
        if let Some(escape) = escape {
            let fish = &mut self.fish[peer];
            fish.ambient = Some(AmbientBehavior::Startled {
                until_tick: self.tick.saturating_add(60),
                resume_target: fish.target,
                resume_depth: fish.depth_target,
            });
            fish.target = escape;
            fish.depth_target = if fish.depth <= source_depth {
                -1.2
            } else {
                1.2
            };
            fish.waypoint = None;
        }
    }

    pub fn step(&mut self) {
        self.tick = self.tick.saturating_add(1);
        if let Some(boat) = &mut self.boat {
            let destination = if boat.phase == 0 { boat.via } else { boat.exit };
            let dx = destination.x - boat.position.x;
            let dy = destination.y - boat.position.y;
            let distance = (dx * dx + dy * dy).sqrt();
            let step = BOAT_SPEED / TICKS_PER_SECOND as f32;
            if distance <= step {
                boat.position = destination;
                if boat.phase == 0 {
                    boat.phase = 1;
                } else {
                    self.boat = None;
                }
            } else {
                boat.position.x += dx / distance * step;
                boat.position.y += dy / distance * step;
            }
        }
        if self
            .boat
            .as_ref()
            .is_some_and(|boat| self.tick >= boat.expires_at_tick)
        {
            self.boat = None;
        }
        let (boat_rule, boat_behavior) = self
            .boat
            .as_ref()
            .map(|boat| {
                self.boat_policy(&boat.interaction_id)
                    .expect("active boat has a validated definition")
            })
            .unwrap_or((
                self.interaction_rules.boat,
                self.interaction_rules.boat_behavior,
            ));
        let threatened_ids = self.boat.as_ref().map(|boat| {
            let mut candidates: Vec<_> = self
                .fish
                .iter()
                .filter(|fish| {
                    fish.capabilities.avoid_threat
                        && !fish.fleeing
                        && fish.position.distance_squared(boat.position) <= boat_rule.radius.powi(2)
                })
                .map(|fish| (fish.position.distance_squared(boat.position), fish.id))
                .collect();
            candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            candidates
                .into_iter()
                .take(boat_behavior.candidate_limit)
                .map(|(_, id)| id)
                .collect::<Vec<_>>()
        });
        for fish in &mut self.fish {
            let Some(boat) = &self.boat else {
                if fish.fleeing {
                    clear_action_target(fish);
                }
                fish.fleeing = false;
                fish.threat_hold_until_tick = 0;
                continue;
            };
            let distance_sq = fish.position.distance_squared(boat.position);
            let mut newly_fleeing = false;
            if threatened_ids
                .as_ref()
                .is_some_and(|ids| ids.contains(&fish.id))
            {
                fish.fleeing = true;
                newly_fleeing = true;
                fish.threat_hold_until_tick = self.tick.saturating_add(boat_behavior.hold_ticks);
                fish.waypoint = None;
            } else if fish.fleeing
                && distance_sq > (boat_rule.radius * boat_behavior.release_radius_factor).powi(2)
                && self.tick >= fish.threat_hold_until_tick
            {
                fish.fleeing = false;
                fish.threat_hold_until_tick = 0;
                clear_action_target(fish);
            }
            if fish.fleeing
                && (newly_fleeing
                    || fish.position.distance_squared(fish.target) < 0.09
                    || fish.stuck_ticks > 30
                    || self.tick % 10 == fish.id as u64 % 10)
                && let Some(target) = escape_target(
                    fish.position,
                    boat.position,
                    fish.id,
                    self.bounds,
                    &self.obstacles,
                )
            {
                fish.target = target;
                fish.depth_target = boat_behavior.escape_depth;
                fish.waypoint = None;
                fish.stuck_ticks = 0;
            }
        }
        self.feed_sources
            .retain(|source| source.remaining > 0 && source.expires_at_tick > self.tick);
        let feed_policies: Vec<_> = self
            .feed_sources
            .iter()
            .map(|source| {
                self.feed_policy(&source.interaction_id)
                    .expect("active feed has a validated definition")
            })
            .collect();
        let mut assignments = vec![None; self.fish.len()];
        for (source_index, source) in self.feed_sources.iter().enumerate() {
            let (feed_rule, feed_behavior) = feed_policies[source_index];
            let mut candidates: Vec<_> = self
                .fish
                .iter()
                .enumerate()
                .filter(|(index, fish)| {
                    assignments[*index].is_none()
                        && fish.capabilities.consume_food
                        && !fish.fleeing
                        && !source.fed_fish.contains(&fish_id(fish.id))
                        && fish.position.distance_squared(source.position)
                            <= feed_rule.radius.powi(2)
                        && (segment_clear(fish.position, source.position, &self.obstacles)
                            || plan_waypoint(
                                fish.position,
                                source.position,
                                fish.id,
                                self.bounds,
                                &self.obstacles,
                            )
                            .is_some())
                })
                .map(|(index, fish)| {
                    (
                        index,
                        fish.position.distance_squared(source.position),
                        fish.id,
                    )
                })
                .collect();
            candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)));
            for (index, _, _) in candidates.into_iter().take(
                (source.remaining as usize)
                    .min(feed_behavior.reserve_limit)
                    .min(feed_behavior.candidate_limit),
            ) {
                assignments[index] = Some(source_index);
            }
        }
        for (index, assigned) in assignments.iter().enumerate() {
            let fish = &mut self.fish[index];
            if fish.feeding.is_some() && assigned.is_none() && !fish.fleeing {
                clear_action_target(fish);
            }
            fish.feeding = assigned.map(|source_index| self.feed_sources[source_index].id.clone());
            if let Some(source_index) = assigned {
                let source = &self.feed_sources[*source_index];
                fish.target = feeding_target(
                    fish.position,
                    source.position,
                    fish.id,
                    self.bounds,
                    &self.obstacles,
                )
                .unwrap_or(source.position);
                fish.depth_target = feed_policies[*source_index].1.target_depth;
                fish.waypoint = None;
            }
        }
        self.update_ambient_behavior();
        let mut bodies: Vec<_> = self.fish.iter().map(BodyPose::from).collect();
        for (index, fish) in self.fish.iter_mut().enumerate() {
            if matches!(&fish.ambient, Some(AmbientBehavior::Explore { rest_until_tick, .. })
                if self.tick < *rest_until_tick)
            {
                fish.heading_depth = 0.0;
                continue;
            }
            let identity = xorshift(self.seed ^ folded_fish_id(fish.id));
            let course_period = 180 + identity % 180;
            let course_change = self.tick.wrapping_add(identity >> 24) % course_period == 0;
            if fish.feeding.is_none()
                && !fish.fleeing
                && fish.ambient.is_none()
                && (course_change
                    || (fish.position.distance_squared(fish.target) < 0.04
                        && (fish.depth - fish.depth_target).abs() < 0.12)
                    || fish.stuck_ticks > 80)
            {
                fish.target_generation = fish.target_generation.wrapping_add(1);
                if let Some(target) = choose_target(
                    self.bounds,
                    &self.obstacles,
                    self.seed,
                    fish.id,
                    fish.target_generation,
                ) {
                    fish.target = target;
                    fish.depth_target = choose_depth(self.seed, fish.id, fish.target_generation);
                }
                fish.waypoint = None;
                fish.stuck_ticks = 0;
            }
            if fish
                .waypoint
                .is_some_and(|waypoint| fish.position.distance_squared(waypoint) < 0.04)
            {
                fish.waypoint = None;
            }
            if fish.waypoint.is_none()
                && !segment_clear(fish.position, fish.target, &self.obstacles)
            {
                fish.waypoint = plan_waypoint(
                    fish.position,
                    fish.target,
                    fish.id,
                    self.bounds,
                    &self.obstacles,
                );
            }
            let destination = fish.waypoint.unwrap_or(fish.target);
            let delta = Point {
                x: destination.x - fish.position.x,
                y: destination.y - fish.position.y,
            };
            let distance = (delta.x * delta.x + delta.y * delta.y).sqrt();
            let cruise = fish.speed * cruise_speed_factor(self.seed, fish.id, self.tick);
            let swim_speed = if fish.feeding.is_some()
                || fish.fleeing
                || matches!(
                    fish.ambient.as_ref(),
                    Some(AmbientBehavior::Approach { .. } | AmbientBehavior::Startled { .. })
                ) {
                fish.speed
            } else if fish.ambient.is_some() {
                cruise.min(fish.speed * 0.65)
            } else {
                cruise
            };
            let depth_heading = advance_depth(fish, swim_speed);
            let step_budget = swim_speed / TICKS_PER_SECOND as f32;
            fish.heading_depth = depth_heading;
            if distance <= f32::EPSILON {
                if depth_heading != 0.0 {
                    fish.heading = Point { x: 0.0, y: 0.0 };
                }
                bodies[index] = BodyPose::from(&*fish);
                continue;
            }
            let mut desired = Point {
                x: delta.x / distance,
                y: delta.y / distance,
            };
            for other in &bodies {
                if other.id == fish.id {
                    continue;
                }
                let toward = Point {
                    x: other.position.x - fish.position.x,
                    y: other.position.y - fish.position.y,
                };
                let gap = body_gap(bodies[index], *other);
                if gap >= 1.1 || desired.x * toward.x + desired.y * toward.y <= 0.0 {
                    continue;
                }
                let side = Point {
                    x: -desired.y,
                    y: desired.x,
                };
                let sign = if fish.position.y + side.y > self.bounds.max_y - 0.5
                    || fish.position.y + side.y < self.bounds.min_y + 0.5
                {
                    -1.0
                } else {
                    1.0
                };
                let weight = ((1.1 - gap) * 1.8).clamp(0.0, 2.2);
                let x = desired.x + side.x * sign * weight;
                let y = desired.y + side.y * sign * weight;
                let length = x.hypot(y);
                desired = Point {
                    x: x / length,
                    y: y / length,
                };
            }
            let step =
                (step_budget * (1.0 - depth_heading * depth_heading).max(0.0).sqrt()).min(distance);
            let left_first = fish.id & 1 == 0;
            let directions = if left_first {
                [
                    (1.0, 0.0),
                    (0.70710677, 0.70710677),
                    (0.0, 1.0),
                    (0.70710677, -0.70710677),
                    (0.0, -1.0),
                    (-0.70710677, 0.70710677),
                    (-0.70710677, -0.70710677),
                ]
            } else {
                [
                    (1.0, 0.0),
                    (0.70710677, -0.70710677),
                    (0.0, -1.0),
                    (0.70710677, 0.70710677),
                    (0.0, 1.0),
                    (-0.70710677, -0.70710677),
                    (-0.70710677, 0.70710677),
                ]
            };
            let mut moved = false;
            for (forward, side) in directions {
                let direction = Point {
                    x: forward * desired.x - side * desired.y,
                    y: forward * desired.y + side * desired.x,
                };
                let next = Point {
                    x: fish.position.x + direction.x * step,
                    y: fish.position.y + direction.y * step,
                };
                if self.bounds.contains(next, FISH_RADIUS)
                    && segment_clear(fish.position, next, &self.obstacles)
                {
                    fish.position = next;
                    fish.heading = Point {
                        x: direction.x * step / step_budget,
                        y: direction.y * step / step_budget,
                    };
                    fish.stuck_ticks = 0;
                    moved = true;
                    break;
                }
            }
            if !moved {
                fish.stuck_ticks = fish.stuck_ticks.saturating_add(1);
            }
            bodies[index] = BodyPose::from(&*fish);
        }
        self.separate_bodies();
        self.resolve_social_encounter();
        for fish in &mut self.fish {
            let Some((source, _)) = self.feed_sources.iter_mut().zip(feed_policies.iter()).find(
                |(source, (_, behavior))| {
                    fish.feeding.as_deref() == Some(source.id.as_str())
                        && fish.position.distance_squared(source.position)
                            <= behavior.eating_radius.powi(2)
                        && (fish.depth - behavior.target_depth).abs()
                            <= behavior.eating_depth_tolerance
                },
            ) else {
                continue;
            };
            if source.remaining > 0 && !source.fed_fish.contains(&fish_id(fish.id)) {
                source.remaining -= 1;
                source.fed_fish.push(fish_id(fish.id));
                fish.feeding = None;
                clear_action_target(fish);
            }
        }
        self.feed_sources.retain(|source| source.remaining > 0);
        for fish in &mut self.fish {
            if fish
                .feeding
                .as_ref()
                .is_some_and(|id| !self.feed_sources.iter().any(|source| &source.id == id))
            {
                fish.feeding = None;
                if !fish.fleeing {
                    clear_action_target(fish);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct BodyPose {
    id: u128,
    position: Point,
    depth: f32,
    heading: Point,
    heading_depth: f32,
}

impl From<&Fish> for BodyPose {
    fn from(fish: &Fish) -> Self {
        Self {
            id: fish.id,
            position: fish.position,
            depth: fish.depth,
            heading: fish.heading,
            heading_depth: fish.heading_depth,
        }
    }
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn body_axis(pose: BodyPose) -> ([f32; 3], [f32; 3]) {
    let length = pose.heading.x.hypot(pose.heading_depth);
    let (x, z) = if length > 1e-6 {
        (pose.heading.x / length, pose.heading_depth / length)
    } else {
        (1.0, 0.0)
    };
    let offset_x = x * BODY_HALF_LENGTH;
    let offset_z = z * BODY_HALF_LENGTH;
    (
        [
            pose.position.x - offset_x,
            pose.position.y,
            pose.depth - offset_z,
        ],
        [
            pose.position.x + offset_x,
            pose.position.y,
            pose.depth + offset_z,
        ],
    )
}

// Closest distance of the two oriented body axes in the 3D water volume.
fn body_gap(left: BodyPose, right: BodyPose) -> f32 {
    let axis_lower_bound = ((left.position.x - right.position.x).abs() - 2.0 * BODY_HALF_LENGTH)
        .max((left.position.y - right.position.y).abs())
        .max((left.depth - right.depth).abs() - 2.0 * BODY_HALF_LENGTH)
        - BODY_CLEARANCE;
    if axis_lower_bound > 1.1 {
        return axis_lower_bound;
    }
    let (a, b) = body_axis(left);
    let (c, d) = body_axis(right);
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [d[0] - c[0], d[1] - c[1], d[2] - c[2]];
    let w = [a[0] - c[0], a[1] - c[1], a[2] - c[2]];
    let aa = dot3(u, u);
    let bb = dot3(u, v);
    let cc = dot3(v, v);
    let dd = dot3(u, w);
    let ee = dot3(v, w);
    let denominator = aa * cc - bb * bb;
    let mut s = if denominator > 1e-8 {
        ((bb * ee - cc * dd) / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut t = ((bb * s + ee) / cc).clamp(0.0, 1.0);
    s = ((bb * t - dd) / aa).clamp(0.0, 1.0);
    t = ((bb * s + ee) / cc).clamp(0.0, 1.0);
    let closest = [
        w[0] + u[0] * s - v[0] * t,
        w[1] + u[1] * s - v[1] * t,
        w[2] + u[2] * s - v[2] * t,
    ];
    dot3(closest, closest).sqrt() - BODY_CLEARANCE
}

fn clear_action_target(fish: &mut Fish) {
    fish.ambient = None;
    fish.target = fish.position;
    fish.depth_target = fish.depth;
    fish.waypoint = None;
    fish.stuck_ticks = 0;
}

fn finish_ambient(fish: &mut Fish) {
    if let Some(state) = fish.ambient.take() {
        let (target, depth) = state.resume();
        fish.target = target;
        fish.depth_target = depth;
        fish.waypoint = None;
        fish.stuck_ticks = 0;
    }
}

fn folded_fish_id(id: u128) -> u64 {
    id as u64 ^ ((id >> 64) as u64).rotate_left(29)
}

fn cruise_speed_factor(seed: u64, id: u128, tick: u64) -> f32 {
    let identity = xorshift(seed ^ folded_fish_id(id) ^ 0x9e3779b97f4a7c15);
    let period = 100 + identity % 160;
    let clock = tick.wrapping_add((identity >> 16) % period);
    let block = clock / period;
    let fraction = (clock % period) as f32 / period as f32;
    let smooth = fraction * fraction * (3.0 - 2.0 * fraction);
    let level = |index: u64| {
        let value = xorshift(identity ^ index.wrapping_mul(0xbf58476d1ce4e5b9));
        (value as u32) as f32 / u32::MAX as f32
    };
    0.38 + 0.62 * (level(block) * (1.0 - smooth) + level(block.wrapping_add(1)) * smooth)
}

fn exploration_target(
    from: Point,
    surface: bool,
    bounds: Bounds,
    obstacles: &[Circle],
    id: u128,
) -> Option<Point> {
    let y = if surface {
        bounds.max_y - FISH_RADIUS - 0.3
    } else {
        bounds.min_y + FISH_RADIUS + 0.3
    };
    let side = if id & 1 == 0 { 1.0 } else { -1.0 };
    for offset in [0.0, 0.8, -0.8, 1.6, -1.6] {
        let target = Point {
            x: (from.x + offset * side)
                .clamp(bounds.min_x + FISH_RADIUS, bounds.max_x - FISH_RADIUS),
            y,
        };
        if valid_point(target, bounds, obstacles)
            && (segment_clear(from, target, obstacles)
                || plan_waypoint(from, target, id, bounds, obstacles).is_some())
        {
            return Some(target);
        }
    }
    None
}

fn default_feed_interaction_id() -> String {
    "feed".into()
}

fn default_boat_interaction_id() -> String {
    "boat".into()
}

fn valid_definition_id(id: &str) -> bool {
    (2..=64).contains(&id.len())
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_action_for_effect(
    definitions: &[ActionDefinition],
    id: &str,
    effect: InteractionEffect,
) -> bool {
    if definitions.is_empty() {
        return matches!(
            (id, effect),
            ("feed", InteractionEffect::Attraction) | ("boat", InteractionEffect::Threat)
        );
    }
    definitions
        .iter()
        .any(|definition| definition.id == id && definition.effect == effect)
}

fn valid_action_catalog(
    definitions: &[ActionDefinition],
    rules: WorldInteractionRules,
    bounds: Bounds,
) -> bool {
    if definitions.is_empty() {
        return true;
    }
    if !(2..=MAX_ACTION_DEFINITIONS).contains(&definitions.len()) {
        return false;
    }
    let mut seen = std::collections::HashSet::with_capacity(definitions.len());
    if definitions
        .iter()
        .any(|definition| !definition.valid(bounds) || !seen.insert(definition.id.as_str()))
    {
        return false;
    }
    let feed = definitions
        .iter()
        .find(|definition| definition.id == "feed");
    let boat = definitions
        .iter()
        .find(|definition| definition.id == "boat");
    if !feed.is_some_and(|definition| {
        definition.effect == InteractionEffect::Attraction
            && definition.rule == rules.feed
            && definition.feed_behavior == Some(rules.feed_behavior)
    }) || !boat.is_some_and(|definition| {
        definition.effect == InteractionEffect::Threat
            && definition.rule == rules.boat
            && definition.boat_behavior == Some(rules.boat_behavior)
    }) {
        return false;
    }
    let strongest_attraction = definitions
        .iter()
        .filter(|definition| definition.effect == InteractionEffect::Attraction)
        .map(|definition| definition.rule.priority)
        .max();
    let weakest_threat = definitions
        .iter()
        .filter(|definition| definition.effect == InteractionEffect::Threat)
        .map(|definition| definition.rule.priority)
        .min();
    matches!((strongest_attraction, weakest_threat), (Some(a), Some(t)) if t > a)
}

fn fish_id(id: u128) -> String {
    format!("{id:032x}")
}

fn valid_feed_id(id: &str) -> bool {
    valid_action_id(id)
}

fn valid_action_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_boat_point(point: Point, bounds: Bounds, obstacles: &[Circle]) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && bounds.contains(point, BOAT_RADIUS)
        && !obstacles.iter().any(|obstacle| {
            point.distance_squared(obstacle.center) < (obstacle.radius + BOAT_RADIUS).powi(2)
        })
}

fn boat_route_valid(boat: &Boat, bounds: Bounds, obstacles: &[Circle]) -> bool {
    let (segment_start, segment_end) = if boat.phase == 0 {
        (boat.entry.x, boat.via.x)
    } else {
        (boat.via.x, boat.exit.x)
    };
    let on_current_segment = (boat.position.y - boat.via.y).abs() < 0.0001
        && boat.position.x >= segment_start.min(segment_end) - 0.0001
        && boat.position.x <= segment_start.max(segment_end) + 0.0001;
    [boat.entry, boat.via, boat.exit]
        .iter()
        .all(|point| valid_boat_point(*point, bounds, obstacles))
        && on_current_segment
        && segment_clear_with_margin(boat.entry, boat.via, obstacles, BOAT_RADIUS)
        && segment_clear_with_margin(boat.via, boat.exit, obstacles, BOAT_RADIUS)
        && segment_clear_with_margin(boat.entry, boat.position, obstacles, BOAT_RADIUS)
}

fn escape_target(
    from: Point,
    threat: Point,
    id: u128,
    bounds: Bounds,
    obstacles: &[Circle],
) -> Option<Point> {
    let dx = from.x - threat.x;
    let dy = from.y - threat.y;
    let length = (dx * dx + dy * dy).sqrt();
    let (away_x, away_y) = if length > f32::EPSILON {
        (dx / length, dy / length)
    } else if id & 1 == 0 {
        (0.0, 1.0)
    } else {
        (0.0, -1.0)
    };
    let side = if id & 1 == 0 { 1.0 } else { -1.0 };
    let directions = [
        (away_x, away_y),
        (
            away_x * 0.70710677 - side * away_y * 0.70710677,
            away_y * 0.70710677 + side * away_x * 0.70710677,
        ),
        (
            away_x * 0.70710677 + side * away_y * 0.70710677,
            away_y * 0.70710677 - side * away_x * 0.70710677,
        ),
        (-side * away_y, side * away_x),
        (side * away_y, -side * away_x),
    ];
    let mut best: Option<(Point, f32)> = None;
    for distance in [2.5, 1.5, 0.75] {
        for (vx, vy) in directions {
            let candidate = Point {
                x: from.x + vx * distance,
                y: from.y + vy * distance,
            };
            if !valid_point(candidate, bounds, obstacles)
                || candidate.distance_squared(threat) <= from.distance_squared(threat) + 0.01
                || !(segment_clear(from, candidate, obstacles)
                    || plan_waypoint(from, candidate, id, bounds, obstacles).is_some())
            {
                continue;
            }
            let gain = candidate.distance_squared(threat) - from.distance_squared(threat);
            if best.as_ref().is_none_or(|(_, score)| gain > *score) {
                best = Some((candidate, gain));
            }
        }
    }
    best.map(|(point, _)| point)
}

fn feeding_target(
    from: Point,
    position: Point,
    id: u128,
    bounds: Bounds,
    obstacles: &[Circle],
) -> Option<Point> {
    let directions = [
        (1.0, 0.0),
        (0.70710677, 0.70710677),
        (0.0, 1.0),
        (-0.70710677, 0.70710677),
        (-1.0, 0.0),
        (-0.70710677, -0.70710677),
        (0.0, -1.0),
        (0.70710677, -0.70710677),
    ];
    for offset in 0..directions.len() {
        let (dx, dy) = directions[(id as usize + offset) % directions.len()];
        let target = Point {
            x: position.x + dx * 0.24,
            y: position.y + dy * 0.24,
        };
        if valid_point(target, bounds, obstacles)
            && (segment_clear(from, target, obstacles)
                || plan_waypoint(from, target, id, bounds, obstacles).is_some())
        {
            return Some(target);
        }
    }
    None
}

fn point_blocked(point: Point, obstacles: &[Circle]) -> bool {
    obstacles.iter().any(|obstacle| {
        point.distance_squared(obstacle.center) < (obstacle.radius + FISH_RADIUS).powi(2)
    })
}

fn valid_point(point: Point, bounds: Bounds, obstacles: &[Circle]) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && bounds.contains(point, FISH_RADIUS)
        && !point_blocked(point, obstacles)
}

fn segment_clear(start: Point, end: Point, obstacles: &[Circle]) -> bool {
    segment_clear_with_margin(start, end, obstacles, FISH_RADIUS)
}

fn segment_clear_with_margin(start: Point, end: Point, obstacles: &[Circle], margin: f32) -> bool {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_sq = dx * dx + dy * dy;
    obstacles.iter().all(|obstacle| {
        let t = if length_sq <= f32::EPSILON {
            0.0
        } else {
            (((obstacle.center.x - start.x) * dx + (obstacle.center.y - start.y) * dy) / length_sq)
                .clamp(0.0, 1.0)
        };
        let closest = Point {
            x: start.x + t * dx,
            y: start.y + t * dy,
        };
        closest.distance_squared(obstacle.center) >= (obstacle.radius + margin).powi(2)
    })
}

fn plan_waypoint(
    start: Point,
    target: Point,
    id: u128,
    bounds: Bounds,
    obstacles: &[Circle],
) -> Option<Point> {
    let dx = target.x - start.x;
    let dy = target.y - start.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f32::EPSILON {
        return None;
    }
    let perpendicular = Point {
        x: -dy / length,
        y: dx / length,
    };
    for obstacle in obstacles {
        if segment_clear(start, target, &[*obstacle]) {
            continue;
        }
        let sides = if id & 1 == 0 {
            [1.0, -1.0]
        } else {
            [-1.0, 1.0]
        };
        for offset in [0.45, 0.8, 1.2] {
            for side in sides {
                let distance = obstacle.radius + FISH_RADIUS + offset;
                let waypoint = Point {
                    x: obstacle.center.x + perpendicular.x * side * distance,
                    y: obstacle.center.y + perpendicular.y * side * distance,
                };
                if bounds.contains(waypoint, FISH_RADIUS)
                    && segment_clear(start, waypoint, obstacles)
                    && segment_clear(waypoint, target, obstacles)
                {
                    return Some(waypoint);
                }
            }
        }
    }
    None
}

fn choose_target(
    bounds: Bounds,
    obstacles: &[Circle],
    seed: u64,
    id: u128,
    generation: u32,
) -> Option<Point> {
    let folded_id = id as u64 ^ ((id >> 64) as u64).rotate_left(29);
    let mut state = seed
        ^ folded_id.wrapping_mul(0x9e3779b97f4a7c15)
        ^ (generation as u64).wrapping_mul(0xbf58476d1ce4e5b9);
    for _ in 0..16 {
        state = xorshift(state);
        let fx = (state as u32) as f32 / u32::MAX as f32;
        state = xorshift(state);
        let fy = (state as u32) as f32 / u32::MAX as f32;
        let target = Point {
            x: bounds.min_x + FISH_RADIUS + fx * (bounds.max_x - bounds.min_x - 2.0 * FISH_RADIUS),
            y: bounds.min_y + FISH_RADIUS + fy * (bounds.max_y - bounds.min_y - 2.0 * FISH_RADIUS),
        };
        if !point_blocked(target, obstacles) {
            return Some(target);
        }
    }
    None
}

fn choose_depth(seed: u64, id: u128, generation: u32) -> f32 {
    let folded_id = id as u64 ^ ((id >> 64) as u64).rotate_left(29);
    let state = xorshift(seed ^ folded_id ^ (generation as u64).wrapping_mul(0x94d049bb133111eb));
    let fraction = (state as u32) as f32 / u32::MAX as f32;
    let sign = if (generation as u64 + (folded_id & 1)) & 1 == 0 {
        1.0
    } else {
        -1.0
    };
    sign * (0.65 + fraction * 0.75)
}

fn advance_depth(fish: &mut Fish, swim_speed: f32) -> f32 {
    let step = DEPTH_SPEED.min(swim_speed * 0.6) / TICKS_PER_SECOND as f32;
    let change = (fish.depth_target - fish.depth).clamp(-step, step);
    fish.depth = (fish.depth + change).clamp(MIN_DEPTH, MAX_DEPTH);
    change / (swim_speed / TICKS_PER_SECOND as f32)
}

fn xorshift(mut value: u64) -> u64 {
    if value == 0 {
        value = 0x6a09e667f3bcc909;
    }
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Bounds {
        Bounds {
            min_x: -7.5,
            max_x: 7.5,
            min_y: -4.0,
            max_y: 4.0,
        }
    }

    fn action_catalog(rules: WorldInteractionRules) -> Vec<ActionDefinition> {
        let mut slow_feed = rules.feed;
        slow_feed.radius = 2.0;
        slow_feed.duration_ticks = 40;
        slow_feed.max_active = 2;
        let mut slow_behavior = rules.feed_behavior;
        slow_behavior.candidate_limit = 1;
        slow_behavior.reserve_limit = 1;
        slow_behavior.target_depth = -0.8;
        let mut small_boat = rules.boat;
        small_boat.priority += 1;
        small_boat.duration_ticks = 80;
        let mut small_behavior = rules.boat_behavior;
        small_behavior.candidate_limit = 1;
        small_behavior.escape_depth = -0.8;
        vec![
            ActionDefinition {
                id: "feed".into(),
                label: "Корм".into(),
                allowed_zone_id: "water".into(),
                effect: InteractionEffect::Attraction,
                rule: rules.feed,
                feed_behavior: Some(rules.feed_behavior),
                boat_behavior: None,
            },
            ActionDefinition {
                id: "boat".into(),
                label: "Лодка".into(),
                allowed_zone_id: "water".into(),
                effect: InteractionEffect::Threat,
                rule: rules.boat,
                feed_behavior: None,
                boat_behavior: Some(rules.boat_behavior),
            },
            ActionDefinition {
                id: "feed-slow".into(),
                label: "Медленный корм".into(),
                allowed_zone_id: "water".into(),
                effect: InteractionEffect::Attraction,
                rule: slow_feed,
                feed_behavior: Some(slow_behavior),
                boat_behavior: None,
            },
            ActionDefinition {
                id: "boat-small".into(),
                label: "Малая лодка".into(),
                allowed_zone_id: "water".into(),
                effect: InteractionEffect::Threat,
                rule: small_boat,
                feed_behavior: None,
                boat_behavior: Some(small_behavior),
            },
        ]
    }

    #[test]
    fn catalog_feed_uses_own_behavior_and_shared_limit_after_restart() {
        let rules = WorldInteractionRules::default();
        let mut world =
            World::new_with_catalog(bounds(), 77, rules, action_catalog(rules)).unwrap();
        world.spawn_fish(1, Point { x: -0.5, y: 0.0 }, 1.0).unwrap();
        world.spawn_fish(2, Point { x: 0.5, y: 0.0 }, 1.0).unwrap();
        let id = "00000000000000000000000000000001";
        world
            .start_action("feed-slow", id, Point { x: 0.0, y: 0.0 })
            .unwrap();
        assert_eq!(world.feed_sources()[0].interaction_id, "feed-slow");
        assert_eq!(world.feed_sources()[0].expires_at_tick, 40);
        world.step();
        let assigned: Vec<_> = world
            .fish()
            .iter()
            .filter(|fish| fish.feeding.is_some())
            .collect();
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].depth_target, -0.8);
        let mut outside =
            World::new_with_catalog(bounds(), 78, rules, action_catalog(rules)).unwrap();
        outside
            .spawn_fish(3, Point { x: 2.5, y: 0.0 }, 1.0)
            .unwrap();
        outside
            .start_action(
                "feed-slow",
                "00000000000000000000000000000005",
                Point { x: 0.0, y: 0.0 },
            )
            .unwrap();
        outside.step();
        assert!(outside.fish()[0].feeding.is_none());
        let mut replay = World::restore(world.checkpoint()).unwrap();
        assert_eq!(replay.action_definitions(), world.action_definitions());
        for _ in 0..20 {
            world.step();
            replay.step();
            assert_eq!(world.checkpoint(), replay.checkpoint());
        }
        world
            .start_action(
                "feed",
                "00000000000000000000000000000002",
                Point { x: 2.0, y: 0.0 },
            )
            .unwrap();
        world
            .start_action(
                "feed-slow",
                "00000000000000000000000000000003",
                Point { x: -2.0, y: 0.0 },
            )
            .unwrap();
        assert_eq!(
            world.start_action(
                "feed",
                "00000000000000000000000000000004",
                Point { x: 3.0, y: 0.0 }
            ),
            Err(SimError::FeedLimit)
        );
        assert!(world.cancel_feed(id));
        assert!(world.feed_sources().iter().all(|source| source.id != id));
    }

    #[test]
    fn catalog_threat_uses_own_behavior_and_cancels_after_restart() {
        let rules = WorldInteractionRules::default();
        let mut world =
            World::new_with_catalog(bounds(), 19, rules, action_catalog(rules)).unwrap();
        world.spawn_fish(1, Point { x: -5.0, y: 0.0 }, 1.0).unwrap();
        world.spawn_fish(2, Point { x: -4.8, y: 0.0 }, 1.0).unwrap();
        let id = "00000000000000000000000000000001";
        world
            .start_action("boat-small", id, Point { x: 1.0, y: 0.0 })
            .unwrap();
        assert_eq!(world.boat().unwrap().interaction_id, "boat-small");
        assert_eq!(world.boat().unwrap().expires_at_tick, 80);
        assert_eq!(
            world.start_action(
                "boat",
                "00000000000000000000000000000002",
                Point { x: 1.0, y: 0.0 }
            ),
            Err(SimError::BoatLimit)
        );
        world.step();
        assert_eq!(world.fish().iter().filter(|fish| fish.fleeing).count(), 1);
        assert_eq!(
            world
                .fish()
                .iter()
                .find(|fish| fish.fleeing)
                .unwrap()
                .depth_target,
            -0.8
        );
        let mut replay = World::restore(world.checkpoint()).unwrap();
        for _ in 0..20 {
            world.step();
            replay.step();
            assert_eq!(world.checkpoint(), replay.checkpoint());
        }
        assert!(replay.cancel_boat(id));
        assert!(replay.boat().is_none());
        assert!(replay.fish().iter().all(|fish| !fish.fleeing));
    }

    #[test]
    fn catalog_rejects_unknown_or_corrupt_definitions_and_old_checkpoint_restores() {
        let rules = WorldInteractionRules::default();
        let catalog = action_catalog(rules);
        let mut world = World::new_with_catalog(bounds(), 7, rules, catalog.clone()).unwrap();
        assert_eq!(
            world.start_action(
                "feed-other",
                "00000000000000000000000000000001",
                Point { x: 0.0, y: 0.0 }
            ),
            Err(SimError::UnknownInteraction)
        );
        world
            .start_action(
                "feed-slow",
                "00000000000000000000000000000001",
                Point { x: 0.0, y: 0.0 },
            )
            .unwrap();
        let mut corrupt = world.checkpoint();
        corrupt.feed_sources[0].interaction_id = "boat".into();
        assert_eq!(
            World::restore(corrupt).err(),
            Some(SimError::InvalidCheckpoint)
        );
        let mut corrupt = world.checkpoint();
        corrupt.action_definitions.push(catalog[2].clone());
        assert_eq!(
            World::restore(corrupt).err(),
            Some(SimError::InvalidCheckpoint)
        );
        let mut corrupt = catalog;
        corrupt[2].rule.priority = rules.boat.priority;
        assert_eq!(
            World::new_with_catalog(bounds(), 7, rules, corrupt).err(),
            Some(SimError::InvalidInteractionRules)
        );
        let mut legacy = World::new(bounds(), 7).unwrap();
        legacy
            .start_feed("00000000000000000000000000000001", Point { x: 0.0, y: 0.0 })
            .unwrap();
        legacy
            .start_boat("00000000000000000000000000000002", Point { x: 1.0, y: 0.0 })
            .unwrap();
        let mut old = serde_json::to_value(legacy.checkpoint()).unwrap();
        old.as_object_mut().unwrap().remove("action_definitions");
        old["feed_sources"][0]
            .as_object_mut()
            .unwrap()
            .remove("interaction_id");
        old["boat"]
            .as_object_mut()
            .unwrap()
            .remove("interaction_id");
        let restored = World::restore(serde_json::from_value(old).unwrap()).unwrap();
        assert!(restored.action_definitions().is_empty());
        assert_eq!(restored.feed_sources()[0].interaction_id, "feed");
        assert_eq!(restored.boat().unwrap().interaction_id, "boat");
    }

    #[test]
    fn one_hundred_fish_stay_inside_water_and_replay_deterministically() {
        let mut world = World::new(bounds(), 42).unwrap();
        world
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 0.8,
            })
            .unwrap();
        for index in 0..100 {
            let col = index % 10;
            let row = index / 10;
            let position = Point {
                x: -6.8 + col as f32 * 1.4,
                y: -3.5 + row as f32 * 0.7,
            };
            if point_blocked(position, world.obstacles()) {
                world
                    .spawn_fish(
                        index as u128,
                        Point {
                            x: position.x,
                            y: 2.5,
                        },
                        1.0,
                    )
                    .unwrap();
            } else {
                world.spawn_fish(index as u128, position, 1.0).unwrap();
            }
        }
        assert_eq!(
            world.spawn_fish(101, Point { x: 1.0, y: 1.0 }, 1.0),
            Err(SimError::FishLimit)
        );
        let mut replay = world.clone();
        for _ in 0..200 {
            world.step();
            replay.step();
            assert_eq!(world.fish(), replay.fish());
            for fish in world.fish() {
                assert!(bounds().contains(fish.position, FISH_RADIUS));
                assert!(!point_blocked(fish.position, world.obstacles()));
            }
        }
        assert_eq!(world.tick_number(), 200);
    }

    #[test]
    fn fish_swim_in_depth_with_a_bounded_three_dimensional_step() {
        let mut world = World::new(bounds(), 42).unwrap();
        world.spawn_fish(1, Point { x: 0.0, y: 0.0 }, 1.2).unwrap();
        let mut seen_near = false;
        let mut seen_far = false;
        for _ in 0..1200 {
            let before = world.fish()[0].clone();
            world.step();
            let after = &world.fish()[0];
            let displacement = ((after.position.x - before.position.x).powi(2)
                + (after.position.y - before.position.y).powi(2)
                + (after.depth - before.depth).powi(2))
            .sqrt();
            assert!(displacement <= after.speed / TICKS_PER_SECOND as f32 + 0.00001);
            assert!((MIN_DEPTH..=MAX_DEPTH).contains(&after.depth));
            seen_near |= after.depth < -0.5;
            seen_far |= after.depth > 0.5;
        }
        assert!(seen_near && seen_far, "fish should visit both depth layers");
    }

    #[test]
    fn idle_pace_varies_independently_and_smoothly() {
        let speeds: Vec<_> = (0..2000)
            .map(|tick| cruise_speed_factor(42, 1, tick))
            .collect();
        let other: Vec<_> = (0..2000)
            .map(|tick| cruise_speed_factor(42, 2, tick))
            .collect();
        let slow = speeds.iter().copied().fold(f32::INFINITY, f32::min);
        let fast = speeds.iter().copied().fold(0.0, f32::max);
        assert!(slow >= 0.38 && fast <= 1.0 && fast - slow > 0.3);
        assert!(
            speeds
                .windows(2)
                .all(|pair| (pair[1] - pair[0]).abs() < 0.02)
        );
        assert!(
            speeds
                .iter()
                .zip(other)
                .any(|(one, two)| (one - two).abs() > 0.2)
        );
    }

    #[test]
    fn exploration_pauses_then_resumes_after_checkpoint() {
        let mut world = World::new(bounds(), 42).unwrap();
        world.spawn_fish(1, Point { x: 0.0, y: 0.0 }, 1.2).unwrap();
        for _ in 0..3000 {
            world.step();
            if matches!(
                world.fish()[0].ambient.as_ref(),
                Some(AmbientBehavior::Explore { .. })
            ) {
                break;
            }
        }
        assert!(matches!(
            world.fish()[0].ambient.as_ref(),
            Some(AmbientBehavior::Explore { .. })
        ));
        let original_until = world.fish()[0].ambient.as_ref().unwrap().until_tick();
        let resting = world.fish()[0].position;
        let mut restored = World::restore(world.checkpoint()).unwrap();
        for _ in 0..10 {
            world.step();
            restored.step();
            assert_eq!(world.fish(), restored.fish());
            assert_eq!(
                world.fish()[0].position,
                resting,
                "fish must pause before exploring"
            );
        }
        let mut moved_vertically = false;
        let mut resumed = false;
        for _ in 0..280 {
            world.step();
            restored.step();
            assert_eq!(world.fish(), restored.fish());
            moved_vertically |= (world.fish()[0].position.y - resting.y).abs() > 0.3;
            resumed |= world.tick >= original_until
                && world.fish()[0]
                    .ambient
                    .as_ref()
                    .is_none_or(|state| state.until_tick() != original_until);
        }
        assert!(moved_vertically);
        assert!(
            resumed,
            "the original investigation must end and release its goal"
        );
    }

    #[test]
    fn a_close_approach_startles_one_fish_then_expires() {
        let mut world = World::new(bounds(), 0).unwrap();
        world
            .spawn_fish(1, Point { x: -0.35, y: 0.0 }, 1.2)
            .unwrap();
        world.spawn_fish(2, Point { x: 0.35, y: 0.0 }, 1.2).unwrap();
        world.tick = 119;
        world.step();
        assert_eq!(
            world
                .fish()
                .iter()
                .filter(|fish| matches!(
                    fish.ambient.as_ref(),
                    Some(AmbientBehavior::Startled { .. })
                ))
                .count(),
            1
        );
        let mut restored = World::restore(world.checkpoint()).unwrap();
        for _ in 0..65 {
            world.step();
            restored.step();
            assert_eq!(world.fish(), restored.fish());
        }
        assert!(world.fish().iter().all(|fish| !matches!(
            fish.ambient.as_ref(),
            Some(AmbientBehavior::Startled { .. })
        )));
    }

    #[test]
    fn fish_pass_close_without_crossing_through_each_others_bodies() {
        let mut world = World::new(bounds(), 41).unwrap();
        world.spawn_fish(1, Point { x: -2.4, y: 0.0 }, 1.8).unwrap();
        world.spawn_fish(2, Point { x: 2.4, y: 0.0 }, 1.8).unwrap();
        world.fish[0].target = Point { x: 3.0, y: 0.0 };
        world.fish[1].target = Point { x: -3.0, y: 0.0 };
        world.fish[0].depth_target = 0.0;
        world.fish[1].depth_target = 0.0;
        world.fish[1].heading = Point { x: -1.0, y: 0.0 };
        let mut closest = f32::MAX;
        let mut moving = [0; 2];
        for _ in 0..160 {
            let before = [world.fish()[0].position, world.fish()[1].position];
            world.step();
            let gap = body_gap(
                BodyPose::from(&world.fish()[0]),
                BodyPose::from(&world.fish()[1]),
            );
            assert!(gap >= -0.35, "fish bodies deeply overlapped by {gap}");
            closest = closest.min(gap);
            for (index, previous) in before.iter().enumerate() {
                if world.fish()[index].position.distance_squared(*previous) > 0.000025 {
                    moving[index] += 1;
                }
            }
        }
        assert!(closest < 0.4, "the fish should pass close to each other");
        assert!(
            moving.iter().all(|ticks| *ticks > 80),
            "neither fish may wait for the other"
        );
        assert!(world.fish()[0].position.x > 0.0);
        assert!(world.fish()[1].position.x < 0.0);
    }

    #[test]
    fn feeding_preempts_exploration_and_removing_a_peer_ends_an_approach() {
        let mut world = World::new(bounds(), 17).unwrap();
        world.spawn_fish(1, Point { x: 0.0, y: 0.0 }, 1.2).unwrap();
        world.spawn_fish(2, Point { x: 1.0, y: 0.0 }, 1.2).unwrap();
        let resume = world.fish()[0].target;
        let resume_depth = world.fish()[0].depth_target;
        world.fish[0].ambient = Some(AmbientBehavior::Explore {
            until_tick: 100,
            rest_until_tick: 10,
            resume_target: resume,
            resume_depth,
        });
        world
            .start_feed("00000000000000000000000000000001", Point { x: 0.0, y: 0.0 })
            .unwrap();
        world.step();
        assert!(
            world.fish()[0].ambient.is_none(),
            "food must interrupt exploration"
        );
        world.cancel_feed("00000000000000000000000000000001");
        let resume = world.fish()[0].target;
        let resume_depth = world.fish()[0].depth_target;
        world.fish[0].ambient = Some(AmbientBehavior::Approach {
            until_tick: world.tick + 100,
            peer_id: fish_id(2),
            resume_target: resume,
            resume_depth,
        });
        world.remove_fish(2).unwrap();
        assert!(world.fish()[0].ambient.is_none());
        World::restore(world.checkpoint()).unwrap();
    }

    #[test]
    fn fish_must_reach_feed_depth_before_eating() {
        let mut world = World::new(bounds(), 31).unwrap();
        world.spawn_fish(1, Point { x: 0.0, y: 0.0 }, 1.2).unwrap();
        let mut checkpoint = world.checkpoint();
        checkpoint.fish[0].depth = 1.2;
        checkpoint.fish[0].depth_target = 1.2;
        world = World::restore(checkpoint).unwrap();
        world
            .start_feed("000000000000000000000000000000ab", Point { x: 0.0, y: 0.0 })
            .unwrap();
        let portions = world.feed_sources()[0].remaining;
        world.step();
        assert_eq!(world.feed_sources()[0].remaining, portions);
        let mut ate = false;
        for _ in 0..100 {
            world.step();
            if world.feed_sources()[0].remaining < portions {
                let fish = &world.fish()[0];
                assert!(fish.feeding.is_none());
                assert_eq!(fish.target, fish.position);
                assert_eq!(fish.depth_target, fish.depth);
                ate = true;
                break;
            }
        }
        assert!(ate, "the fish should reach and consume one portion");
        assert_eq!(world.feed_sources()[0].remaining, portions - 1);
    }

    #[test]
    fn fish_routes_around_a_circular_obstacle() {
        let mut world = World::new(bounds(), 7).unwrap();
        world
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 1.0,
            })
            .unwrap();
        world.spawn_fish(2, Point { x: -3.0, y: 0.0 }, 1.5).unwrap();
        world.set_target(2, Point { x: 3.0, y: 0.0 }).unwrap();
        let mut reached_far_side = false;
        for _ in 0..240 {
            world.step();
            assert!(!point_blocked(world.fish()[0].position, world.obstacles()));
            reached_far_side |= world.fish()[0].position.x > 1.0;
        }
        assert!(
            reached_far_side,
            "fish should pass the obstacle instead of orbiting its near side"
        );
    }

    #[test]
    fn rejects_invalid_geometry_and_duplicate_ids() {
        let mut world = World::new(bounds(), 1).unwrap();
        assert_eq!(
            world.spawn_fish(1, Point { x: 99.0, y: 0.0 }, 1.0),
            Err(SimError::InvalidPosition)
        );
        assert_eq!(
            world.spawn_fish(1, Point { x: 0.0, y: 0.0 }, 0.0),
            Err(SimError::InvalidSpeed)
        );
        world.spawn_fish(1, Point { x: 0.0, y: 0.0 }, 1.0).unwrap();
        assert_eq!(
            world.spawn_fish(1, Point { x: 1.0, y: 0.0 }, 1.0),
            Err(SimError::DuplicateId)
        );
        assert_eq!(
            world.add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 0.5
            }),
            Err(SimError::InvalidObstacle)
        );
    }

    #[test]
    fn checkpoint_roundtrip_preserves_route_and_rejects_corruption() {
        let mut original = World::new(bounds(), 91).unwrap();
        original
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 1.0,
            })
            .unwrap();
        let fish_id = u64::MAX as u128 + 7;
        original
            .spawn_fish(fish_id, Point { x: -3.0, y: 0.0 }, 1.2)
            .unwrap();
        original
            .set_target(fish_id, Point { x: 3.0, y: 0.0 })
            .unwrap();
        for _ in 0..30 {
            original.step();
        }
        let saved = serde_json::to_vec(&original.checkpoint()).unwrap();
        let decoded: WorldCheckpoint = serde_json::from_slice(&saved).unwrap();
        let through_json_value: WorldCheckpoint =
            serde_json::from_value(serde_json::to_value(original.checkpoint()).unwrap()).unwrap();
        assert_eq!(through_json_value.fish[0].id, fish_id);
        let mut previous_format = serde_json::to_value(original.checkpoint()).unwrap();
        previous_format
            .as_object_mut()
            .unwrap()
            .remove("feed_sources");
        previous_format.as_object_mut().unwrap().remove("boat");
        previous_format["fish"][0]
            .as_object_mut()
            .unwrap()
            .remove("feeding");
        previous_format["fish"][0]
            .as_object_mut()
            .unwrap()
            .remove("fleeing");
        for field in ["depth", "depth_target", "heading_depth"] {
            previous_format["fish"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
        }
        let old: WorldCheckpoint = serde_json::from_value(previous_format).unwrap();
        assert!(World::restore(old).unwrap().feed_sources().is_empty());
        let mut restored = World::restore(decoded.clone()).unwrap();
        assert_eq!(restored.tick_number(), 30);
        for _ in 0..100 {
            original.step();
            restored.step();
            assert_eq!(original.checkpoint(), restored.checkpoint());
        }
        let mut corrupted = decoded.clone();
        corrupted.fish.push(decoded.fish[0].clone());
        assert!(matches!(
            World::restore(corrupted),
            Err(SimError::InvalidCheckpoint)
        ));
        let mut corrupted = decoded;
        corrupted.fish[0].position = Point { x: 99.0, y: 0.0 };
        assert!(matches!(
            World::restore(corrupted),
            Err(SimError::InvalidCheckpoint)
        ));
    }

    #[test]
    fn feed_assigns_nearby_fish_consumes_once_and_replays_after_checkpoint() {
        let mut world = World::new(bounds(), 31).unwrap();
        world.spawn_fish(1, Point { x: -2.0, y: 0.0 }, 1.2).unwrap();
        world.spawn_fish(2, Point { x: 2.0, y: 0.0 }, 1.2).unwrap();
        world.spawn_fish(3, Point { x: 6.0, y: 0.0 }, 1.2).unwrap();
        let id = "000000000000000000000000000000ab";
        world.start_feed(id, Point { x: 0.0, y: 0.0 }).unwrap();
        assert_eq!(
            world.start_feed(id, Point { x: 0.0, y: 0.0 }),
            Err(SimError::DuplicateFeed)
        );
        for _ in 0..10 {
            world.step();
        }
        assert!(world.fish()[0].feeding.is_some());
        assert!(world.fish()[1].feeding.is_some());
        assert_eq!(world.fish()[2].feeding, None);
        let encoded = serde_json::to_vec(&world.checkpoint()).unwrap();
        let mut recovered = World::restore(serde_json::from_slice(&encoded).unwrap()).unwrap();
        for _ in 0..90 {
            world.step();
            recovered.step();
            assert_eq!(world.checkpoint(), recovered.checkpoint());
        }
        assert_eq!(world.feed_sources()[0].remaining, 8);
        assert_eq!(world.feed_sources()[0].fed_fish.len(), 2);
        for _ in 0..200 {
            world.step();
        }
        assert!(world.feed_sources().is_empty());
        assert!(world.fish().iter().all(|fish| fish.feeding.is_none()));
    }

    #[test]
    fn removed_fish_leaves_valid_active_actions_and_cannot_eat_twice_after_restore() {
        let mut world = World::new(bounds(), 31).unwrap();
        let fish_key = 0xab;
        world
            .spawn_fish(fish_key, Point { x: -1.0, y: 0.0 }, 1.2)
            .unwrap();
        let feed_id = "000000000000000000000000000000ab";
        world.start_feed(feed_id, Point { x: 0.0, y: 0.0 }).unwrap();
        for _ in 0..150 {
            world.step();
            if world.feed_sources()[0]
                .fed_fish
                .contains(&fish_id(fish_key))
            {
                break;
            }
        }
        assert_eq!(world.feed_sources()[0].remaining, 9);
        let removed = world.remove_fish(fish_key).unwrap();
        assert_eq!(removed.id, fish_key);
        assert_eq!(world.remove_fish(fish_key), Err(SimError::UnknownFish));
        let mut restored = World::restore(world.checkpoint()).unwrap();
        assert!(restored.fish().is_empty());
        assert_eq!(restored.feed_sources()[0].remaining, 9);
        restored
            .spawn_fish_with_capabilities(
                removed.id,
                removed.position,
                removed.speed,
                removed.capabilities,
            )
            .unwrap();
        assert!(restored.fish()[0].feeding.is_none());
        assert!(!restored.fish()[0].fleeing);
        for _ in 0..20 {
            restored.step();
        }
        assert_eq!(restored.feed_sources()[0].remaining, 9);
        for _ in 0..280 {
            restored.step();
        }
        assert!(restored.feed_sources().is_empty());
        assert_eq!(restored.fish().len(), 1);
    }

    #[test]
    fn removing_fish_during_boat_preserves_checkpoint_and_reuse_obeys_capacity() {
        let mut world = World::new(bounds(), 44).unwrap();
        for id in 1..=MAX_FISH as u128 {
            world.spawn_fish(id, Point { x: 0.0, y: 0.0 }, 1.2).unwrap();
        }
        let boat_id = "000000000000000000000000000000bc";
        world.start_boat(boat_id, Point { x: 0.0, y: 0.0 }).unwrap();
        for _ in 0..10 {
            world.step();
        }
        let removed = world.remove_fish(1).unwrap();
        assert_eq!(world.fish().len(), MAX_FISH - 1);
        let mut restored = World::restore(world.checkpoint()).unwrap();
        assert!(restored.boat().is_some());
        restored
            .spawn_fish(101, Point { x: 0.0, y: 0.0 }, 1.2)
            .unwrap();
        assert_eq!(
            restored.spawn_fish_with_capabilities(
                removed.id,
                removed.position,
                removed.speed,
                removed.capabilities,
            ),
            Err(SimError::FishLimit)
        );
        restored.remove_fish(101).unwrap();
        restored
            .spawn_fish_with_capabilities(
                removed.id,
                removed.position,
                removed.speed,
                removed.capabilities,
            )
            .unwrap();
        assert_eq!(restored.fish().len(), MAX_FISH);
        assert!(World::restore(restored.checkpoint()).is_ok());
    }

    #[test]
    fn feed_rejects_invalid_points_enforces_three_sources_and_expires_without_fish() {
        let mut world = World::new(bounds(), 2).unwrap();
        for index in 0..3 {
            world
                .start_feed(
                    &format!("{index:032x}"),
                    Point {
                        x: index as f32,
                        y: 0.0,
                    },
                )
                .unwrap();
        }
        assert_eq!(
            world.start_feed("00000000000000000000000000000004", Point { x: 3.0, y: 0.0 }),
            Err(SimError::FeedLimit)
        );
        assert_eq!(
            world.start_feed("invalid", Point { x: 0.0, y: 0.0 }),
            Err(SimError::InvalidFeedId)
        );
        assert_eq!(
            world.start_feed(
                "00000000000000000000000000000005",
                Point { x: 99.0, y: 0.0 }
            ),
            Err(SimError::InvalidPosition)
        );
        for _ in 0..world.interaction_rules().feed.duration_ticks {
            world.step();
        }
        assert!(world.feed_sources().is_empty());
        world
            .start_feed("00000000000000000000000000000004", Point { x: 0.0, y: 0.0 })
            .unwrap();
    }

    #[test]
    fn feeding_fish_routes_around_obstacle_without_crossing_it() {
        let mut world = World::new(bounds(), 11).unwrap();
        world
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 0.8,
            })
            .unwrap();
        world.spawn_fish(7, Point { x: -1.5, y: 0.0 }, 1.5).unwrap();
        world
            .start_feed("00000000000000000000000000000001", Point { x: 1.5, y: 0.0 })
            .unwrap();
        for _ in 0..180 {
            world.step();
            assert!(!point_blocked(world.fish()[0].position, world.obstacles()));
        }
        assert_eq!(world.feed_sources()[0].remaining, 9);
    }

    #[test]
    fn cancelling_feed_releases_only_its_fish_assignments() {
        let mut world = World::new(bounds(), 31).unwrap();
        world.spawn_fish(7, Point { x: -1.0, y: 0.0 }, 1.0).unwrap();
        let first = "00000000000000000000000000000001";
        let second = "00000000000000000000000000000002";
        world.start_feed(first, Point { x: 1.0, y: 0.0 }).unwrap();
        world.start_feed(second, Point { x: 5.0, y: 0.0 }).unwrap();
        world.step();
        assert_eq!(world.fish()[0].feeding.as_deref(), Some(first));
        assert!(!world.cancel_feed("00000000000000000000000000000003"));
        assert!(world.cancel_feed(first));
        assert_eq!(world.fish()[0].feeding, None);
        assert_eq!(world.fish()[0].target, world.fish()[0].position);
        assert_eq!(world.fish()[0].depth_target, world.fish()[0].depth);
        assert_eq!(world.feed_sources().len(), 1);
        assert_eq!(world.feed_sources()[0].id, second);
        assert!(!world.cancel_feed(first));
        World::restore(world.checkpoint()).unwrap();
    }

    #[test]
    fn expiring_feed_releases_the_fish_route_and_depth_goal() {
        let mut rules = WorldInteractionRules::default();
        rules.feed.duration_ticks = 2;
        let mut world = World::new_with_rules(bounds(), 31, rules).unwrap();
        world.spawn_fish(7, Point { x: -2.0, y: 0.0 }, 1.0).unwrap();
        world
            .start_feed("00000000000000000000000000000001", Point { x: 1.0, y: 0.0 })
            .unwrap();
        world.step();
        let generation = world.fish()[0].target_generation;
        assert!(world.fish()[0].feeding.is_some());
        world.step();
        assert!(world.feed_sources().is_empty());
        assert!(world.fish()[0].feeding.is_none());
        assert_eq!(
            world.fish()[0].target_generation,
            generation + 1,
            "ordinary swimming must replace the expired feeding route immediately"
        );
        assert_ne!(world.fish()[0].depth_target, 0.0);
    }

    #[test]
    fn boat_preempts_feed_then_expiry_restores_feeding_after_checkpoint() {
        let mut rules = WorldInteractionRules::default();
        rules.boat.duration_ticks = 3;
        let mut world = World::new_with_rules(bounds(), 57, rules).unwrap();
        world.spawn_fish(7, Point { x: 6.8, y: 0.0 }, 1.0).unwrap();
        let feed = "00000000000000000000000000000001";
        world.start_feed(feed, Point { x: 5.4, y: 0.0 }).unwrap();
        world.step();
        assert_eq!(world.fish()[0].feeding.as_deref(), Some(feed));
        world
            .start_boat(
                "00000000000000000000000000000002",
                Point { x: -0.1, y: 0.0 },
            )
            .unwrap();
        world.step();
        assert!(world.fish()[0].fleeing);
        assert!(world.fish()[0].feeding.is_none());
        world.step();
        let mut restored = World::restore(world.checkpoint()).unwrap();
        let mut cancelled = World::restore(world.checkpoint()).unwrap();
        assert!(cancelled.cancel_boat("00000000000000000000000000000002"));
        assert!(!cancelled.fish()[0].fleeing);
        assert_eq!(cancelled.fish()[0].target, cancelled.fish()[0].position);
        cancelled.step();
        assert_eq!(cancelled.fish()[0].feeding.as_deref(), Some(feed));
        world.step();
        restored.step();
        assert_eq!(world.checkpoint(), restored.checkpoint());
        assert!(world.boat().is_none());
        assert!(!world.fish()[0].fleeing);
        assert_eq!(
            world.fish()[0].feeding.as_deref(),
            Some(feed),
            "the fish must resume active food instead of its obsolete escape route"
        );
    }

    #[test]
    fn feed_and_threat_select_only_capable_fish_after_restart() {
        let mut world = World::new(bounds(), 57).unwrap();
        world
            .spawn_fish_with_capabilities(
                7,
                Point { x: 6.8, y: 0.0 },
                1.0,
                FishCapabilities {
                    consume_food: true,
                    avoid_threat: false,
                },
            )
            .unwrap();
        world
            .spawn_fish_with_capabilities(
                8,
                Point { x: 6.4, y: 0.5 },
                1.0,
                FishCapabilities {
                    consume_food: false,
                    avoid_threat: true,
                },
            )
            .unwrap();
        world
            .spawn_fish_with_capabilities(
                9,
                Point { x: 6.0, y: -0.5 },
                1.0,
                FishCapabilities {
                    consume_food: false,
                    avoid_threat: false,
                },
            )
            .unwrap();
        let feed = "00000000000000000000000000000001";
        world.start_feed(feed, Point { x: 5.4, y: 0.0 }).unwrap();
        world.step();
        assert_eq!(world.fish()[0].feeding.as_deref(), Some(feed));
        assert!(world.fish()[1..].iter().all(|fish| fish.feeding.is_none()));
        world
            .start_boat(
                "00000000000000000000000000000002",
                Point { x: -0.1, y: 0.0 },
            )
            .unwrap();
        world.step();
        assert!(!world.fish()[0].fleeing);
        assert_eq!(world.fish()[0].feeding.as_deref(), Some(feed));
        assert!(world.fish()[1].fleeing);
        assert!(!world.fish()[2].fleeing);
        let mut replay = World::restore(world.checkpoint()).unwrap();
        for _ in 0..30 {
            world.step();
            replay.step();
            assert_eq!(world.checkpoint(), replay.checkpoint());
        }
        let mut legacy = serde_json::to_value(world.checkpoint()).unwrap();
        legacy["fish"][0]
            .as_object_mut()
            .unwrap()
            .remove("capabilities");
        let restored = World::restore(serde_json::from_value(legacy).unwrap()).unwrap();
        assert_eq!(restored.fish()[0].capabilities, FishCapabilities::default());
    }

    #[test]
    fn interaction_rules_change_attraction_and_survive_checkpoint() {
        let mut rules = WorldInteractionRules::default();
        rules.feed.definition_version = 2;
        rules.feed.radius = 4.0;
        rules.feed.duration_ticks = 40;
        rules.feed.max_active = 1;
        rules.boat.radius = 3.2;
        rules.boat.duration_ticks = 80;
        let mut configured = World::new_with_rules(bounds(), 77, rules).unwrap();
        let mut legacy = World::new(bounds(), 77).unwrap();
        for world in [&mut configured, &mut legacy] {
            world.spawn_fish(7, Point { x: -2.0, y: 0.0 }, 1.0).unwrap();
            world
                .start_feed("00000000000000000000000000000001", Point { x: 2.0, y: 0.0 })
                .unwrap();
            world.step();
        }
        assert_eq!(
            configured.fish()[0].feeding.as_deref(),
            Some("00000000000000000000000000000001")
        );
        assert!(legacy.fish()[0].feeding.is_none());
        assert_eq!(configured.feed_sources()[0].expires_at_tick, 40);
        assert_eq!(
            configured.start_feed("00000000000000000000000000000002", Point { x: 0.0, y: 0.0 }),
            Err(SimError::FeedLimit)
        );
        let checkpoint = configured.checkpoint();
        assert_eq!(
            World::restore(checkpoint).unwrap().interaction_rules(),
            rules
        );
        let mut previous_rules = serde_json::to_value(configured.checkpoint()).unwrap();
        previous_rules["interaction_rules"]
            .as_object_mut()
            .unwrap()
            .remove("feed_behavior");
        previous_rules["interaction_rules"]
            .as_object_mut()
            .unwrap()
            .remove("boat_behavior");
        let restored = World::restore(serde_json::from_value(previous_rules).unwrap()).unwrap();
        assert_eq!(
            restored.interaction_rules().feed_behavior,
            FeedBehavior::default()
        );
        assert_eq!(
            restored.interaction_rules().boat_behavior,
            BoatBehavior::default()
        );
        let mut old = serde_json::to_value(legacy.checkpoint()).unwrap();
        old.as_object_mut().unwrap().remove("interaction_rules");
        let restored = World::restore(serde_json::from_value(old).unwrap()).unwrap();
        assert_eq!(
            restored.interaction_rules(),
            WorldInteractionRules::default()
        );
        rules.feed.radius = f32::NAN;
        assert_eq!(
            World::new_with_rules(bounds(), 77, rules).unwrap_err(),
            SimError::InvalidInteractionRules
        );
    }

    #[test]
    fn bounded_threat_behavior_limits_candidates_and_survives_checkpoint() {
        let mut rules = WorldInteractionRules::default();
        rules.boat_behavior.candidate_limit = 1;
        rules.boat_behavior.hold_ticks = 2;
        rules.boat_behavior.release_radius_factor = 1.1;
        rules.boat_behavior.escape_depth = -0.8;
        let mut world = World::new_with_rules(bounds(), 19, rules).unwrap();
        world.spawn_fish(1, Point { x: -5.0, y: 0.0 }, 1.0).unwrap();
        world.spawn_fish(2, Point { x: -4.8, y: 0.0 }, 1.0).unwrap();
        world
            .start_boat("00000000000000000000000000000001", Point { x: 1.0, y: 0.0 })
            .unwrap();
        world.step();
        assert!(world.fish()[0].fleeing);
        assert_eq!(world.fish()[0].depth_target, -0.8);
        assert!(!world.fish()[1].fleeing);
        let mut replay = World::restore(world.checkpoint()).unwrap();
        for _ in 0..20 {
            world.step();
            replay.step();
            assert_eq!(world.checkpoint(), replay.checkpoint());
        }
        rules.boat_behavior.candidate_limit = 0;
        assert_eq!(
            World::new_with_rules(bounds(), 19, rules).unwrap_err(),
            SimError::InvalidInteractionRules
        );
    }

    #[test]
    fn boat_visits_selected_point_exits_without_teleporting_and_fish_flee() {
        let mut world = World::new(bounds(), 15).unwrap();
        world.spawn_fish(2, Point { x: -5.0, y: 0.0 }, 1.2).unwrap();
        world
            .start_feed(
                "00000000000000000000000000000001",
                Point { x: -3.0, y: 0.0 },
            )
            .unwrap();
        let boat_id = "00000000000000000000000000000002";
        world.start_boat(boat_id, Point { x: 1.0, y: 0.0 }).unwrap();
        let mut last = world.boat().unwrap().position;
        let mut visited = false;
        let mut saw_fleeing = false;
        for _ in 0..200 {
            world.step();
            let fish = &world.fish()[0];
            assert!(bounds().contains(fish.position, FISH_RADIUS));
            if fish.fleeing {
                saw_fleeing = true;
                assert!(fish.feeding.is_none());
            }
            if let Some(boat) = world.boat() {
                assert!(boat.position.distance_squared(last) <= 0.101_f32.powi(2));
                visited |= boat.position.distance_squared(boat.via) < 0.0001;
                last = boat.position;
            } else {
                break;
            }
        }
        assert!(visited);
        assert!(saw_fleeing);
        assert!(world.boat().is_none());
        assert!(!world.fish()[0].fleeing);
    }

    #[test]
    fn boat_rejects_blocked_route_and_enforces_one_active_boat() {
        let mut world = World::new(bounds(), 5).unwrap();
        world
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 0.8,
            })
            .unwrap();
        let id = "00000000000000000000000000000002";
        assert_eq!(
            world.start_boat(id, Point { x: 2.0, y: 0.0 }),
            Err(SimError::InvalidBoatRoute)
        );
        assert_eq!(
            world.start_boat("bad", Point { x: 2.0, y: 2.0 }),
            Err(SimError::InvalidBoatId)
        );
        world.start_boat(id, Point { x: 2.0, y: 2.0 }).unwrap();
        assert_eq!(
            world.start_boat("00000000000000000000000000000003", Point { x: 2.0, y: 2.0 }),
            Err(SimError::BoatLimit)
        );
        assert!(!world.cancel_boat("00000000000000000000000000000003"));
        assert!(world.cancel_boat(id));
        assert!(world.boat().is_none());
    }

    #[test]
    fn boat_checkpoint_replays_fish_response_and_rejects_corruption() {
        let mut world = World::new(bounds(), 23).unwrap();
        for index in 0..100 {
            let x = -6.5 + (index % 10) as f32 * 1.3;
            let y = -3.2 + (index / 10) as f32 * 0.7;
            world.spawn_fish(index, Point { x, y }, 1.0).unwrap();
        }
        world
            .start_boat("00000000000000000000000000000002", Point { x: 0.0, y: 0.0 })
            .unwrap();
        for _ in 0..35 {
            world.step();
        }
        assert!(world.fish().iter().any(|fish| fish.fleeing));
        let saved = serde_json::to_vec(&world.checkpoint()).unwrap();
        let checkpoint: WorldCheckpoint = serde_json::from_slice(&saved).unwrap();
        let mut replay = World::restore(checkpoint.clone()).unwrap();
        for _ in 0..160 {
            world.step();
            replay.step();
            assert_eq!(world.checkpoint(), replay.checkpoint());
            assert!(
                world
                    .fish()
                    .iter()
                    .all(|fish| bounds().contains(fish.position, FISH_RADIUS))
            );
        }
        let mut corrupted = checkpoint;
        corrupted.boat.as_mut().unwrap().position.x = 99.0;
        assert_eq!(
            World::restore(corrupted).err(),
            Some(SimError::InvalidCheckpoint)
        );
    }
}
