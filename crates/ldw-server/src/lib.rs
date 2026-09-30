use sqlx::{PgPool, migrate::MigrateError};
pub mod access;
pub mod blob_gc;
pub mod blob_store;
pub mod http;
pub mod paint_image;
pub mod realtime;
pub mod simulation;
pub mod upload;

/// The server uses versioned, embedded migrations; no database credentials live in source.
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

#[cfg(test)]
pub(crate) async fn test_pool() -> PgPool {
    static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    MIGRATED
        .get_or_init(|| async {
            let database_url = std::env::var("DATABASE_URL").expect("isolated test database");
            let pool = PgPool::connect(&database_url)
                .await
                .expect("connect test PostgreSQL");
            migrate(&pool)
                .await
                .expect("apply migrations once before parallel tests");
        })
        .await;
    PgPool::connect(&std::env::var("DATABASE_URL").expect("isolated test database"))
        .await
        .expect("connect test PostgreSQL")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Request, StatusCode, header},
    };
    use std::{net::SocketAddr, sync::Arc};
    use tower::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn migration_keeps_active_scene_inside_its_session() {
        let pool = test_pool().await;
        migrate(&pool).await.expect("migrations are repeatable");

        let owner = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("test-{}", owner))
            .execute(&pool).await.expect("insert owner");
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        for session in [session_a, session_b] {
            sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
                .bind(session)
                .bind(owner)
                .execute(&pool)
                .await
                .expect("insert session");
        }
        let scene_a = Uuid::new_v4();
        let scene_b = Uuid::new_v4();
        for (scene, session) in [(scene_a, session_a), (scene_b, session_b)] {
            sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
                .bind(scene).bind(session).execute(&pool).await.expect("insert scene");
        }
        let wrong = sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_b)
            .bind(session_a)
            .execute(&pool)
            .await;
        assert!(
            wrong.is_err(),
            "a session cannot activate another session’s scene"
        );
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_a)
            .bind(session_a)
            .execute(&pool)
            .await
            .expect("activate own scene");
        let found: Uuid = sqlx::query_scalar("SELECT active_scene_id FROM sessions WHERE id = $1")
            .bind(session_a)
            .fetch_one(&pool)
            .await
            .expect("read own scene");
        assert_eq!(found, scene_a);

        let store = access::AccessStore::new(pool.clone(), [7u8; 32]);
        let admin_password = Uuid::new_v4().to_string();
        store
            .bootstrap_admin("ci-admin", &admin_password)
            .await
            .expect("bootstrap admin once");
        assert!(
            store
                .bootstrap_admin("other-admin", &admin_password)
                .await
                .is_err()
        );
        assert!(
            store
                .login("ci-admin", "wrong-password", "test-peer")
                .await
                .is_err()
        );
        let admin = store
            .login("ci-admin", &admin_password, "test-peer")
            .await
            .expect("admin login");
        let first_password = Uuid::new_v4().to_string();
        let second_password = Uuid::new_v4().to_string();
        store
            .create_owner(&admin.token, "ci-owner-one", &first_password)
            .await
            .expect("first owner");
        store
            .create_owner(&admin.token, "ci-owner-two", &second_password)
            .await
            .expect("second owner");
        let first = store
            .login("ci-owner-one", &first_password, "test-peer")
            .await
            .expect("first login");
        let second = store
            .login("ci-owner-two", &second_password, "test-peer")
            .await
            .expect("second login");
        assert!(
            store
                .create_owner(&first.token, "unauthorized-owner", &first_password)
                .await
                .is_err()
        );
        assert!(
            store
                .create_session(&first.token, &second.csrf)
                .await
                .is_err()
        );
        let first_scene = store
            .create_session(&first.token, &first.csrf)
            .await
            .expect("first session");
        let second_scene = store
            .create_session(&second.token, &second.csrf)
            .await
            .expect("second session");
        assert_eq!(
            store
                .owner_scene(&first.token, first_scene.session_id)
                .await
                .expect("own scene")
                .scene_id,
            first_scene.scene_id
        );
        assert!(
            store
                .owner_scene(&first.token, second_scene.session_id)
                .await
                .is_err(),
            "owner token cannot read another owner's scene"
        );
        assert_eq!(
            store
                .owner_scene(&admin.token, second_scene.session_id)
                .await
                .expect("admin access")
                .scene_id,
            second_scene.scene_id
        );

        assert!(
            store
                .create_viewer(&second.token, &second.csrf, first_scene.session_id, false)
                .await
                .is_err(),
            "another Owner cannot provision a Viewer"
        );
        let read_only = store
            .create_viewer(&first.token, &first.csrf, first_scene.session_id, false)
            .await
            .expect("provision read-only Viewer");
        let interactive = store
            .create_viewer(&first.token, &first.csrf, first_scene.session_id, true)
            .await
            .expect("provision interactive Viewer");
        assert_eq!(
            store
                .viewer_scene(&read_only.token, first_scene.session_id)
                .await
                .expect("Viewer reads own scene")
                .1,
            "viewer"
        );
        assert_eq!(
            store
                .viewer_scene(&interactive.token, first_scene.session_id)
                .await
                .expect("interactive Viewer reads own scene")
                .1,
            "viewer_interact"
        );
        assert!(
            store
                .viewer_scene(&read_only.token, second_scene.session_id)
                .await
                .is_err()
        );
        assert!(matches!(
            store
                .create_viewer(&first.token, &first.csrf, first_scene.session_id, false)
                .await,
            Err(access::AccessError::ViewerLimit)
        ));
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let interaction = realtime::InteractionCommand {
            kind: "command".to_owned(),
            command_id: Uuid::new_v4(),
            session_id: first_scene.session_id,
            scene_id: first_scene.scene_id,
            scene_epoch: 1,
            interaction_id: "boat".to_owned(),
            point: realtime::Point { x: 1.0, y: -1.0 },
            target_action_id: None,
            expires_at: now_ms + 8000,
        };
        let rejected = realtime::process_command(
            &store,
            access::GrantKind::Viewer,
            &read_only.token,
            first_scene.session_id,
            &interaction,
        )
        .await
        .expect("read-only command receives durable rejection");
        assert_eq!(rejected["code"], "READ_ONLY");
        assert_eq!(rejected["revision"], 0);
        assert_eq!(
            realtime::process_command(
                &store,
                access::GrantKind::Viewer,
                &read_only.token,
                first_scene.session_id,
                &interaction,
            )
            .await
            .unwrap(),
            rejected
        );
        let accepted = realtime::process_command(
            &store,
            access::GrantKind::Viewer,
            &interactive.token,
            first_scene.session_id,
            &interaction,
        )
        .await
        .expect("interactive Viewer creates one event");
        assert_eq!(accepted["accepted"], true);
        assert_eq!(accepted["revision"], 1);
        assert_eq!(
            realtime::process_command(
                &store,
                access::GrantKind::Viewer,
                &interactive.token,
                first_scene.session_id,
                &interaction,
            )
            .await
            .unwrap(),
            accepted,
            "repeated command returns same committed ACK"
        );
        let mut conflict = interaction.clone();
        conflict.point.x = 2.0;
        assert_eq!(
            realtime::process_command(
                &store,
                access::GrantKind::Viewer,
                &interactive.token,
                first_scene.session_id,
                &conflict,
            )
            .await
            .unwrap()["code"],
            "COMMAND_CONFLICT"
        );
        assert_eq!(
            realtime::command_status(
                &store,
                access::GrantKind::Viewer,
                &interactive.token,
                first_scene.session_id,
                interaction.command_id,
            )
            .await
            .unwrap(),
            Some(accepted.clone())
        );
        let event_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM scene_events WHERE scene_id = $1")
                .bind(first_scene.scene_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            event_count, 1,
            "duplicate and conflict do not create events"
        );
        assert!(store.bump_active_epochs().await.unwrap() >= 2);
        assert_eq!(
            store
                .owner_scene(&first.token, first_scene.session_id)
                .await
                .unwrap()
                .scene_epoch,
            2
        );
        assert_eq!(
            realtime::process_command(
                &store,
                access::GrantKind::Viewer,
                &interactive.token,
                first_scene.session_id,
                &interaction,
            )
            .await
            .unwrap(),
            accepted,
            "known outcome survives epoch change"
        );
        let mut stale = interaction.clone();
        stale.command_id = Uuid::new_v4();
        assert_eq!(
            realtime::process_command(
                &store,
                access::GrantKind::Viewer,
                &interactive.token,
                first_scene.session_id,
                &stale,
            )
            .await
            .unwrap()["code"],
            "STALE_SCENE"
        );
        let concurrent = realtime::InteractionCommand {
            command_id: Uuid::new_v4(),
            session_id: second_scene.session_id,
            scene_id: second_scene.scene_id,
            scene_epoch: 2,
            expires_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
                + 8000,
            ..interaction.clone()
        };
        let (first_result, second_result) = tokio::join!(
            realtime::process_command(
                &store,
                access::GrantKind::Owner,
                &second.token,
                second_scene.session_id,
                &concurrent
            ),
            realtime::process_command(
                &store,
                access::GrantKind::Owner,
                &second.token,
                second_scene.session_id,
                &concurrent
            )
        );
        assert_eq!(first_result.unwrap(), second_result.unwrap());
        let count_after_race: i64 =
            sqlx::query_scalar("SELECT count(*) FROM scene_events WHERE scene_id = $1")
                .bind(second_scene.scene_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            count_after_race, 1,
            "concurrent duplicate commits one event"
        );
        let mut expired = concurrent.clone();
        expired.command_id = Uuid::new_v4();
        expired.expires_at = 1;
        assert_eq!(
            realtime::process_command(
                &store,
                access::GrantKind::Owner,
                &second.token,
                second_scene.session_id,
                &expired,
            )
            .await
            .unwrap()["code"],
            "EXPIRED_COMMAND"
        );
        let mut outside = concurrent.clone();
        outside.command_id = Uuid::new_v4();
        outside.point.x = 8.0;
        assert_eq!(
            realtime::process_command(
                &store,
                access::GrantKind::Owner,
                &second.token,
                second_scene.session_id,
                &outside,
            )
            .await
            .unwrap()["code"],
            "OUTSIDE_WATER"
        );
        let invite = store
            .open_invitation(&first.token, &first.csrf, first_scene.session_id)
            .await
            .expect("owner opens invitation");
        assert_eq!(invite.pin.len(), 6);
        assert!(
            store
                .open_invitation(&second.token, &second.csrf, first_scene.session_id)
                .await
                .is_err(),
            "other owner cannot open pairing"
        );
        assert!(
            store
                .pair_controller(
                    first_scene.session_id,
                    "device-a",
                    "test-ip",
                    access::PairCode::Pin("0000000")
                )
                .await
                .is_err()
        );
        let controller = store
            .pair_controller(
                first_scene.session_id,
                "device-a",
                "test-ip",
                access::PairCode::Pin(&invite.pin),
            )
            .await
            .expect("valid PIN pairs controller");
        assert_eq!(
            store
                .controller_scene(&controller.token, first_scene.session_id)
                .await
                .expect("controller reads own scene")
                .scene_id,
            first_scene.scene_id
        );
        assert!(
            store
                .controller_scene(&controller.token, second_scene.session_id)
                .await
                .is_err(),
            "controller token cannot read another session"
        );
        assert!(
            store
                .check_controller_csrf(&controller.token, &second.csrf, first_scene.session_id)
                .await
                .is_err(),
            "controller cannot use another CSRF token"
        );
        store
            .check_controller_csrf(&controller.token, &controller.csrf, first_scene.session_id)
            .await
            .expect("own CSRF token");
        let replacement = store
            .open_invitation(&first.token, &first.csrf, first_scene.session_id)
            .await
            .expect("rotate invitation");
        assert!(
            store
                .pair_controller(
                    first_scene.session_id,
                    "device-b",
                    "test-ip",
                    access::PairCode::Qr(&invite.qr_secret)
                )
                .await
                .is_err(),
            "old QR must be invalid"
        );
        for number in 0..9 {
            store
                .pair_controller(
                    first_scene.session_id,
                    &format!("device-{number}"),
                    "test-ip",
                    access::PairCode::Qr(&replacement.qr_secret),
                )
                .await
                .expect("controller within limit");
        }
        assert!(matches!(
            store
                .pair_controller(
                    first_scene.session_id,
                    "last-device",
                    "test-ip",
                    access::PairCode::Qr(&replacement.qr_secret)
                )
                .await,
            Err(access::AccessError::ControllerLimit)
        ));
        sqlx::query("UPDATE device_grants SET last_heartbeat_at = now() - interval '61 seconds' WHERE token_hash = $1")
            .bind(access::hash_token(&controller.token).to_vec())
            .execute(&pool).await.unwrap();
        let replacement_controller = store
            .pair_controller(
                first_scene.session_id,
                "replacement-device",
                "test-ip",
                access::PairCode::Qr(&replacement.qr_secret),
            )
            .await
            .expect("disconnected Controller frees a slot");
        assert!(matches!(
            store
                .resume_controller(&controller.token, first_scene.session_id)
                .await,
            Err(access::AccessError::ControllerLimit)
        ));
        sqlx::query("UPDATE device_grants SET last_heartbeat_at = now() - interval '61 seconds' WHERE token_hash = $1")
            .bind(access::hash_token(&replacement_controller.token).to_vec())
            .execute(&pool).await.unwrap();
        store
            .resume_controller(&controller.token, first_scene.session_id)
            .await
            .expect("Controller reuses newly free slot");
        store
            .heartbeat_controller(&controller.token, first_scene.session_id)
            .await
            .expect("active Controller updates heartbeat");
        for _ in 0..4 {
            assert!(
                store
                    .pair_controller(
                        second_scene.session_id,
                        "guessing-device",
                        "other-ip",
                        access::PairCode::Pin("111111")
                    )
                    .await
                    .is_err()
            );
        }
        assert!(matches!(
            store
                .pair_controller(
                    second_scene.session_id,
                    "guessing-device",
                    "other-ip",
                    access::PairCode::Pin("111111")
                )
                .await,
            Err(access::AccessError::InvalidCredentials)
        ));
        assert!(matches!(
            store
                .pair_controller(
                    second_scene.session_id,
                    "guessing-device",
                    "other-ip",
                    access::PairCode::Pin("111111")
                )
                .await,
            Err(access::AccessError::RateLimited)
        ));

        let simulation_hub = simulation::SimulationHub::default();
        let app = http::router(http::AppState {
            access: store.clone(),
            blob_store: blob_store::BlobStore::create(
                std::env::temp_dir().join(format!("ldw-http-blobs-{}", Uuid::new_v4())),
            )
            .unwrap(),
            public_origin: Arc::from("https://world.example.test"),
            simulation_hub: simulation_hub.clone(),
        });
        let readiness = Request::builder()
            .uri("/health/ready")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(readiness).await.unwrap().status(),
            StatusCode::OK
        );
        let owner_cookie = format!("__Host-ldw-owner={}", first.token);
        let own_request = Request::builder()
            .uri(format!("/api/sessions/{}/scene", first_scene.session_id))
            .header(header::COOKIE, &owner_cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(own_request).await.unwrap().status(),
            StatusCode::OK
        );
        let foreign_request = Request::builder()
            .uri(format!("/api/sessions/{}/scene", second_scene.session_id))
            .header(header::COOKIE, &owner_cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(foreign_request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let missing_origin = Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header(header::COOKIE, &owner_cookie)
            .header("x-csrf-token", &first.csrf)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(missing_origin).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let bad_csrf = Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header(header::COOKIE, &owner_cookie)
            .header(header::ORIGIN, "https://world.example.test")
            .header("x-csrf-token", &second.csrf)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(bad_csrf).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let correct = Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header(header::COOKIE, &owner_cookie)
            .header(header::ORIGIN, "https://world.example.test")
            .header("x-csrf-token", &first.csrf)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(correct).await.unwrap().status(),
            StatusCode::OK
        );

        let second_owner_cookie = format!("__Host-ldw-owner={}", second.token);
        let viewer_body = serde_json::json!({ "interact": false }).to_string();
        let viewer_without_origin = Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{}/viewers", second_scene.session_id))
            .header(header::COOKIE, &second_owner_cookie)
            .header("x-csrf-token", &second.csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(viewer_body.clone()))
            .unwrap();
        assert_eq!(
            app.clone()
                .oneshot(viewer_without_origin)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let viewer_request = Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{}/viewers", second_scene.session_id))
            .header(header::COOKIE, &second_owner_cookie)
            .header("x-csrf-token", &second.csrf)
            .header(header::ORIGIN, "https://world.example.test")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(viewer_body))
            .unwrap();
        let viewer_response = app.clone().oneshot(viewer_request).await.unwrap();
        assert_eq!(viewer_response.status(), StatusCode::OK);
        let viewer_set_cookie = viewer_response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(viewer_set_cookie.starts_with("__Host-ldw-viewer="));
        assert!(
            viewer_set_cookie.contains("Secure")
                && viewer_set_cookie.contains("HttpOnly")
                && viewer_set_cookie.contains("SameSite=Strict")
        );
        let viewer_cookie = viewer_set_cookie.split(';').next().unwrap();
        let viewer_own = Request::builder()
            .uri(format!("/api/sessions/{}/scene", second_scene.session_id))
            .header(header::COOKIE, viewer_cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(viewer_own).await.unwrap().status(),
            StatusCode::OK
        );
        let viewer_foreign = Request::builder()
            .uri(format!("/api/sessions/{}/scene", first_scene.session_id))
            .header(header::COOKIE, viewer_cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(viewer_foreign).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let viewer_mutation = Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header(header::COOKIE, viewer_cookie)
            .header(header::ORIGIN, "https://world.example.test")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(viewer_mutation).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );

        let tv_session = store
            .create_session(&first.token, &first.csrf)
            .await
            .unwrap();
        let claim_request = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/viewer-claims",
                tv_session.session_id
            ))
            .header(header::ORIGIN, "https://world.example.test")
            .body(Body::empty())
            .unwrap();
        let claim_response = app.clone().oneshot(claim_request).await.unwrap();
        assert_eq!(claim_response.status(), StatusCode::OK);
        let claim_cookie = claim_response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        assert!(claim_cookie.starts_with("__Host-ldw-viewer-claim="));
        let claim_body: serde_json::Value =
            serde_json::from_slice(&to_bytes(claim_response.into_body(), 4096).await.unwrap())
                .unwrap();
        assert_eq!(claim_body["code"].as_str().unwrap().len(), 8);
        let activation_uri = format!(
            "/api/sessions/{}/viewer-claims/{}/activate",
            tv_session.session_id,
            claim_body["claim_id"].as_str().unwrap()
        );
        let pending_request = Request::builder()
            .method("POST")
            .uri(&activation_uri)
            .header(header::ORIGIN, "https://world.example.test")
            .header(header::COOKIE, &claim_cookie)
            .header("x-csrf-token", claim_body["csrf"].as_str().unwrap())
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(pending_request).await.unwrap().status(),
            StatusCode::ACCEPTED
        );
        let claim_without_csrf = Request::builder()
            .method("POST")
            .uri(&activation_uri)
            .header(header::ORIGIN, "https://world.example.test")
            .header(header::COOKIE, &claim_cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone()
                .oneshot(claim_without_csrf)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let approve_body =
            serde_json::json!({"code":claim_body["code"],"interact":false}).to_string();
        let wrong_approval = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/viewer-claims/approve",
                tv_session.session_id
            ))
            .header(header::COOKIE, &second_owner_cookie)
            .header(header::ORIGIN, "https://world.example.test")
            .header("x-csrf-token", &second.csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(approve_body.clone()))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(wrong_approval).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let approval = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/viewer-claims/approve",
                tv_session.session_id
            ))
            .header(header::COOKIE, &owner_cookie)
            .header(header::ORIGIN, "https://world.example.test")
            .header("x-csrf-token", &first.csrf)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(approve_body))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(approval).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        let activate = Request::builder()
            .method("POST")
            .uri(&activation_uri)
            .header(header::ORIGIN, "https://world.example.test")
            .header(header::COOKIE, &claim_cookie)
            .header("x-csrf-token", claim_body["csrf"].as_str().unwrap())
            .body(Body::empty())
            .unwrap();
        let activated = app.clone().oneshot(activate).await.unwrap();
        assert_eq!(activated.status(), StatusCode::OK);
        let tv_cookie = activated
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let tv_scene = Request::builder()
            .uri(format!("/api/sessions/{}/scene", tv_session.session_id))
            .header(header::COOKIE, tv_cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(tv_scene).await.unwrap().status(),
            StatusCode::OK
        );
        let replay_activate = Request::builder()
            .method("POST")
            .uri(&activation_uri)
            .header(header::ORIGIN, "https://world.example.test")
            .header(header::COOKIE, &claim_cookie)
            .header("x-csrf-token", claim_body["csrf"].as_str().unwrap())
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(replay_activate).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );

        let second_invite = store
            .open_invitation(&second.token, &second.csrf, second_scene.session_id)
            .await
            .expect("owner two opens own invitation");
        let pair_body = serde_json::json!({
            "client_key": "http-client", "qr_secret": second_invite.qr_secret,
        });
        let mut pair_request = Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{}/pair", second_scene.session_id))
            .header(header::ORIGIN, "https://world.example.test")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(pair_body.to_string()))
            .unwrap();
        pair_request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:4100".parse::<SocketAddr>().unwrap()));
        let pair_response = app.clone().oneshot(pair_request).await.unwrap();
        assert_eq!(pair_response.status(), StatusCode::OK);
        let set_cookie = pair_response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            set_cookie.contains("Secure")
                && set_cookie.contains("HttpOnly")
                && set_cookie.contains("SameSite=Strict")
        );
        let controller_cookie = set_cookie.split(';').next().unwrap();
        let controller_own = Request::builder()
            .uri(format!("/api/sessions/{}/scene", second_scene.session_id))
            .header(header::COOKIE, controller_cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(controller_own).await.unwrap().status(),
            StatusCode::OK
        );
        let controller_foreign = Request::builder()
            .uri(format!("/api/sessions/{}/scene", first_scene.session_id))
            .header(header::COOKIE, controller_cookie)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone()
                .oneshot(controller_foreign)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::{Message as ClientMessage, client::IntoClientRequest};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let url = format!("ws://{address}/api/sessions/{}/ws", first_scene.session_id);
        let mut read_request = url.as_str().into_client_request().unwrap();
        read_request
            .headers_mut()
            .insert("origin", "https://world.example.test".parse().unwrap());
        read_request.headers_mut().insert(
            "cookie",
            format!("__Host-ldw-viewer={}", read_only.token)
                .parse()
                .unwrap(),
        );
        let (mut reader, _) = tokio_tungstenite::connect_async(read_request)
            .await
            .unwrap();
        reader
            .send(ClientMessage::text(
                serde_json::json!({"type":"hello","csrf":read_only.csrf}).to_string(),
            ))
            .await
            .unwrap();
        let reader_snapshot: serde_json::Value =
            serde_json::from_str(reader.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(reader_snapshot["type"], "snapshot");
        assert_eq!(reader_snapshot["revision"], 1);
        assert_eq!(
            reader_snapshot["pendingInteractions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let mut interactive_request = url.as_str().into_client_request().unwrap();
        interactive_request
            .headers_mut()
            .insert("origin", "https://world.example.test".parse().unwrap());
        interactive_request.headers_mut().insert(
            "cookie",
            format!("__Host-ldw-viewer={}", interactive.token)
                .parse()
                .unwrap(),
        );
        let (mut writer, _) = tokio_tungstenite::connect_async(interactive_request)
            .await
            .unwrap();
        writer
            .send(ClientMessage::text(
                serde_json::json!({"type":"hello","csrf":interactive.csrf}).to_string(),
            ))
            .await
            .unwrap();
        let writer_snapshot: serde_json::Value =
            serde_json::from_str(writer.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(writer_snapshot["revision"], reader_snapshot["revision"]);
        simulation_hub.publish(simulation::PositionFrame {
            kind: "positions",
            schema_version: 1,
            scene_id: first_scene.scene_id,
            scene_epoch: writer_snapshot["sceneEpoch"].as_i64().unwrap(),
            revision: 1,
            simulation_tick: 10,
            positions: vec![simulation::EntityPosition {
                id: format!("fish-{:032x}", Uuid::new_v4().as_u128()),
                position: ldw_sim::Point { x: 1.0, y: -1.0 },
                heading: ldw_sim::Point { x: 1.0, y: 0.0 },
                depth: -0.5,
                heading_depth: -0.25,
            }],
            action_positions: vec![],
        });
        let writer_positions: serde_json::Value = serde_json::from_str(
            tokio::time::timeout(std::time::Duration::from_secs(3), writer.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        let reader_positions: serde_json::Value = serde_json::from_str(
            tokio::time::timeout(std::time::Duration::from_secs(3), reader.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(writer_positions, reader_positions);
        assert_eq!(writer_positions["type"], "positions");
        assert_eq!(writer_positions["simulationTick"], 10);
        assert_eq!(writer_positions["revision"], 1);
        let command = realtime::InteractionCommand {
            command_id: Uuid::new_v4(),
            scene_epoch: 2,
            interaction_id: "feed".to_owned(),
            expires_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
                + 8000,
            ..interaction.clone()
        };
        writer
            .send(ClientMessage::text(
                serde_json::to_string(&command).unwrap(),
            ))
            .await
            .unwrap();
        let ack: serde_json::Value =
            serde_json::from_str(writer.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ack["accepted"], true);
        assert_eq!(ack["revision"], 2);
        let acknowledged_at = tokio::time::Instant::now();
        let writer_delta: serde_json::Value = serde_json::from_str(
            tokio::time::timeout(std::time::Duration::from_millis(500), writer.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        let reader_delta: serde_json::Value = serde_json::from_str(
            tokio::time::timeout(std::time::Duration::from_millis(500), reader.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(writer_delta, reader_delta);
        assert_eq!(writer_delta["type"], "delta");
        assert_eq!(writer_delta["revision"], 2);
        assert!(
            acknowledged_at.elapsed() <= std::time::Duration::from_millis(500),
            "committed interaction must reach both connected screens promptly"
        );
        let publication_scene = store
            .create_session(&first.token, &first.csrf)
            .await
            .unwrap();
        assert!(
            simulation::publish_first_fish(
                store.pool(),
                publication_scene.scene_id,
                Uuid::new_v4(),
                "coral-fish",
                "paint-invalid",
                ldw_sim::Point { x: 99.0, y: 0.0 },
            )
            .await
            .is_err()
        );
        let first_fish = Uuid::new_v4();
        let published = simulation::publish_first_fish(
            store.pool(),
            publication_scene.scene_id,
            first_fish,
            "coral-fish",
            "paint-first",
            ldw_sim::Point { x: 0.5, y: -0.5 },
        )
        .await
        .unwrap();
        let fish_entity = &published["entity"];
        assert_eq!(
            fish_entity["id"],
            format!("fish-{:032x}", first_fish.as_u128())
        );
        assert!(
            simulation::publish_first_fish(
                store.pool(),
                publication_scene.scene_id,
                Uuid::new_v4(),
                "coral-fish",
                "paint-second",
                ldw_sim::Point { x: 0.0, y: 0.0 },
            )
            .await
            .is_err(),
            "a second first-fish transaction must not overwrite the checkpoint"
        );
        let loaded = simulation::load_scene(store.pool(), publication_scene.scene_id)
            .await
            .unwrap();
        assert_eq!(loaded.world.fish().len(), 1);
        assert_eq!(loaded.world.fish()[0].id, first_fish.as_u128());
        assert_eq!(loaded.world.tick_number(), 0);
        drop(reader);
        let mut reconnect_request = url.as_str().into_client_request().unwrap();
        reconnect_request
            .headers_mut()
            .insert("origin", "https://world.example.test".parse().unwrap());
        reconnect_request.headers_mut().insert(
            "cookie",
            format!("__Host-ldw-viewer={}", read_only.token)
                .parse()
                .unwrap(),
        );
        let (mut reader_again, _) = tokio_tungstenite::connect_async(reconnect_request)
            .await
            .unwrap();
        reader_again
            .send(ClientMessage::text(
                serde_json::json!({"type":"hello","csrf":read_only.csrf}).to_string(),
            ))
            .await
            .unwrap();
        let recovered: serde_json::Value = serde_json::from_str(
            reader_again
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(recovered["revision"], 2);
        assert!(recovered["entities"].as_array().unwrap().is_empty());
        assert_eq!(
            recovered["pendingInteractions"].as_array().unwrap().len(),
            2
        );
        server.abort();
    }
}
