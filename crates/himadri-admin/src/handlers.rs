use crate::models::{
    CreateModelRequest, CreateProviderRequest, Model, Provider, UpdateModelRequest,
    UpdateProviderRequest,
};
use crate::provider_store::{ModelStore, ProviderStore};
use crate::store::StoreBackend;
use crate::{ApiKey, CreateApiKeyRequest, UpdateApiKeyRequest};

pub struct AdminHandlers {
    store: StoreBackend,
    provider_store: Option<ProviderStore>,
    model_store: Option<ModelStore>,
    _master_key: Option<String>,
}

impl AdminHandlers {
    pub fn new(store: StoreBackend, master_key: Option<String>) -> Self {
        Self {
            store,
            provider_store: None,
            model_store: None,
            _master_key: master_key,
        }
    }

    pub fn with_provider_model_stores(
        mut self,
        provider_store: ProviderStore,
        model_store: ModelStore,
    ) -> Self {
        self.provider_store = Some(provider_store);
        self.model_store = Some(model_store);
        self
    }

    pub fn provider_store(&self) -> Option<&ProviderStore> {
        self.provider_store.as_ref()
    }

    pub fn model_store(&self) -> Option<&ModelStore> {
        self.model_store.as_ref()
    }

    // ─── Key Management ───────────────────────────────────────────────

    pub async fn list_keys(&self) -> Vec<ApiKey> {
        self.store.list().await.unwrap_or_default()
    }

    pub async fn create_key(&self, request: CreateApiKeyRequest) -> ApiKey {
        self.store.create(request).await.unwrap()
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
        match &self.provider_store {
            Some(s) => s.create(request).await.ok(),
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
        match &self.provider_store {
            Some(s) => s.update(id, request).await.ok().flatten(),
            None => None,
        }
    }

    pub async fn delete_provider(&self, id: &str) -> bool {
        match &self.provider_store {
            Some(s) => s.delete(id).await.unwrap_or(false),
            None => false,
        }
    }

    pub async fn toggle_provider(&self, id: &str, enabled: bool) -> Option<Provider> {
        match &self.provider_store {
            Some(s) => s.toggle(id, enabled).await.ok().flatten(),
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
            Some(s) => s.create(request).await.ok(),
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
            Some(s) => s.update(id, request).await.ok().flatten(),
            None => None,
        }
    }

    pub async fn delete_model(&self, id: &str) -> bool {
        match &self.model_store {
            Some(s) => s.delete(id).await.unwrap_or(false),
            None => false,
        }
    }

    pub async fn toggle_model(&self, id: &str, enabled: bool) -> Option<Model> {
        match &self.model_store {
            Some(s) => s.toggle(id, enabled).await.ok().flatten(),
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
