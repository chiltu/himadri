use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use himadri_core::Usage;
use himadri_plugin::context::PluginContext;
use himadri_plugin::traits::{Plugin, PluginError, PluginType, Stage};

// ─── Global Spend Store ──────────────────────────────────────────────

/// Process-level registry of spend stores keyed by store_id.
static GLOBAL_STORES: once_cell::sync::Lazy<parking_lot::RwLock<HashMap<String, Arc<SpendStore>>>> =
    once_cell::sync::Lazy::new(|| parking_lot::RwLock::new(HashMap::new()));

/// Accumulates per-key USD spend with optional key count cap.
struct SpendStore {
    inner: parking_lot::RwLock<SpendStoreInner>,
}

struct SpendStoreInner {
    spend: HashMap<String, f64>,
    max_keys: usize,
}

impl SpendStore {
    fn new(max_keys: usize) -> Self {
        Self {
            inner: parking_lot::RwLock::new(SpendStoreInner {
                spend: HashMap::new(),
                max_keys,
            }),
        }
    }

    fn add(&self, key: &str, usd: f64) {
        let mut inner = self.inner.write();
        let exists = inner.spend.contains_key(key);
        if !exists && inner.max_keys > 0 && inner.spend.len() >= inner.max_keys {
            // Evict the key with the lowest accumulated spend
            if let Some((min_key, _min_val)) = inner
                .spend
                .iter()
                .min_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            {
                let min_key = min_key.clone();
                inner.spend.remove(&min_key);
            }
        }
        *inner.spend.entry(key.to_string()).or_insert(0.0) += usd;
    }

    fn get(&self, key: &str) -> f64 {
        let inner = self.inner.read();
        inner.spend.get(key).copied().unwrap_or(0.0)
    }

    fn reset(&self, key: &str) {
        let mut inner = self.inner.write();
        inner.spend.remove(key);
    }

    fn reset_all(&self) {
        let mut inner = self.inner.write();
        inner.spend.clear();
    }
}

fn get_or_create_store(store_id: &str, max_keys: usize) -> Arc<SpendStore> {
    let stores = GLOBAL_STORES.read();
    if let Some(store) = stores.get(store_id) {
        return store.clone();
    }
    drop(stores);

    let mut stores = GLOBAL_STORES.write();
    stores
        .entry(store_id.to_string())
        .or_insert_with(|| Arc::new(SpendStore::new(max_keys)))
        .clone()
}

/// Remove accumulated spend for a specific API key from the named store.
pub fn reset_store_key(store_id: &str, api_key: &str) {
    let stores = GLOBAL_STORES.read();
    if let Some(store) = stores.get(store_id) {
        store.reset(api_key);
    }
}

/// Clear all accumulated spend for every key in the named store.
pub fn reset_store(store_id: &str) {
    let stores = GLOBAL_STORES.read();
    if let Some(store) = stores.get(store_id) {
        store.reset_all();
    }
}

/// Get current spend for a key (for admin APIs / dashboard).
pub fn get_spend(store_id: &str, api_key: &str) -> f64 {
    let stores = GLOBAL_STORES.read();
    stores.get(store_id).map(|s| s.get(api_key)).unwrap_or(0.0)
}

/// Get all spend records for a store (for admin APIs / dashboard).
pub fn get_all_spend(store_id: &str) -> HashMap<String, f64> {
    let stores = GLOBAL_STORES.read();
    stores
        .get(store_id)
        .map(|s| s.inner.read().spend.clone())
        .unwrap_or_default()
}

// ─── Budget Plugin ───────────────────────────────────────────────────

const DEFAULT_MAX_KEYS: usize = 10_000;

#[allow(dead_code)]
pub struct BudgetPlugin {
    store_id: String,
    spend_limit_usd: f64,
    input_per_m_tokens: f64,
    output_per_m_tokens: f64,
    store: Arc<SpendStore>,
}

