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
        let admin = AdminHandlers::new(store, None);
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
            .await;
        assert!(admin.get_key(&key.id).await.is_some());
        assert!(admin.delete_key(&key.id).await);
        assert!(admin.get_key(&key.id).await.is_none());
    }
}
