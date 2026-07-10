//! Dual-backend parity contract for the model/endpoint stores.
//!
//! `provider_store.rs` (SQLite) and `postgres_provider_store.rs` (Postgres)
//! are parallel hand-written implementations that must behave identically.
//! Nothing in the type system enforces that — divergences have shipped before
//! (cascade mechanism, malformed-id handling, duplicated crypto) — so this
//! module states the shared behavior once and runs every assertion against
//! each available backend.
//!
//! SQLite always runs (throwaway file DB). The Postgres arm runs when
//! `TEST_POSTGRES_URL` points at a reachable server (e.g.
//! `docker run -d -e POSTGRES_PASSWORD=test -p 5433:5432 postgres:16-alpine`
//! then `TEST_POSTGRES_URL=postgres://postgres:test@localhost:5433/postgres
//! cargo test -p himadri-admin --features postgres`); otherwise it is skipped
//! with a note. Postgres state is shared across runs, so tests only assert on
//! rows they created (unique names, no "table is empty" assumptions) and
//! clean up after themselves.

use crate::crypto::CipherKey;
use crate::error::AdminError;
use crate::models::{
    CreateModelEndpointRequest, CreateModelRequest, UpdateModelEndpointRequest, UpdateModelRequest,
};
use crate::provider_backend::{connect_model_stores, ModelEndpointStoreBackend, ModelStoreBackend};

struct Backend {
    label: &'static str,
    models: ModelStoreBackend,
    endpoints: ModelEndpointStoreBackend,
    sqlite_path: Option<std::path::PathBuf>,
}

fn parity_cipher() -> CipherKey {
    CipherKey::from_base64("MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=").unwrap()
}

