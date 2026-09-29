use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
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
pub struct SceneSummary {
    pub session_id: Uuid,
    pub scene_id: Uuid,
    pub world_id: String,
    pub world_version: i32,
    pub scene_epoch: i64,
    pub revision: i64,
}

impl AccessStore {
    pub fn new(pool: PgPool, pin_key: [u8; 32]) -> Self {
        Self { pool, pin_key }
    }

    /// Only a local bootstrap command may call this; it is never an HTTP route.
    pub async fn bootstrap_admin(&self, login: &str, password: &str) -> Result<Uuid, AccessError> {
        let hash = password_hash(login, password)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(757823)").execute(&mut *tx).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin'")
            .fetch_one(&mut *tx).await?;
        if count != 0 { return Err(AccessError::AlreadyInitialized); }
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'admin', $3)")
            .bind(id).bind(login).bind(hash).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn create_owner(&self, admin_token: &str, login: &str, password: &str) -> Result<Uuid, AccessError> {
        let (_, role) = self.owner_principal(admin_token).await?;
        if role != "admin" { return Err(AccessError::Forbidden); }
        let hash = password_hash(login, password)?;
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', $3)")
            .bind(id).bind(login).bind(hash).execute(&self.pool).await?;
        Ok(id)
    }

    pub async fn login(&self, login: &str, password: &str) -> Result<OwnerGrant, AccessError> {
        let account: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, role, password_hash FROM accounts WHERE login = $1 AND disabled_at IS NULL"
        ).bind(login).fetch_optional(&self.pool).await?;
        let (account_id, role, stored_hash) = account.ok_or(AccessError::InvalidCredentials)?;
        let parsed = PasswordHash::new(&stored_hash).map_err(|_| AccessError::Crypto)?;
        Argon2::default().verify_password(password.as_bytes(), &parsed)
            .map_err(|_| AccessError::InvalidCredentials)?;
        let token = random_token()?;
        let csrf = random_token()?;
        sqlx::query("INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) VALUES ($1, $2, $3, $4, now() + interval '12 hours')")
            .bind(Uuid::new_v4()).bind(account_id).bind(hash_token(&token).to_vec())
            .bind(hash_token(&csrf).to_vec()).execute(&self.pool).await?;
        Ok(OwnerGrant { account_id, role, token, csrf })
    }

    pub async fn check_owner_csrf(&self, token: &str, csrf: &str) -> Result<(), AccessError> {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM owner_grants WHERE token_hash = $1 AND csrf_hash = $2 AND revoked_at IS NULL AND expires_at > now()"
        ).bind(hash_token(token).to_vec()).bind(hash_token(csrf).to_vec())
            .fetch_optional(&self.pool).await?;
        if found.is_none() { return Err(AccessError::Forbidden); }
        Ok(())
    }

    pub async fn create_session(&self, owner_token: &str, csrf: &str) -> Result<SessionIds, AccessError> {
        self.check_owner_csrf(owner_token, csrf).await?;
        let (owner_id, _) = self.owner_principal(owner_token).await?;
        let ids = SessionIds { session_id: Uuid::new_v4(), scene_id: Uuid::new_v4() };
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(ids.session_id).bind(owner_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(ids.scene_id).bind(ids.session_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(ids.scene_id).bind(ids.session_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(ids)
    }

    pub async fn owner_scene(&self, token: &str, session_id: Uuid) -> Result<SceneSummary, AccessError> {
        let (account_id, role) = self.owner_principal(token).await?;
        let row: Option<(Uuid, Uuid, String, i32, i64, i64)> = sqlx::query_as(
            "SELECT s.id, c.id, c.world_id, c.world_version, c.scene_epoch, c.revision \
             FROM sessions s JOIN scenes c ON c.id = s.active_scene_id \
             WHERE s.id = $1 AND (s.owner_id = $2 OR $3 = 'admin')"
        ).bind(session_id).bind(account_id).bind(role).fetch_optional(&self.pool).await?;
        scene_row(row)
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
    Argon2::default().hash_password(password.as_bytes())
        .map(|hash| hash.to_string()).map_err(|_| AccessError::Crypto)
}

pub(crate) fn random_token() -> Result<String, AccessError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| AccessError::Crypto)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn scene_row(row: Option<(Uuid, Uuid, String, i32, i64, i64)>) -> Result<SceneSummary, AccessError> {
    row.map(|(session_id, scene_id, world_id, world_version, scene_epoch, revision)| SceneSummary {
        session_id, scene_id, world_id, world_version, scene_epoch, revision
    }).ok_or(AccessError::Forbidden)
}
