use crate::models::{
    CreateModelRequest, CreateProviderRequest, Model, Provider, UpdateModelRequest,
    UpdateProviderRequest,
};
use crate::provider_backend::{ModelStoreBackend, ProviderStoreBackend};
use crate::store::StoreBackend;
use crate::{ApiKey, CreateApiKeyRequest, UpdateApiKeyRequest};

pub struct AdminHandlers {
    store: StoreBackend,
    provider_store: Option<ProviderStoreBackend>,
    model_store: Option<ModelStoreBackend>,
}

/// Log a store error — otherwise swallowed by the `Option`/`bool` return of the
/// handler methods — so an operator can see *why* a provider/model mutation
/// failed (a DB outage, or a validation reason like "provider has models")
/// instead of only a bare 404/500, then discard it.
fn logged<T>(op: &str, result: Result<T, String>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("{op} failed: {e}");
            None
        }
    }
}

impl AdminHandlers {
    pub fn new(store: StoreBackend) -> Self {
        Self {
            store,
            provider_store: None,
            model_store: None,
        }
    }

    pub fn with_provider_model_stores(
        mut self,
        provider_store: ProviderStoreBackend,
        model_store: ModelStoreBackend,
    ) -> Self {
        self.provider_store = Some(provider_store);
        self.model_store = Some(model_store);
        self
    }

    pub fn provider_store(&self) -> Option<&ProviderStoreBackend> {
        self.provider_store.as_ref()
    }

    pub fn model_store(&self) -> Option<&ModelStoreBackend> {
        self.model_store.as_ref()
    }

    // ─── Key Management ───────────────────────────────────────────────

    pub async fn list_keys(&self) -> Vec<ApiKey> {
        self.store.list().await.unwrap_or_default()
    }

    pub async fn create_key(&self, request: CreateApiKeyRequest) -> Result<ApiKey, String> {
        self.store.create(request).await
    }

    pub async fn get_key(&self, id: &str) -> Option<ApiKey> {
        self.store.get(id).await.unwrap_or(None)
    }

    pub async fn update_key(&self, id: &str, request: UpdateApiKeyRequest) -> Option<ApiKey> {
        self.store.update(id, request).await.unwrap_or(None)
    }

    pub async fn delete_key(&self, id: &str) -> bool {
        self.store.delete(id).await.unwrap_or(false)
    }

    pub async fn revoke_key(&self, id: &str) -> bool {
        self.store.revoke(id).await.unwrap_or(false)
    }

    pub async fn rotate_key(&self, id: &str) -> Option<ApiKey> {
        self.store.rotate(id).await.unwrap_or(None)
    }

    // ─── Provider Management ──────────────────────────────────────────

    pub async fn list_providers(&self) -> Vec<Provider> {
        match &self.provider_store {
            Some(s) => s.list().await.unwrap_or_default(),
            None => vec![],
        }
    }

    pub async fn list_enabled_providers(&self) -> Vec<Provider> {
        match &self.provider_store {
            Some(s) => s.list_enabled().await.unwrap_or_default(),
            None => vec![],
        }
    }

    pub async fn create_provider(&self, request: CreateProviderRequest) -> Option<Provider> {
        if let Some(url) = &request.base_url {
            if let Err(reason) = himadri_core::provider_url_is_allowed(
                url,
                himadri_core::allow_private_provider_urls(),
            ) {
                tracing::warn!("Rejected provider base_url on create: {reason}");
                return None;
            }
        }
        match &self.provider_store {
            Some(s) => logged("create_provider", s.create(request).await),
            None => None,
        }
    }

    pub async fn get_provider(&self, id: &str) -> Option<Provider> {
        match &self.provider_store {
            Some(s) => s.get(id).await.ok().flatten(),
            None => None,
        }
    }

    pub async fn update_provider(
        &self,
        id: &str,
        request: UpdateProviderRequest,
    ) -> Option<Provider> {
        // Outer Some(Some(url)) means "set base_url to url"; validate it.
        if let Some(Some(url)) = &request.base_url {
            if let Err(reason) = himadri_core::provider_url_is_allowed(
                url,
                himadri_core::allow_private_provider_urls(),
            ) {
                tracing::warn!("Rejected provider base_url on update: {reason}");
                return None;
            }
        }
        match &self.provider_store {
            Some(s) => logged("update_provider", s.update(id, request).await).flatten(),
            None => None,
        }
    }

    pub async fn delete_provider(&self, id: &str) -> bool {
        match &self.provider_store {
            Some(s) => logged("delete_provider", s.delete(id).await).unwrap_or(false),
            None => false,
        }
    }

    pub async fn toggle_provider(&self, id: &str, enabled: bool) -> Option<Provider> {
        match &self.provider_store {
            Some(s) => logged("toggle_provider", s.toggle(id, enabled).await).flatten(),
            None => None,
        }
    }

    // ─── Model Management ─────────────────────────────────────────────

    pub async fn list_models(&self) -> Vec<Model> {
        match &self.model_store {
            Some(s) => s.list().await.unwrap_or_default(),
            None => vec![],
        }
    }

    pub async fn list_models_by_provider(&self, provider_id: &str) -> Vec<Model> {
        match &self.model_store {
            Some(s) => s.list_by_provider(provider_id).await.unwrap_or_default(),
            None => vec![],
        }
    }

    pub async fn list_enabled_models(&self) -> Vec<Model> {
        match &self.model_store {
            Some(s) => s.list_enabled().await.unwrap_or_default(),
            None => vec![],
        }
    }

    pub async fn create_model(&self, request: CreateModelRequest) -> Option<Model> {
        match &self.model_store {
            Some(s) => logged("create_model", s.create(request).await),
            None => None,
        }
    }

    pub async fn get_model(&self, id: &str) -> Option<Model> {
        match &self.model_store {
            Some(s) => s.get(id).await.ok().flatten(),
            None => None,
        }
    }

    pub async fn update_model(&self, id: &str, request: UpdateModelRequest) -> Option<Model> {
        match &self.model_store {
            Some(s) => logged("update_model", s.update(id, request).await).flatten(),
            None => None,
        }
    }

    pub async fn delete_model(&self, id: &str) -> bool {
        match &self.model_store {
            Some(s) => logged("delete_model", s.delete(id).await).unwrap_or(false),
            None => false,
        }
    }

    pub async fn toggle_model(&self, id: &str, enabled: bool) -> Option<Model> {
        match &self.model_store {
            Some(s) => logged("toggle_model", s.toggle(id, enabled).await).flatten(),
            None => None,
        }
    }

    pub async fn list_enabled_models_for_api(&self) -> Vec<himadri_core::ModelObject> {
        let models = self.list_enabled_models().await;
        let providers = self.list_providers().await;
        let provider_map: std::collections::HashMap<String, String> = providers
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();

        models
            .into_iter()
            .filter_map(|m| {
                let owned_by = provider_map.get(&m.provider_id)?.clone();
                Some(himadri_core::ModelObject {
                    id: m.name.clone(),
                    object: "model".to_string(),
                    created: m.created_at.timestamp() as u64,
                    owned_by,
                })
            })
            .collect()
    }
}