impl BudgetPlugin {
    /// Create a new BudgetPlugin with the given configuration.
    ///
    /// # Configuration
    /// - `store_id`: Shared store identifier (default: "default")
    /// - `spend_limit_usd`: Max cumulative spend per API key in USD (0 = unlimited)
    /// - `input_per_m_tokens`: Cost per 1M prompt tokens (USD)
    /// - `output_per_m_tokens`: Cost per 1M completion tokens (USD)
    /// - `max_keys`: Max tracked keys per store (default: 10000)
    pub fn new(config: BudgetConfig) -> Result<Arc<Self>, String> {
        let store_id = config.store_id.unwrap_or_else(|| "default".to_string());
        let spend_limit_usd = config.spend_limit_usd.unwrap_or(0.0);
        let input_per_m_tokens = config.input_per_m_tokens.unwrap_or(0.0);
        let output_per_m_tokens = config.output_per_m_tokens.unwrap_or(0.0);
        let max_keys = config.max_keys.unwrap_or(DEFAULT_MAX_KEYS);

        if spend_limit_usd < 0.0 {
            return Err("spend_limit_usd must be >= 0".to_string());
        }

        if spend_limit_usd > 0.0 && input_per_m_tokens == 0.0 && output_per_m_tokens == 0.0 {
            return Err(
                "spend_limit_usd is set but both input_per_m_tokens and output_per_m_tokens are 0; \
                 cost will always be 0 and the budget limit will never be enforced"
                    .to_string(),
            );
        }

        let store = get_or_create_store(&store_id, max_keys);

        Ok(Arc::new(Self {
            store_id,
            spend_limit_usd,
            input_per_m_tokens,
            output_per_m_tokens,
            store,
        }))
    }

    fn calculate_cost(&self, usage: &Usage) -> f64 {
        (usage.prompt_tokens as f64 / 1_000_000.0) * self.input_per_m_tokens
            + (usage.completion_tokens as f64 / 1_000_000.0) * self.output_per_m_tokens
    }

    fn get_api_key(ctx: &PluginContext) -> Option<String> {
        // Try metadata first (for backward compat)
        if let Some(key) = ctx.get_metadata("api_key") {
            if let Some(s) = key.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
        // Fall back to auth context
        ctx.auth.as_ref().map(|a| a.api_key.clone())
    }
}

#[derive(Debug, Clone, Default)]
pub struct BudgetConfig {
    pub store_id: Option<String>,
    pub spend_limit_usd: Option<f64>,
    pub input_per_m_tokens: Option<f64>,
    pub output_per_m_tokens: Option<f64>,
    pub max_keys: Option<usize>,
}

#[async_trait]
impl Plugin for BudgetPlugin {
    fn name(&self) -> &str {
        "budget"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Middleware
    }

    fn stage(&self) -> Stage {
        Stage::BeforeRequest
    }

    fn also_after_request(&self) -> bool {
        // Run again after the request so we can record cost from the response.
        true
    }

    async fn execute(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let key = match Self::get_api_key(ctx) {
            Some(k) => k,
            None => return Ok(()), // No API key — skip per-key budget tracking
        };

        if ctx.response.is_some() {
            // After-request stage: record cost
            self.record_cost(ctx, &key);
        } else {
            // Before-request stage: check budget. A per-principal cap from the
            // auth context (e.g. JWT `budget_limit_usd`) overrides the global one.
            let per_principal = ctx.auth.as_ref().and_then(|a| a.budget_limit_usd);
            self.check_budget(&key, per_principal)?;
        }

        Ok(())
    }
}

impl BudgetPlugin {
    /// Resolve the effective cumulative spend cap: a positive per-principal cap
    /// takes precedence over the gateway's global limit. `0`/`None` means the
    /// global limit applies; a global limit of `0` means unlimited.
    fn effective_limit(&self, per_principal: Option<f64>) -> f64 {
        match per_principal {
            Some(limit) if limit > 0.0 => limit,
            _ => self.spend_limit_usd,
        }
    }

    fn check_budget(&self, key: &str, per_principal: Option<f64>) -> Result<(), PluginError> {
        let limit = self.effective_limit(per_principal);
        if limit <= 0.0 {
            return Ok(()); // Unlimited
        }

        let current = self.store.get(key);
        if current >= limit {
            return Err(PluginError::Rejected {
                name: self.name().to_string(),
                reason: format!(
                    "budget exceeded: spent ${:.4} of ${:.2} limit",
                    current, limit
                ),
            });
        }

        Ok(())
    }

