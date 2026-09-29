use sqlx::{PgPool, migrate::MigrateError};
pub mod access;

/// The server uses versioned, embedded migrations; no database credentials live in source.
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn migration_keeps_active_scene_inside_its_session() {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL for isolated test database");
        let pool = PgPool::connect(&database_url).await.expect("connect test PostgreSQL");
        migrate(&pool).await.expect("apply migrations");
        migrate(&pool).await.expect("migrations are repeatable");

        let owner = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("test-{}", owner))
            .execute(&pool).await.expect("insert owner");
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        for session in [session_a, session_b] {
            sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
                .bind(session).bind(owner).execute(&pool).await.expect("insert session");
        }
        let scene_a = Uuid::new_v4();
        let scene_b = Uuid::new_v4();
        for (scene, session) in [(scene_a, session_a), (scene_b, session_b)] {
            sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
                .bind(scene).bind(session).execute(&pool).await.expect("insert scene");
        }
        let wrong = sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_b).bind(session_a).execute(&pool).await;
        assert!(wrong.is_err(), "a session cannot activate another session’s scene");
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_a).bind(session_a).execute(&pool).await.expect("activate own scene");
        let found: Uuid = sqlx::query_scalar("SELECT active_scene_id FROM sessions WHERE id = $1")
            .bind(session_a).fetch_one(&pool).await.expect("read own scene");
        assert_eq!(found, scene_a);

        let store = access::AccessStore::new(pool.clone(), [7u8; 32]);
        let admin_password = Uuid::new_v4().to_string();
        store.bootstrap_admin("ci-admin", &admin_password).await.expect("bootstrap admin once");
        assert!(store.bootstrap_admin("other-admin", &admin_password).await.is_err());
        assert!(store.login("ci-admin", "wrong-password").await.is_err());
        let admin = store.login("ci-admin", &admin_password).await.expect("admin login");
        let first_password = Uuid::new_v4().to_string();
        let second_password = Uuid::new_v4().to_string();
        store.create_owner(&admin.token, "ci-owner-one", &first_password).await.expect("first owner");
        store.create_owner(&admin.token, "ci-owner-two", &second_password).await.expect("second owner");
        let first = store.login("ci-owner-one", &first_password).await.expect("first login");
        let second = store.login("ci-owner-two", &second_password).await.expect("second login");
        assert!(store.create_owner(&first.token, "unauthorized-owner", &first_password).await.is_err());
        assert!(store.create_session(&first.token, &second.csrf).await.is_err());
        let first_scene = store.create_session(&first.token, &first.csrf).await.expect("first session");
        let second_scene = store.create_session(&second.token, &second.csrf).await.expect("second session");
        assert_eq!(store.owner_scene(&first.token, first_scene.session_id).await.expect("own scene").scene_id,
            first_scene.scene_id);
        assert!(store.owner_scene(&first.token, second_scene.session_id).await.is_err(),
            "owner token cannot read another owner's scene");
        assert_eq!(store.owner_scene(&admin.token, second_scene.session_id).await.expect("admin access").scene_id,
            second_scene.scene_id);
    }
}
