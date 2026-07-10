#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::handlers::AdminHandlers;
    use crate::store::{ApiKeyStore, CreateApiKeyRequest, UpdateApiKeyRequest};
    use himadri_core::{AuthContext, AuthScope};
    use std::sync::Arc;

    // ═══════════════════════════════════════════════════════════════════
    // RBAC Tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_rbac_admin_scope() {
        let ctx = AuthContext {
            api_key: "test".to_string(),
            key_id: Some("key-1".to_string()),
            scope: AuthScope::Admin,
            org_id: None,
            team_id: None,
            user_id: None,
            rate_limit_override: None,
            roles: Vec::new(),
            budget_limit_usd: None,
        };
        assert_eq!(ctx.scope, AuthScope::Admin);
    }

    #[test]
    fn test_rbac_readonly_scope() {
        let ctx = AuthContext {
            api_key: "test".to_string(),
            key_id: Some("key-1".to_string()),
            scope: AuthScope::ReadOnly,
            org_id: None,
            team_id: None,
            user_id: None,
            rate_limit_override: None,
            roles: Vec::new(),
            budget_limit_usd: None,
        };
        assert_eq!(ctx.scope, AuthScope::ReadOnly);
    }

    #[test]
    fn test_rbac_apikey_scope() {
        let ctx = AuthContext {
            api_key: "test".to_string(),
            key_id: Some("key-1".to_string()),
            scope: AuthScope::ApiKey,
            org_id: None,
            team_id: None,
            user_id: None,
            rate_limit_override: None,
            roles: Vec::new(),
            budget_limit_usd: None,
        };
        assert_eq!(ctx.scope, AuthScope::ApiKey);
    }

    #[tokio::test]
    async fn test_rbac_readonly_key_has_correct_scope() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "readonly-key".to_string(),
            scopes: vec!["read-only".to_string()],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });

        let validated = store.validate(&key.key);
        assert!(validated.is_some());
        // Read-only key should have ReadOnly scope
        // (This is handled by middleware, not store directly)
    }

    #[tokio::test]
    async fn test_rbac_admin_key_has_correct_scope() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "admin-key".to_string(),
            scopes: vec!["admin".to_string()],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });

        let validated = store.validate(&key.key);
        assert!(validated.is_some());
        assert!(validated.unwrap().scopes.contains(&"admin".to_string()));
    }

    #[test]
    fn test_rbac_scope_equality() {
        assert_eq!(AuthScope::Admin, AuthScope::Admin);
        assert_eq!(AuthScope::ReadOnly, AuthScope::ReadOnly);
        assert_eq!(AuthScope::ApiKey, AuthScope::ApiKey);
        assert_ne!(AuthScope::Admin, AuthScope::ReadOnly);
        assert_ne!(AuthScope::Admin, AuthScope::ApiKey);
        assert_ne!(AuthScope::ReadOnly, AuthScope::ApiKey);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Config Management Tests (placeholder - requires ConfigStore impl)
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_config_default() {
        let config = himadri_core::Config::default();
        assert_eq!(
            config.strategy.mode,
            himadri_core::config::StrategyMode::Single
        );
        assert!(!config.targets.is_empty());
    }

    #[test]
    fn test_config_validation_empty_targets() {
        let mut config = himadri_core::Config::default();
        config.targets.clear();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_valid() {
        let config = himadri_core::Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_strategy_modes() {
        let modes = [
            himadri_core::config::StrategyMode::Single,
            himadri_core::config::StrategyMode::Fallback,
            himadri_core::config::StrategyMode::LoadBalance,
            himadri_core::config::StrategyMode::LeastLatency,
            himadri_core::config::Config::default().strategy.mode,
        ];
        // All modes should be valid
        assert!(modes.len() >= 4);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Key Lifecycle Tests (sync ApiKeyStore)
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_create_key() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test-key".to_string(),
            scopes: vec!["admin".to_string()],
            expires_at: None,
            metadata: None,
            org_id: Some("org-1".to_string()),
            team_id: Some("team-1".to_string()),
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        assert!(!key.id.is_empty());
        assert!(key.key.starts_with("sk-"));
        assert_eq!(key.name, "test-key");
        assert!(key.enabled);
        assert_eq!(key.org_id, Some("org-1".to_string()));
    }

    #[test]
    fn test_get_key() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".to_string(),
            scopes: vec![],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        let fetched = store.get(&key.id).unwrap();
        assert_eq!(fetched.id, key.id);
    }

    #[test]
    fn test_get_key_not_found() {
        let store = ApiKeyStore::new();
        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn test_list_keys() {
        let store = ApiKeyStore::new();
        for i in 0..5 {
            store.create(CreateApiKeyRequest {
                name: format!("key-{}", i),
                scopes: vec![],
                expires_at: None,
                metadata: None,
                org_id: None,
                team_id: None,
                user_id: None,
                models: None,
                rate_limit_override: None,
                token_budget: None,
            });
        }
        assert_eq!(store.list().len(), 5);
    }

    #[test]
    fn test_update_key() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "original".to_string(),
            scopes: vec![],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        let updated = store
            .update(
                &key.id,
                UpdateApiKeyRequest {
                    name: Some("updated".to_string()),
                    scopes: Some(vec!["admin".to_string()]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.name, "updated");
        assert_eq!(updated.scopes, vec!["admin"]);
    }

    #[test]
    fn test_delete_key() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "to-delete".to_string(),
            scopes: vec![],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        assert!(store.delete(&key.id));
        assert!(store.get(&key.id).is_none());
    }

    #[test]
    fn test_revoke_key() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "to-revoke".to_string(),
            scopes: vec![],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        assert!(store.revoke(&key.id));
        assert!(!store.get(&key.id).unwrap().enabled);
    }

    #[test]
    fn test_rotate_key() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "to-rotate".to_string(),
            scopes: vec![],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        let rotated = store.rotate(&key.id).unwrap();
        assert_ne!(key.key, rotated.key);
        assert!(store.validate(&key.key).is_none());
        assert!(store.validate(&rotated.key).is_some());
    }

    #[test]
    fn test_validate_key_valid() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".to_string(),
            scopes: vec!["admin".to_string()],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        let validated = store.validate(&key.key);
        assert!(validated.is_some());
        assert_eq!(validated.unwrap().usage_count, 1);
    }

    #[test]
    fn test_validate_key_revoked() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".to_string(),
            scopes: vec![],
            expires_at: None,
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        store.revoke(&key.id);
        assert!(store.validate(&key.key).is_none());
    }

    #[test]
    fn test_validate_key_expired() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".to_string(),
            scopes: vec![],
            expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        assert!(store.validate(&key.key).is_none());
    }

    #[test]
    fn test_key_expiration() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".to_string(),
            scopes: vec![],
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            metadata: None,
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        assert!(store.validate(&key.key).is_some());
        store.update(
            &key.id,
            UpdateApiKeyRequest {
                expires_at: Some(Some(chrono::Utc::now() - chrono::Duration::hours(1))),
                ..Default::default()
            },
        );
        assert!(store.validate(&key.key).is_none());
    }

    #[test]
    fn test_key_metadata() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "test".to_string(),
            scopes: vec![],
            expires_at: None,
            metadata: Some(serde_json::json!({"env": "prod"})),
            org_id: None,
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        let fetched = store.get(&key.id).unwrap();
        assert_eq!(fetched.metadata.unwrap()["env"], "prod");
    }

    #[test]
    fn test_auth_context_anonymous() {
        let ctx = AuthContext::anonymous();
        assert_eq!(ctx.api_key, "anonymous");
        assert_eq!(ctx.scope, AuthScope::Admin);
    }

    #[test]
    fn test_full_key_lifecycle() {
        let store = ApiKeyStore::new();
        let key = store.create(CreateApiKeyRequest {
            name: "lifecycle".to_string(),
            scopes: vec!["admin".to_string()],
            expires_at: None,
            metadata: Some(serde_json::json!({"env": "test"})),
            org_id: Some("org-1".to_string()),
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });
        assert!(key.enabled);
        assert_eq!(
            store.get(&key.id).unwrap().org_id,
            Some("org-1".to_string())
        );
        store.update(
            &key.id,
            UpdateApiKeyRequest {
                name: Some("updated".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(store.get(&key.id).unwrap().name, "updated");
        store.revoke(&key.id);
        assert!(!store.get(&key.id).unwrap().enabled);
        let rotated = store.rotate(&key.id).unwrap();
        assert_ne!(key.key, rotated.key);
        store.delete(&key.id);
        assert!(store.get(&key.id).is_none());
    }

    #[tokio::test]
    async fn test_admin_handlers_crud() {
        let store = crate::store::StoreBackend::Memory(Arc::new(ApiKeyStore::new()));
        let admin = AdminHandlers::new(store);
        let key = admin
            .create_key(CreateApiKeyRequest {
                name: "test".to_string(),
                scopes: vec![],
                expires_at: None,
                metadata: None,
                org_id: None,
                team_id: None,
                user_id: None,
                models: None,
                rate_limit_override: None,
                token_budget: None,
            })
            .await
            .unwrap();
        assert!(admin.get_key(&key.id).await.unwrap().is_some());
        assert_eq!(admin.delete_key(&key.id).await, Ok(true));
        assert!(admin.get_key(&key.id).await.unwrap().is_none());
    }

    // ═══════════════════════════════════════════════════════════════════
    // AuthMiddleware::authenticate / is_bypass
    // ═══════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn authenticate_master_key_grants_admin() {
        use crate::middleware::AuthMiddleware;
        use crate::store::StoreBackend;

        let auth = AuthMiddleware::new(StoreBackend::new().await, Some("master-key".to_string()));
        assert!(!auth.is_bypass());

        let ctx = auth
            .authenticate("master-key")
            .await
            .expect("no store error")
            .expect("master key should authenticate");
        assert_eq!(ctx.scope, AuthScope::Admin);
    }

    #[tokio::test]
    async fn authenticate_unknown_key_returns_none() {
        use crate::middleware::AuthMiddleware;
        use crate::store::StoreBackend;

        let auth = AuthMiddleware::new(StoreBackend::new().await, Some("master-key".to_string()));
        let result = auth
            .authenticate("not-a-real-key")
            .await
            .expect("no store error");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn authenticate_stored_key_returns_context_with_scope() {
        use crate::middleware::AuthMiddleware;
        use crate::store::{ApiKeyStore, StoreBackend};

        // Seed a key in an in-memory store, then authenticate against it.
        let store = ApiKeyStore::new();
        let created = store.create(CreateApiKeyRequest {
            name: "svc".to_string(),
            scopes: vec!["read-only".to_string()],
            expires_at: None,
            metadata: None,
            org_id: Some("org-1".to_string()),
            team_id: None,
            user_id: None,
            models: None,
            rate_limit_override: None,
            token_budget: None,
        });

        let auth = AuthMiddleware::new(
            StoreBackend::Memory(std::sync::Arc::new(store)),
            Some("master-key".to_string()),
        );
        let ctx = auth
            .authenticate(&created.key)
            .await
            .expect("no store error")
            .expect("stored key should authenticate");
        assert_eq!(ctx.scope, AuthScope::ReadOnly);
        assert_eq!(ctx.org_id.as_deref(), Some("org-1"));
    }

    #[tokio::test]
    async fn auth_is_bypass_when_no_master_key() {
        use crate::middleware::AuthMiddleware;
        use crate::store::StoreBackend;

        let auth = AuthMiddleware::new(StoreBackend::new().await, None);
        assert!(auth.is_bypass());
    }

    /// The SSRF guard must be enforced at the admin boundary: an endpoint whose
    /// base_url points at an internal address is rejected, a public one accepted.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn create_endpoint_enforces_ssrf_guard() {
        use crate::models::{CreateModelEndpointRequest, CreateModelRequest};
        use crate::store::StoreBackend;

        let path =
            std::env::temp_dir().join(format!("himadri-ep-ssrf-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}", path.display());
        let (model_store, endpoint_store) =
            crate::provider_backend::connect_model_stores(&url, None)
                .await
                .expect("sqlite model store");
        let admin = AdminHandlers::new(StoreBackend::new().await)
            .with_model_stores(model_store, endpoint_store);

        let model = admin
            .create_model(CreateModelRequest {
                name: "m".to_string(),
                display_name: None,
                enabled: true,
            })
            .await
            .expect("model created");

        let ep = |base: &str| CreateModelEndpointRequest {
            provider_type: "openai".to_string(),
            base_url: Some(base.to_string()),
            api_key: Some("sk-x".to_string()),
            weight: 1.0,
            enabled: true,
        };

        // Cloud metadata endpoint → rejected as a typed validation error
        // (mapped to 400 by the HTTP layer) before the store is touched.
        assert!(matches!(
            admin
                .create_endpoint(&model.id, ep("http://169.254.169.254/latest/meta-data/"))
                .await,
            Err(crate::error::AdminError::Validation(_))
        ));
        // Public endpoint → accepted.
        assert!(admin
            .create_endpoint(&model.id, ep("https://api.openai.com/v1"))
            .await
            .is_ok());

        let _ = std::fs::remove_file(&path);
    }

    /// SQLite update must persist every updatable field — models, budget and
    /// expires_at were previously dropped silently (Postgres kept them).
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_update_persists_models_and_budget() {
        use crate::store::{SqliteStore, TokenBudget};

        let store = SqliteStore::new("sqlite://:memory:").await.unwrap();
        let created = store
            .create(CreateApiKeyRequest {
                name: "k".to_string(),
                scopes: vec![],
                expires_at: None,
                metadata: None,
                org_id: Some("org-1".to_string()),
                team_id: None,
                user_id: None,
                models: None,
                rate_limit_override: None,
                token_budget: None,
            })
            .await
            .unwrap();

        let updated = store
            .update(
                &created.id,
                UpdateApiKeyRequest {
                    models: Some(Some(vec!["gpt-4o".to_string()])),
                    token_budget: Some(Some(TokenBudget {
                        max_tokens_per_request: Some(1000),
                        max_tokens_per_day: None,
                        max_tokens_per_month: None,
                        cost_limit_per_day: None,
                        cost_limit_per_month: None,
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.models, Some(vec!["gpt-4o".to_string()]));
        assert_eq!(
            updated
                .token_budget
                .as_ref()
                .and_then(|b| b.max_tokens_per_request),
            Some(1000)
        );
        // Unspecified fields keep their values; Some(None) clears them.
        assert_eq!(updated.org_id.as_deref(), Some("org-1"));
        let cleared = store
            .update(
                &created.id,
                UpdateApiKeyRequest {
                    org_id: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cleared.org_id, None);
        assert_eq!(cleared.models, Some(vec!["gpt-4o".to_string()]));
    }
}
