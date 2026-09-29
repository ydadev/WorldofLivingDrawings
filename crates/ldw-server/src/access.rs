use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("access denied")]
    Forbidden,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("admin already exists")]
    AlreadyInitialized,
    #[error("invalid account fields")]
    InvalidAccount,
    #[error("pairing rate limit reached")]
    RateLimited,
    #[error("controller limit reached")]
    ControllerLimit,
    #[error("viewer limit reached")]
    ViewerLimit,
    #[error("owner approval required")]
    OwnerApprovalRequired,
    #[error("cryptographic operation failed")]
    Crypto,
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct AccessStore {
    pool: PgPool,
    pin_key: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerGrant {
    pub account_id: Uuid,
    pub role: String,
    pub token: String,
    pub csrf: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIds {
    pub session_id: Uuid,
    pub scene_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairInvitation {
    pub session_id: Uuid,
    pub pin: String,
    pub qr_secret: String,
}

pub enum PairCode<'a> {
    Pin(&'a str),
    Qr(&'a str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerGrant {
    pub session_id: Uuid,
    pub participant_id: Uuid,
    pub token: String,
    pub csrf: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerGrant {
    pub session_id: Uuid,
    pub role: String,
    pub token: String,
    pub csrf: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneSummary {
    pub session_id: Uuid,
    pub scene_id: Uuid,
    pub world_id: String,
    pub world_version: i32,
    pub scene_epoch: i64,
    pub revision: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKind {
    Owner,
    Controller,
    Viewer,
}

#[derive(Debug, Clone)]
pub struct SceneAccess {
    pub scene: SceneSummary,
    pub grant_id: Uuid,
    pub role: String,
}

impl SceneAccess {
    pub fn may_interact(&self) -> bool {
        self.role != "viewer"
    }
}

impl AccessStore {
    pub fn new(pool: PgPool, pin_key: [u8; 32]) -> Self {
        Self { pool, pin_key }
    }

    pub async fn ping(&self) -> Result<(), AccessError> {
        let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&self.pool).await?;
        Ok(())
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Re-read on every command so an expired or revoked grant cannot keep acting
    /// merely because its WebSocket was already open.
    pub async fn scene_access(
        &self,
        kind: GrantKind,
        token: &str,
        session_id: Uuid,
    ) -> Result<SceneAccess, AccessError> {
        let token_hash = hash_token(token).to_vec();
        let row: Option<(Uuid, Uuid, String, i32, i64, i64, Uuid, String)> = match kind {
            GrantKind::Owner => sqlx::query_as(
                "SELECT s.id, c.id, c.world_id, c.world_version, c.scene_epoch, c.revision, g.id, a.role \
                 FROM owner_grants g JOIN accounts a ON a.id = g.account_id \
                 JOIN sessions s ON (s.owner_id = a.id OR a.role = 'admin') \
                 JOIN scenes c ON c.id = s.active_scene_id \
                 WHERE s.id = $1 AND g.token_hash = $2 AND g.revoked_at IS NULL \
                 AND g.expires_at > now() AND a.disabled_at IS NULL AND s.status != 'closed'",
            )
            .bind(session_id)
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await?,
            GrantKind::Controller | GrantKind::Viewer => {
                let requested_role = if kind == GrantKind::Controller { "controller" } else { "viewer" };
                sqlx::query_as(
                    "SELECT s.id, c.id, c.world_id, c.world_version, c.scene_epoch, c.revision, g.id, g.role \
                     FROM device_grants g JOIN sessions s ON s.id = g.session_id \
                     JOIN scenes c ON c.id = s.active_scene_id \
                     WHERE s.id = $1 AND g.token_hash = $2 \
                     AND (($3 = 'controller' AND g.role = 'controller') OR \
                          ($3 = 'viewer' AND g.role IN ('viewer', 'viewer_interact'))) \
                     AND g.revoked_at IS NULL AND g.expires_at > now() AND s.status != 'closed' \
                     AND (g.role != 'controller' OR g.last_activity_at > now() - interval '2 hours')"
                )
                    .bind(session_id)
                    .bind(token_hash)
                    .bind(requested_role)
                    .fetch_optional(&self.pool)
                    .await?
            }
        };
        let (session, scene, world, version, epoch, revision, grant_id, role) =
            row.ok_or(AccessError::Forbidden)?;
        Ok(SceneAccess {
            scene: SceneSummary {
                session_id: session,
                scene_id: scene,
                world_id: world,
                world_version: version,
                scene_epoch: epoch,
                revision,
            },
            grant_id,
            role,
        })
    }

    pub async fn check_socket_csrf(
        &self,
        kind: GrantKind,
        token: &str,
        csrf: &str,
        session_id: Uuid,
    ) -> Result<(), AccessError> {
        let found: Option<i32> = match kind {
            GrantKind::Owner => sqlx::query_scalar(
                "SELECT 1 FROM owner_grants g JOIN accounts a ON a.id = g.account_id \
                 JOIN sessions s ON (s.owner_id = a.id OR a.role = 'admin') \
                 WHERE s.id = $1 AND g.token_hash = $2 AND g.csrf_hash = $3 \
                 AND g.revoked_at IS NULL AND g.expires_at > now() AND a.disabled_at IS NULL AND s.status != 'closed'",
            )
            .bind(session_id)
            .bind(hash_token(token).to_vec())
            .bind(hash_token(csrf).to_vec())
            .fetch_optional(&self.pool)
            .await?,
            GrantKind::Controller | GrantKind::Viewer => {
                let requested_role = if kind == GrantKind::Controller { "controller" } else { "viewer" };
                sqlx::query_scalar(
                    "SELECT 1 FROM device_grants g JOIN sessions s ON s.id = g.session_id \
                     WHERE s.id = $1 AND g.token_hash = $2 AND g.csrf_hash = $3 \
                     AND (($4 = 'controller' AND g.role = 'controller') OR \
                          ($4 = 'viewer' AND g.role IN ('viewer', 'viewer_interact'))) \
                     AND g.revoked_at IS NULL AND g.expires_at > now() \
                     AND s.status != 'closed' \
                     AND (g.role != 'controller' OR g.last_activity_at > now() - interval '2 hours')"
                )
                    .bind(session_id)
                    .bind(hash_token(token).to_vec())
                    .bind(hash_token(csrf).to_vec())
                    .bind(requested_role)
                    .fetch_optional(&self.pool)
                    .await?
            }
        };
        found.ok_or(AccessError::Forbidden).map(|_| ())
    }

    /// Only a local bootstrap command may call this; it is never an HTTP route.
    pub async fn bootstrap_admin(&self, login: &str, password: &str) -> Result<Uuid, AccessError> {
        let hash = password_hash(login, password)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(757823)")
            .execute(&mut *tx)
            .await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin'")
            .fetch_one(&mut *tx)
            .await?;
        if count != 0 {
            return Err(AccessError::AlreadyInitialized);
        }
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'admin', $3)",
        )
        .bind(id)
        .bind(login)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn create_owner(
        &self,
        admin_token: &str,
        login: &str,
        password: &str,
    ) -> Result<Uuid, AccessError> {
        let (_, role) = self.owner_principal(admin_token).await?;
        if role != "admin" {
            return Err(AccessError::Forbidden);
        }
        let hash = password_hash(login, password)?;
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', $3)",
        )
        .bind(id)
        .bind(login)
        .bind(hash)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn login(&self, login: &str, password: &str) -> Result<OwnerGrant, AccessError> {
        let account: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, role, password_hash FROM accounts WHERE login = $1 AND disabled_at IS NULL",
        )
        .bind(login)
        .fetch_optional(&self.pool)
        .await?;
        let (account_id, role, stored_hash) = account.ok_or(AccessError::InvalidCredentials)?;
        let parsed = PasswordHash::new(&stored_hash).map_err(|_| AccessError::Crypto)?;
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| AccessError::InvalidCredentials)?;
        let token = random_token()?;
        let csrf = random_token()?;
        sqlx::query("INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) VALUES ($1, $2, $3, $4, now() + interval '12 hours')")
            .bind(Uuid::new_v4()).bind(account_id).bind(hash_token(&token).to_vec())
            .bind(hash_token(&csrf).to_vec()).execute(&self.pool).await?;
        Ok(OwnerGrant {
            account_id,
            role,
            token,
            csrf,
        })
    }

    pub async fn check_owner_csrf(&self, token: &str, csrf: &str) -> Result<(), AccessError> {
        let found: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM owner_grants WHERE token_hash = $1 AND csrf_hash = $2 AND revoked_at IS NULL AND expires_at > now()"
        ).bind(hash_token(token).to_vec()).bind(hash_token(csrf).to_vec())
            .fetch_optional(&self.pool).await?;
        if found.is_none() {
            return Err(AccessError::Forbidden);
        }
        Ok(())
    }

    pub async fn create_session(
        &self,
        owner_token: &str,
        csrf: &str,
    ) -> Result<SessionIds, AccessError> {
        self.check_owner_csrf(owner_token, csrf).await?;
        let (owner_id, _) = self.owner_principal(owner_token).await?;
        let ids = SessionIds {
            session_id: Uuid::new_v4(),
            scene_id: Uuid::new_v4(),
        };
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(ids.session_id)
            .bind(owner_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(ids.scene_id).bind(ids.session_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(ids.scene_id)
            .bind(ids.session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(ids)
    }

    pub async fn owner_scene(
        &self,
        token: &str,
        session_id: Uuid,
    ) -> Result<SceneSummary, AccessError> {
        let (account_id, role) = self.owner_principal(token).await?;
        let row: Option<(Uuid, Uuid, String, i32, i64, i64)> = sqlx::query_as(
            "SELECT s.id, c.id, c.world_id, c.world_version, c.scene_epoch, c.revision \
             FROM sessions s JOIN scenes c ON c.id = s.active_scene_id \
             WHERE s.id = $1 AND (s.owner_id = $2 OR $3 = 'admin')",
        )
        .bind(session_id)
        .bind(account_id)
        .bind(role)
        .fetch_optional(&self.pool)
        .await?;
        scene_row(row)
    }

    /// A trusted Viewer is provisioned by an Owner on the device opening this request.
    /// The grant is session-scoped and cannot be promoted by a Controller invitation.
    pub async fn create_viewer(
        &self,
        owner_token: &str,
        csrf: &str,
        session_id: Uuid,
        interact: bool,
    ) -> Result<ViewerGrant, AccessError> {
        self.check_owner_csrf(owner_token, csrf).await?;
        let (account_id, owner_role) = self.owner_principal(owner_token).await?;
        let mut tx = self.pool.begin().await?;
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM sessions WHERE id = $1 AND (owner_id = $2 OR $3 = 'admin') FOR UPDATE",
        )
        .bind(session_id)
        .bind(account_id)
        .bind(owner_role)
        .fetch_optional(&mut *tx)
        .await?;
        if !matches!(status.as_deref(), Some("running" | "paused")) {
            return Err(AccessError::Forbidden);
        }
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM device_grants WHERE session_id = $1 AND role IN ('viewer', 'viewer_interact') \
             AND revoked_at IS NULL AND expires_at > now()",
        )
        .bind(session_id)
        .fetch_one(&mut *tx)
        .await?;
        if active >= 2 {
            return Err(AccessError::ViewerLimit);
        }
        let role = if interact {
            "viewer_interact"
        } else {
            "viewer"
        };
        let token = random_token()?;
        let csrf = random_token()?;
        sqlx::query(
            "INSERT INTO device_grants (id, session_id, role, token_hash, csrf_hash, expires_at) \
             VALUES ($1, $2, $3, $4, $5, now() + interval '7 days')",
        )
        .bind(Uuid::new_v4())
        .bind(session_id)
        .bind(role)
        .bind(hash_token(&token).to_vec())
        .bind(hash_token(&csrf).to_vec())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ViewerGrant {
            session_id,
            role: role.to_owned(),
            token,
            csrf,
        })
    }

    pub async fn viewer_scene(
        &self,
        token: &str,
        session_id: Uuid,
    ) -> Result<(SceneSummary, String), AccessError> {
        let row: Option<(Uuid, Uuid, String, i32, i64, i64, String)> = sqlx::query_as(
            "SELECT s.id, c.id, c.world_id, c.world_version, c.scene_epoch, c.revision, g.role \
             FROM device_grants g JOIN sessions s ON s.id = g.session_id \
             JOIN scenes c ON c.id = s.active_scene_id \
             WHERE s.id = $1 AND g.token_hash = $2 AND g.role IN ('viewer', 'viewer_interact') \
             AND g.revoked_at IS NULL AND g.expires_at > now() AND s.status != 'closed'",
        )
        .bind(session_id)
        .bind(hash_token(token).to_vec())
        .fetch_optional(&self.pool)
        .await?;
        let (session, scene, world, version, epoch, revision, role) =
            row.ok_or(AccessError::Forbidden)?;
        Ok((
            SceneSummary {
                session_id: session,
                scene_id: scene,
                world_id: world,
                world_version: version,
                scene_epoch: epoch,
                revision,
            },
            role,
        ))
    }

    pub async fn open_invitation(
        &self,
        owner_token: &str,
        csrf: &str,
        session_id: Uuid,
    ) -> Result<PairInvitation, AccessError> {
        self.check_owner_csrf(owner_token, csrf).await?;
        self.owner_scene(owner_token, session_id).await?;
        let pin = random_pin()?;
        let qr_secret = random_token()?;
        let mut tx = self.pool.begin().await?;
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM sessions WHERE id = $1 FOR UPDATE")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await?;
        if status.as_deref() != Some("running") && status.as_deref() != Some("paused") {
            return Err(AccessError::Forbidden);
        }
        sqlx::query(
            "INSERT INTO pair_invitations (session_id, qr_hash, pin_hash, expires_at) \
             VALUES ($1, $2, $3, now() + interval '5 minutes') \
             ON CONFLICT (session_id) DO UPDATE SET qr_hash = EXCLUDED.qr_hash, \
             pin_hash = EXCLUDED.pin_hash, expires_at = EXCLUDED.expires_at, \
             generation = pair_invitations.generation + 1, created_at = now()",
        )
        .bind(session_id)
        .bind(hash_token(&qr_secret).to_vec())
        .bind(self.pin_mac(session_id, &pin))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(PairInvitation {
            session_id,
            pin,
            qr_secret,
        })
    }

    pub async fn pair_controller(
        &self,
        session_id: Uuid,
        client_key: &str,
        peer_ip: &str,
        code: PairCode<'_>,
    ) -> Result<ControllerGrant, AccessError> {
        if client_key.is_empty()
            || client_key.len() > 128
            || peer_ip.is_empty()
            || peer_ip.len() > 64
        {
            return Err(AccessError::InvalidCredentials);
        }
        let client_hash = self.keyed_hash(b"client", client_key.as_bytes());
        let ip_hash = self.keyed_hash(b"ip", peer_ip.as_bytes());
        let mut tx = self.pool.begin().await?;
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM sessions WHERE id = $1 FOR UPDATE")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await?;
        if status.as_deref() != Some("running") && status.as_deref() != Some("paused") {
            return Err(AccessError::Forbidden);
        }
        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE client_key_hash = $2), \
             count(*) FILTER (WHERE ip_hash = $3), count(*) \
             FROM pair_attempts WHERE session_id = $1 AND NOT accepted \
             AND attempted_at > now() - interval '1 minute'",
        )
        .bind(session_id)
        .bind(&client_hash)
        .bind(&ip_hash)
        .fetch_one(&mut *tx)
        .await?;
        if counts.0 >= 5 || counts.1 >= 30 || counts.2 >= 60 {
            return Err(AccessError::RateLimited);
        }
        let invitation: Option<(Vec<u8>, Vec<u8>, bool)> = sqlx::query_as(
            "SELECT qr_hash, pin_hash, approval_required FROM pair_invitations \
             WHERE session_id = $1 AND expires_at > now()",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?;
        let valid = invitation
            .as_ref()
            .is_some_and(|(qr_hash, pin_hash, _)| match code {
                PairCode::Pin(pin) => {
                    pin.len() == 6
                        && pin.bytes().all(|byte| byte.is_ascii_digit())
                        && self.verify_pin(session_id, pin, pin_hash)
                }
                PairCode::Qr(secret) => {
                    secret.len() == 43 && hash_token(secret).as_slice() == qr_hash
                }
            });
        sqlx::query("INSERT INTO pair_attempts (session_id, client_key_hash, ip_hash, accepted) VALUES ($1, $2, $3, $4)")
            .bind(session_id).bind(client_hash).bind(ip_hash).bind(valid)
            .execute(&mut *tx).await?;
        if !valid {
            tx.commit().await?;
            return Err(AccessError::InvalidCredentials);
        }
        if invitation.is_some_and(|(_, _, approval)| approval) {
            tx.commit().await?;
            return Err(AccessError::OwnerApprovalRequired);
        }
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM device_grants WHERE session_id = $1 AND role = 'controller' \
             AND revoked_at IS NULL AND expires_at > now() AND last_activity_at > now() - interval '2 hours' \
             AND coalesce(last_heartbeat_at, issued_at) > now() - interval '60 seconds'"
        ).bind(session_id).fetch_one(&mut *tx).await?;
        if active >= 10 {
            tx.commit().await?;
            return Err(AccessError::ControllerLimit);
        }
        let participant_id = Uuid::new_v4();
        let token = random_token()?;
        let csrf = random_token()?;
        sqlx::query(
            "INSERT INTO device_grants (id, session_id, participant_id, role, token_hash, csrf_hash, \
             expires_at, last_heartbeat_at) VALUES ($1, $2, $3, 'controller', $4, $5, \
             now() + interval '12 hours', now())"
        ).bind(Uuid::new_v4()).bind(session_id).bind(participant_id)
            .bind(hash_token(&token).to_vec()).bind(hash_token(&csrf).to_vec())
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(ControllerGrant {
            session_id,
            participant_id,
            token,
            csrf,
        })
    }

    pub async fn controller_scene(
        &self,
        token: &str,
        session_id: Uuid,
    ) -> Result<SceneSummary, AccessError> {
        let row: Option<(Uuid, Uuid, String, i32, i64, i64)> = sqlx::query_as(
            "SELECT s.id, c.id, c.world_id, c.world_version, c.scene_epoch, c.revision \
             FROM device_grants g JOIN sessions s ON s.id = g.session_id \
             JOIN scenes c ON c.id = s.active_scene_id \
             WHERE s.id = $1 AND g.token_hash = $2 AND g.role = 'controller' \
             AND g.revoked_at IS NULL AND g.expires_at > now() \
             AND g.last_activity_at > now() - interval '2 hours' AND s.status != 'closed'",
        )
        .bind(session_id)
        .bind(hash_token(token).to_vec())
        .fetch_optional(&self.pool)
        .await?;
        scene_row(row)
    }

    pub async fn check_controller_csrf(
        &self,
        token: &str,
        csrf: &str,
        session_id: Uuid,
    ) -> Result<(), AccessError> {
        let found: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM device_grants WHERE session_id = $1 AND token_hash = $2 AND csrf_hash = $3 \
             AND role = 'controller' AND revoked_at IS NULL AND expires_at > now() \
             AND last_activity_at > now() - interval '2 hours'"
        ).bind(session_id).bind(hash_token(token).to_vec()).bind(hash_token(csrf).to_vec())
            .fetch_optional(&self.pool).await?;
        if found.is_none() {
            return Err(AccessError::Forbidden);
        }
        Ok(())
    }

    fn pin_mac(&self, session_id: Uuid, pin: &str) -> Vec<u8> {
        self.keyed_hash(session_id.as_bytes(), pin.as_bytes())
    }

    fn verify_pin(&self, session_id: Uuid, pin: &str, expected: &[u8]) -> bool {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.pin_key).expect("fixed HMAC key");
        mac.update(session_id.as_bytes());
        mac.update(pin.as_bytes());
        mac.verify_slice(expected).is_ok()
    }

    fn keyed_hash(&self, domain: &[u8], value: &[u8]) -> Vec<u8> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.pin_key).expect("fixed HMAC key");
        mac.update(domain);
        mac.update(value);
        mac.finalize().into_bytes().to_vec()
    }

    async fn owner_principal(&self, token: &str) -> Result<(Uuid, String), AccessError> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT a.id, a.role FROM owner_grants g JOIN accounts a ON a.id = g.account_id \
             WHERE g.token_hash = $1 AND g.revoked_at IS NULL AND g.expires_at > now() AND a.disabled_at IS NULL"
        ).bind(hash_token(token).to_vec()).fetch_optional(&self.pool).await?;
        row.ok_or(AccessError::Forbidden)
    }
}

fn password_hash(login: &str, password: &str) -> Result<String, AccessError> {
    if login.len() < 3 || login.len() > 64 || password.len() < 12 || password.len() > 1024 {
        return Err(AccessError::InvalidAccount);
    }
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|_| AccessError::Crypto)
}

pub(crate) fn random_token() -> Result<String, AccessError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| AccessError::Crypto)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn random_pin() -> Result<String, AccessError> {
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|_| AccessError::Crypto)?;
        let value = u32::from_le_bytes(bytes) as u64;
        if value < 4_294_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}

pub(crate) fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn scene_row(
    row: Option<(Uuid, Uuid, String, i32, i64, i64)>,
) -> Result<SceneSummary, AccessError> {
    row.map(
        |(session_id, scene_id, world_id, world_version, scene_epoch, revision)| SceneSummary {
            session_id,
            scene_id,
            world_id,
            world_version,
            scene_epoch,
            revision,
        },
    )
    .ok_or(AccessError::Forbidden)
}