async fn backends() -> Vec<Backend> {
    let mut backends = Vec::new();

    let path = std::env::temp_dir().join(format!("himadri-parity-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let (models, endpoints) = connect_model_stores(&url, Some(parity_cipher()))
        .await
        .expect("sqlite parity stores should connect");
    backends.push(Backend {
        label: "sqlite",
        models,
        endpoints,
        sqlite_path: Some(path),
    });

    #[cfg(feature = "postgres")]
    match std::env::var("TEST_POSTGRES_URL") {
        Ok(url) => {
            let (models, endpoints) = connect_model_stores(&url, Some(parity_cipher()))
                .await
                .expect("postgres parity stores should connect");
            backends.push(Backend {
                label: "postgres",
                models,
                endpoints,
                sqlite_path: None,
            });
        }
        Err(_) => {
            eprintln!(
                "store parity: TEST_POSTGRES_URL not set — running the contract against SQLite only"
            );
        }
    }

    backends
}

/// Best-effort teardown: disable + delete the given models (cascading their
/// endpoints), and drop the throwaway SQLite file.
async fn cleanup(b: &Backend, model_ids: &[&str]) {
    for id in model_ids {
        let _ = b.models.toggle(id, false).await;
        let _ = b.models.delete(id).await;
    }
    if let Some(path) = &b.sqlite_path {
        let _ = std::fs::remove_file(path);
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

fn model_req(name: &str, enabled: bool) -> CreateModelRequest {
    CreateModelRequest {
        name: name.to_string(),
        display_name: Some("Parity Display".to_string()),
        enabled,
    }
}

fn ep_req(api_key: Option<&str>) -> CreateModelEndpointRequest {
    CreateModelEndpointRequest {
        provider_type: "openai".to_string(),
        base_url: Some("https://api.openai.com/v1".to_string()),
        api_key: api_key.map(str::to_string),
        weight: 1.0,
        enabled: true,
    }
}

fn ep_update_none() -> UpdateModelEndpointRequest {
    UpdateModelEndpointRequest {
        provider_type: None,
        base_url: None,
        api_key: None,
        weight: None,
        enabled: None,
    }
}

#[tokio::test]
async fn model_lifecycle_contract() {
    for b in backends().await {
        let name = unique("parity-model");
        let created = b
            .models
            .create(model_req(&name, true))
            .await
            .unwrap_or_else(|e| panic!("[{}] create: {e}", b.label));
        assert!(created.enabled, "[{}]", b.label);

        let fetched = b.models.get(&created.id).await.unwrap().unwrap();
        assert_eq!(fetched.name, name, "[{}]", b.label);
        assert_eq!(
            fetched.display_name.as_deref(),
            Some("Parity Display"),
            "[{}]",
            b.label
        );

        assert!(
            b.models
                .list()
                .await
                .unwrap()
                .iter()
                .any(|m| m.id == created.id),
            "[{}] list must contain the created model",
            b.label
        );

        // Partial update: rename and clear the display name in one call.
        let renamed = unique("parity-renamed");
        let updated = b
            .models
            .update(
                &created.id,
                UpdateModelRequest {
                    name: Some(renamed.clone()),
                    display_name: Some(None),
                    enabled: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, renamed, "[{}]", b.label);
        assert_eq!(updated.display_name, None, "[{}]", b.label);
        assert!(
            updated.enabled,
            "[{}] update must not flip enabled",
            b.label
        );

        let toggled = b.models.toggle(&created.id, false).await.unwrap().unwrap();
        assert!(!toggled.enabled, "[{}]", b.label);

        assert_eq!(
            b.models.delete(&created.id).await,
            Ok(true),
            "[{}]",
            b.label
        );
        assert_eq!(
            b.models.get(&created.id).await.unwrap(),
            None,
            "[{}]",
            b.label
        );

        cleanup(&b, &[]).await;
    }
}

#[tokio::test]
async fn enabled_model_delete_is_conflict_contract() {
    for b in backends().await {
        let m = b
            .models
            .create(model_req(&unique("parity-live"), true))
            .await
            .unwrap();

        // Deleting a live model must be a Conflict (409), never "not found"
        // or a store failure — on both backends.
        assert!(
            matches!(b.models.delete(&m.id).await, Err(AdminError::Conflict(_))),
            "[{}] deleting an enabled model must be a Conflict",
            b.label
        );

        b.models.toggle(&m.id, false).await.unwrap();
        assert_eq!(b.models.delete(&m.id).await, Ok(true), "[{}]", b.label);

        cleanup(&b, &[]).await;
    }
}

#[tokio::test]
async fn model_delete_cascades_to_endpoints_contract() {
    for b in backends().await {
        let m = b
            .models
            .create(model_req(&unique("parity-cascade"), true))
            .await
            .unwrap();
        let e1 = b
            .endpoints
            .create(&m.id, ep_req(Some("sk-one")))
            .await
            .unwrap();
        let e2 = b
            .endpoints
            .create(&m.id, ep_req(Some("sk-two")))
            .await
            .unwrap();
        assert_eq!(
            b.endpoints.list_by_model(&m.id).await.unwrap().len(),
            2,
            "[{}]",
            b.label
        );

        b.models.toggle(&m.id, false).await.unwrap();
        assert_eq!(b.models.delete(&m.id).await, Ok(true), "[{}]", b.label);

        // The cascade mechanism differs by design (application code on
        // SQLite, `ON DELETE CASCADE` FK on Postgres) but the outcome must
        // not: no orphaned endpoints.
        assert!(
            b.endpoints.list_by_model(&m.id).await.unwrap().is_empty(),
            "[{}] model delete must leave no orphaned endpoints",
            b.label
        );
        assert_eq!(
            b.endpoints.get(&e1.id).await.unwrap(),
            None,
            "[{}]",
            b.label
        );
        assert_eq!(
            b.endpoints.get(&e2.id).await.unwrap(),
            None,
            "[{}]",
            b.label
        );

        cleanup(&b, &[]).await;
    }
}

#[tokio::test]
async fn endpoint_lifecycle_and_key_contract() {
    for b in backends().await {
        let m = b
            .models
            .create(model_req(&unique("parity-ep"), true))
            .await
            .unwrap();

        // Create returns the plaintext key (for immediate internal use); the
        // stored value round-trips decrypted.
        let ep = b
            .endpoints
            .create(&m.id, ep_req(Some("sk-parity-secret")))
            .await
            .unwrap();
        assert_eq!(
            ep.api_key.as_deref(),
            Some("sk-parity-secret"),
            "[{}]",
            b.label
        );
        let fetched = b.endpoints.get(&ep.id).await.unwrap().unwrap();
        assert_eq!(
            fetched.api_key.as_deref(),
            Some("sk-parity-secret"),
            "[{}] key must round-trip decrypted",
            b.label
        );

        // A partial update that omits api_key must leave the stored key
        // untouched (the decrypt-failure/credential-wipe regression).
        let updated = b
            .endpoints
            .update(
                &ep.id,
                UpdateModelEndpointRequest {
                    weight: Some(2.5),
                    ..ep_update_none()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.weight, 2.5, "[{}]", b.label);
        assert_eq!(
            updated.api_key.as_deref(),
            Some("sk-parity-secret"),
            "[{}] weight-only update must preserve the key",
            b.label
        );

        // `api_key: Some(Some(_))` rotates; `Some(None)` clears.
        let rotated = b
            .endpoints
            .update(
                &ep.id,
                UpdateModelEndpointRequest {
                    api_key: Some(Some("sk-rotated".to_string())),
                    ..ep_update_none()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            rotated.api_key.as_deref(),
            Some("sk-rotated"),
            "[{}]",
            b.label
        );
        let cleared = b
            .endpoints
            .update(
                &ep.id,
                UpdateModelEndpointRequest {
                    api_key: Some(None),
                    ..ep_update_none()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cleared.api_key, None, "[{}]", b.label);

        // `base_url: None` leaves it alone; `Some(None)` clears it.
        let kept = b
            .endpoints
            .update(&ep.id, ep_update_none())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            kept.base_url.as_deref(),
            Some("https://api.openai.com/v1"),
            "[{}]",
            b.label
        );
        let no_url = b
            .endpoints
            .update(
                &ep.id,
                UpdateModelEndpointRequest {
                    base_url: Some(None),
                    ..ep_update_none()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(no_url.base_url, None, "[{}]", b.label);

        let toggled = b.endpoints.toggle(&ep.id, false).await.unwrap().unwrap();
        assert!(!toggled.enabled, "[{}]", b.label);

        assert_eq!(b.endpoints.delete(&ep.id).await, Ok(true), "[{}]", b.label);
        assert_eq!(
            b.endpoints.get(&ep.id).await.unwrap(),
            None,
            "[{}]",
            b.label
        );
        assert_eq!(
            b.endpoints.delete(&ep.id).await,
            Ok(false),
            "[{}] second delete is not-found, not an error",
            b.label
        );

        cleanup(&b, &[&m.id]).await;
    }
}

/// Missing rows and ids that cannot possibly match (malformed UUIDs on the
/// Postgres backend, arbitrary strings on SQLite) must behave identically:
/// "not found", never a store error. Postgres previously surfaced malformed
/// ids as `Err(RowNotFound)` — a 409/500 at the HTTP layer — while SQLite
/// returned a clean miss.
#[tokio::test]
async fn missing_and_malformed_ids_contract() {
    let missing = uuid::Uuid::new_v4().to_string(); // valid shape, no row
    for b in backends().await {
        for id in ["not-a-uuid", missing.as_str()] {
            assert_eq!(b.models.get(id).await, Ok(None), "[{}] get {id}", b.label);
            assert_eq!(
                b.models.toggle(id, true).await,
                Ok(None),
                "[{}] toggle {id}",
                b.label
            );
            assert_eq!(
                b.models.delete(id).await,
                Ok(false),
                "[{}] delete {id}",
                b.label
            );
            assert!(
                matches!(
                    b.models
                        .update(
                            id,
                            UpdateModelRequest {
                                name: Some("x".to_string()),
                                display_name: None,
                                enabled: None,
                            }
                        )
                        .await,
                    Err(AdminError::NotFound)
                ),
                "[{}] update {id}",
                b.label
            );

            assert_eq!(
                b.endpoints.get(id).await,
                Ok(None),
                "[{}] ep get {id}",
                b.label
            );
            assert_eq!(
                b.endpoints.toggle(id, true).await,
                Ok(None),
                "[{}] ep toggle {id}",
                b.label
            );
            assert_eq!(
                b.endpoints.delete(id).await,
                Ok(false),
                "[{}] ep delete {id}",
                b.label
            );
            assert!(
                matches!(
                    b.endpoints.update(id, ep_update_none()).await,
                    Err(AdminError::NotFound)
                ),
                "[{}] ep update {id}",
                b.label
            );
            assert_eq!(
                b.endpoints.list_by_model(id).await,
                Ok(vec![]),
                "[{}] list_by_model {id}",
                b.label
            );
            // An endpoint may only be created under an existing model.
            assert!(
                matches!(
                    b.endpoints.create(id, ep_req(None)).await,
                    Err(AdminError::NotFound)
                ),
                "[{}] ep create under {id}",
                b.label
            );
        }
        cleanup(&b, &[]).await;
    }
}

/// The Postgres cascade relies on the `ON DELETE CASCADE` FK from migration
/// 004 (SQLite cascades in application code — see `ModelStore::delete`).
/// Assert the constraint actually exists so a future migration can't drop it
/// and silently start orphaning endpoints.
#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_cascade_fk_constraint_exists() {
    let Ok(url) = std::env::var("TEST_POSTGRES_URL") else {
        eprintln!("skipping postgres_cascade_fk_constraint_exists: TEST_POSTGRES_URL not set");
        return;
    };
    // Connect through the normal path first so migrations have run.
    let _ = connect_model_stores(&url, None)
        .await
        .expect("postgres stores should connect");

    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let cascade_fks: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.referential_constraints rc
         JOIN information_schema.table_constraints tc
           ON tc.constraint_name = rc.constraint_name
          AND tc.constraint_schema = rc.constraint_schema
         WHERE tc.table_name = 'model_endpoints'
           AND rc.delete_rule = 'CASCADE'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        cascade_fks >= 1,
        "model_endpoints must keep its ON DELETE CASCADE FK to models"
    );
}