    fn record_cost(&self, ctx: &PluginContext, key: &str) {
        if let Some(ref response) = ctx.response {
            if let Some(ref usage) = response.usage {
                let cost = self.calculate_cost(usage);
                if cost > 0.0 {
                    self.store.add(key, cost);
                }
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use himadri_core::{ChatCompletionRequest, Message, MessageContent, Role};

    fn make_request_ctx(api_key: Option<&str>) -> PluginContext {
        let mut ctx = PluginContext::from_request(
            &ChatCompletionRequest {
                model: "test".to_string(),
                messages: vec![Message {
                    role: Role::User,
                    content: Some(MessageContent::Text("Hello".to_string())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                }],
                stream: false,
                temperature: None,
                top_p: None,
                max_tokens: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                user: None,
                tools: None,
                tool_choice: None,
                extra: Default::default(),
            },
            None,
        );
        if let Some(key) = api_key {
            ctx.set_metadata(
                "api_key".to_string(),
                serde_json::Value::String(key.to_string()),
            );
        }
        ctx
    }

    fn make_response_ctx(
        api_key: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
    ) -> PluginContext {
        let mut ctx = make_request_ctx(Some(api_key));
        ctx.set_response(himadri_core::ChatCompletionResponse {
            id: "test".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "test".to_string(),
            choices: vec![],
            usage: Some(himadri_core::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            }),
            system_fingerprint: None,
        });
        ctx
    }

    fn auth_with_budget(api_key: &str, budget: Option<f64>) -> himadri_core::AuthContext {
        himadri_core::AuthContext {
            api_key: api_key.to_string(),
            key_id: None,
            scope: himadri_core::AuthScope::ApiKey,
            org_id: None,
            team_id: None,
            user_id: None,
            rate_limit_override: None,
            roles: Vec::new(),
            budget_limit_usd: budget,
        }
    }

    #[tokio::test]
    async fn test_per_principal_limit_overrides_global() {
        // Global limit is generous; the principal's own $0.001 cap is stricter.
        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-per-principal".to_string()),
            spend_limit_usd: Some(1000.0),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        let api_key = "jwt:user-pp";

        // Record (100/1M*3)+(50/1M*15) = 0.00105 USD against the principal.
        let mut after = make_response_ctx(api_key, 100, 50);
        after.auth = Some(auth_with_budget(api_key, Some(0.001)));
        plugin.execute(&mut after).await.unwrap();

        // Below the generous global limit, but over the $0.001 per-principal cap.
        let mut before = make_request_ctx(Some(api_key));
        before.auth = Some(auth_with_budget(api_key, Some(0.001)));
        let result = plugin.execute(&mut before).await;
        assert!(matches!(result, Err(PluginError::Rejected { .. })));
    }

    #[tokio::test]
    async fn test_per_principal_limit_with_no_global() {
        // No global limit (0 = unlimited globally); enforcement comes purely
        // from the per-principal cap.
        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-pp-no-global".to_string()),
            spend_limit_usd: Some(0.0),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        let api_key = "jwt:user-pp2";

        let mut after = make_response_ctx(api_key, 100, 50);
        after.auth = Some(auth_with_budget(api_key, Some(0.001)));
        plugin.execute(&mut after).await.unwrap();

        let mut before = make_request_ctx(Some(api_key));
        before.auth = Some(auth_with_budget(api_key, Some(0.001)));
        assert!(plugin.execute(&mut before).await.is_err());

        // A different principal with no cap is unaffected (unlimited).
        let mut other = make_request_ctx(Some("jwt:user-free"));
        other.auth = Some(auth_with_budget("jwt:user-free", None));
        assert!(plugin.execute(&mut other).await.is_ok());
    }

    #[tokio::test]
    async fn test_records_cost_through_plugin_manager() {
        // Regression: the budget plugin must record cost during the after-request
        // stage when driven through PluginManager (it registers in both stages).
        use himadri_plugin::manager::PluginManager;

        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-via-manager".to_string()),
            spend_limit_usd: Some(0.001),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        let mut manager = PluginManager::new();
        manager.register(plugin);

        let api_key = "key-mgr";

        // First request passes the budget check.
        let mut before = make_request_ctx(Some(api_key));
        manager.run_before(&mut before).await.unwrap();

        // After the request, cost is recorded via the after-request stage.
        let mut after = make_response_ctx(api_key, 100, 50);
        manager.run_after(&mut after).await.unwrap();

        // The next request is now over budget.
        let mut before2 = make_request_ctx(Some(api_key));
        let result = manager.run_before(&mut before2).await;
        assert!(matches!(result, Err(PluginError::Rejected { .. })));
    }

    #[test]
    fn test_init_defaults() {
        let plugin = BudgetPlugin::new(BudgetConfig::default()).unwrap();
        assert_eq!(plugin.store_id, "default");
        assert_eq!(plugin.spend_limit_usd, 0.0);
    }

    #[test]
    fn test_init_invalid_type() {
        let result = BudgetPlugin::new(BudgetConfig {
            spend_limit_usd: Some(-1.0),
            ..Default::default()
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_init_zero_pricing_with_limit() {
        let result = BudgetPlugin::new(BudgetConfig {
            spend_limit_usd: Some(10.0),
            input_per_m_tokens: Some(0.0),
            output_per_m_tokens: Some(0.0),
            ..Default::default()
        });
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_no_api_key_skips() {
        let plugin = BudgetPlugin::new(BudgetConfig {
            spend_limit_usd: Some(0.01),
            input_per_m_tokens: Some(1.0),
            output_per_m_tokens: Some(1.0),
            ..Default::default()
        })
        .unwrap();

        let mut ctx = make_request_ctx(None);
        assert!(plugin.execute(&mut ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_below_limit_passes() {
        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-below".to_string()),
            spend_limit_usd: Some(10.0),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        let mut ctx = make_request_ctx(Some("key-below"));
        assert!(plugin.execute(&mut ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_record_and_exceed() {
        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-exceed".to_string()),
            spend_limit_usd: Some(0.001),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        let api_key = "key-exceed";

        // Record cost: (100/1M * 3.0) + (50/1M * 15.0) = 0.00105 USD
        let mut after_ctx = make_response_ctx(api_key, 100, 50);
        assert!(plugin.execute(&mut after_ctx).await.is_ok());

        // Now check should reject
        let mut before_ctx = make_request_ctx(Some(api_key));
        let result = plugin.execute(&mut before_ctx).await;
        assert!(result.is_err());
        assert!(matches!(result, Err(PluginError::Rejected { .. })));
    }

    #[tokio::test]
    async fn test_unlimited_never_rejects() {
        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-unlimited".to_string()),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        let api_key = "key-unlimited";

        // Record huge cost
        let mut after_ctx = make_response_ctx(api_key, 1_000_000, 1_000_000);
        assert!(plugin.execute(&mut after_ctx).await.is_ok());

        // Should still pass
        let mut before_ctx = make_request_ctx(Some(api_key));
        assert!(plugin.execute(&mut before_ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_shared_store() {
        let config = BudgetConfig {
            store_id: Some("test-shared".to_string()),
            spend_limit_usd: Some(0.001),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        };

        let recorder = BudgetPlugin::new(config.clone()).unwrap();
        let checker = BudgetPlugin::new(config).unwrap();

        let api_key = "key-shared";

        // Record via one instance
        let mut after_ctx = make_response_ctx(api_key, 100, 50);
        assert!(recorder.execute(&mut after_ctx).await.is_ok());

        // Check via other instance — they share the same store
        let mut before_ctx = make_request_ctx(Some(api_key));
        let result = checker.execute(&mut before_ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_max_keys_evicts() {
        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-max-keys".to_string()),
            spend_limit_usd: Some(10.0),
            input_per_m_tokens: Some(1.0),
            output_per_m_tokens: Some(1.0),
            max_keys: Some(2),
        })
        .unwrap();

        // Record spend for two keys
        let mut ctx1 = make_response_ctx("low-spend", 1, 0);
        plugin.execute(&mut ctx1).await.unwrap();

        let mut ctx2 = make_response_ctx("high-spend", 10, 0);
        plugin.execute(&mut ctx2).await.unwrap();

        // Adding a third key must evict "low-spend" (min spend)
        let mut ctx3 = make_response_ctx("new-key", 5, 0);
        plugin.execute(&mut ctx3).await.unwrap();

        // low-spend should have been evicted
        assert_eq!(plugin.store.get("low-spend"), 0.0);
        // high-spend should still be present
        assert!(plugin.store.get("high-spend") > 0.0);
    }

    #[tokio::test]
    async fn test_reset_store_key() {
        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-reset-key".to_string()),
            spend_limit_usd: Some(0.001),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        let api_key = "key-to-reset";

        // Record spend that exceeds limit
        let mut after_ctx = make_response_ctx(api_key, 100, 50);
        plugin.execute(&mut after_ctx).await.unwrap();

        // Confirm over budget
        let mut before_ctx = make_request_ctx(Some(api_key));
        assert!(plugin.execute(&mut before_ctx).await.is_err());

        // Reset the key
        reset_store_key("test-reset-key", api_key);

        // Confirm budget is clear
        let mut before_ctx = make_request_ctx(Some(api_key));
        assert!(plugin.execute(&mut before_ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_reset_store() {
        let plugin = BudgetPlugin::new(BudgetConfig {
            store_id: Some("test-reset-all".to_string()),
            spend_limit_usd: Some(0.001),
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        for key in &["key-a", "key-b"] {
            let mut ctx = make_response_ctx(key, 100, 50);
            plugin.execute(&mut ctx).await.unwrap();
        }

        reset_store("test-reset-all");

        for key in &["key-a", "key-b"] {
            let mut ctx = make_request_ctx(Some(key));
            assert!(plugin.execute(&mut ctx).await.is_ok());
        }
    }

    #[tokio::test]
    async fn test_cost_calculation() {
        let plugin = BudgetPlugin::new(BudgetConfig {
            input_per_m_tokens: Some(3.0),
            output_per_m_tokens: Some(15.0),
            ..Default::default()
        })
        .unwrap();

        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };

        // (1000/1M * 3.0) + (500/1M * 15.0) = 0.003 + 0.0075 = 0.0105
        let cost = plugin.calculate_cost(&usage);
        assert!((cost - 0.0105).abs() < 0.0001);
    }
}
