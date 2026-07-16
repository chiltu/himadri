use crate::error::AdminError;
use crate::models::{
    CreateModelEndpointRequest, CreateModelRequest, Model, ModelEndpoint,
    UpdateModelEndpointRequest, UpdateModelRequest,
};
use crate::provider_backend::{ModelEndpointStoreBackend, ModelStoreBackend};
use crate::store::StoreBackend;
use crate::{ApiKey, CreateApiKeyRequest, UpdateApiKeyRequest};

pub struct AdminHandlers {
    store: StoreBackend,
    model_store: Option<ModelStoreBackend>,
    model_endpoint_store: Option<ModelEndpointStoreBackend>,
}

/// Log a store error before returning it, so an operator can see *why* a
/// mutation failed (a DB outage, or a guard like "model is enabled") in the
/// server log even when the client only sees the mapped HTTP status.
fn logged<T>(op: &str, result: Result<T, AdminError>) -> Result<T, AdminError> {
    if let Err(e) = &result {
        tracing::warn!("{op} failed: {e}");
    }
    result
}

/// The error returned when a model/endpoint method is called without a
/// configured DB (no `DATABASE_URL`). Reads return empty/`NotFound` instead;
/// only mutations surface this.
fn store_not_configured() -> AdminError {
    AdminError::Store("model store not configured (set DATABASE_URL)".to_string())
}

impl AdminHandlers {
    pub fn new(store: StoreBackend) -> Self {
        Self {
            store,
            model_store: None,
            model_endpoint_store: None,
        }
    }

    /// Whether the model/endpoint stores are connected — i.e. whether the
    /// database can actually supply routing.
    ///
    /// False both when no `DATABASE_URL` is configured *and* when connecting
    /// failed: the stores are optional, so a connection failure is otherwise
    /// indistinguishable from "not configured" (reads just return empty).
    pub fn has_model_stores(&self) -> bool {
        self.model_store.is_some() && self.model_endpoint_store.is_some()
    }

    pub fn with_model_stores(
        mut self,
        model_store: ModelStoreBackend,
        model_endpoint_store: ModelEndpointStoreBackend,
    ) -> Self {
        self.model_store = Some(model_store);
        self.model_endpoint_store = Some(model_endpoint_store);
        self
    }

    pub fn model_store(&self) -> Option<&ModelStoreBackend> {
        self.model_store.as_ref()
    }

    pub fn model_endpoint_store(&self) -> Option<&ModelEndpointStoreBackend> {
        self.model_endpoint_store.as_ref()
    }

    // ─── Key Management ───────────────────────────────────────────────

    pub async fn list_keys(&self) -> Result<Vec<ApiKey>, AdminError> {
        logged("list_keys", self.store.list().await)
    }

    pub async fn create_key(&self, request: CreateApiKeyRequest) -> Result<ApiKey, AdminError> {
        logged("create_key", self.store.create(request).await)
    }

    pub async fn get_key(&self, id: &str) -> Result<Option<ApiKey>, AdminError> {
        logged("get_key", self.store.get(id).await)
    }

    pub async fn update_key(
        &self,
        id: &str,
        request: UpdateApiKeyRequest,
    ) -> Result<Option<ApiKey>, AdminError> {
        logged("update_key", self.store.update(id, request).await)
    }

    /// `Ok(false)` means the id didn't match; `Err` is a store failure and
    /// must not be collapsed into "not found" (the HTTP layer maps it to an
    /// error status instead of a misleading 404).
    pub async fn delete_key(&self, id: &str) -> Result<bool, AdminError> {
        logged("delete_key", self.store.delete(id).await)
    }

    pub async fn revoke_key(&self, id: &str) -> Result<bool, AdminError> {
        logged("revoke_key", self.store.revoke(id).await)
    }

    pub async fn rotate_key(&self, id: &str) -> Result<Option<ApiKey>, AdminError> {
        logged("rotate_key", self.store.rotate(id).await)
    }

    // ─── Model Management ─────────────────────────────────────────────

    pub async fn list_models(&self) -> Result<Vec<Model>, AdminError> {
        match &self.model_store {
            Some(s) => logged("list_models", s.list().await),
            None => Ok(vec![]),
        }
    }

    pub async fn list_enabled_models(&self) -> Result<Vec<Model>, AdminError> {
        match &self.model_store {
            Some(s) => logged("list_enabled_models", s.list_enabled().await),
            None => Ok(vec![]),
        }
    }

    pub async fn create_model(&self, request: CreateModelRequest) -> Result<Model, AdminError> {
        match &self.model_store {
            Some(s) => logged("create_model", s.create(request).await),
            None => Err(store_not_configured()),
        }
    }

    pub async fn get_model(&self, id: &str) -> Result<Option<Model>, AdminError> {
        match &self.model_store {
            Some(s) => logged("get_model", s.get(id).await),
            None => Ok(None),
        }
    }

    pub async fn update_model(
        &self,
        id: &str,
        request: UpdateModelRequest,
    ) -> Result<Option<Model>, AdminError> {
        match &self.model_store {
            Some(s) => logged("update_model", s.update(id, request).await),
            None => Err(store_not_configured()),
        }
    }

    /// `Ok(true)` deleted, `Ok(false)` no such id, `Err(Conflict)` blocked by
    /// a validation guard (the model is still enabled), `Err(Store)` DB
    /// failure. Callers must not collapse `Err` into "not found".
    pub async fn delete_model(&self, id: &str) -> Result<bool, AdminError> {
        match &self.model_store {
            Some(s) => logged("delete_model", s.delete(id).await),
            None => Ok(false),
        }
    }

    pub async fn toggle_model(&self, id: &str, enabled: bool) -> Result<Option<Model>, AdminError> {
        match &self.model_store {
            Some(s) => logged("toggle_model", s.toggle(id, enabled).await),
            None => Err(store_not_configured()),
        }
    }

    /// Models exposed on `GET /v1/models`: only those that are enabled **and**
    /// active (have at least one enabled *routable* endpoint — the same rule
    /// the gateway's target rebuild applies, so a model never lists here and
    /// then 404s on completion). `owned_by` is the provider type of the
    /// model's first such endpoint.
    pub async fn list_enabled_models_for_api(
        &self,
    ) -> Result<Vec<himadri_core::ModelObject>, AdminError> {
        let models = self.list_enabled_models().await?;
        let endpoints = self.list_endpoints().await?;

        // model_id -> provider_type of its first enabled routable endpoint.
        let mut owned_by: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for e in &endpoints {
            if e.enabled
                && himadri_core::endpoint_is_routable(&e.provider_type, e.base_url.as_deref())
            {
                owned_by
                    .entry(e.model_id.clone())
                    .or_insert_with(|| e.provider_type.clone());
            }
        }

        Ok(models
            .into_iter()
            .filter_map(|m| {
                let owned_by = owned_by.get(&m.id)?.clone();
                Some(himadri_core::ModelObject {
                    id: m.name.clone(),
                    object: "model".to_string(),
                    created: m.created_at.timestamp() as u64,
                    owned_by,
                })
            })
            .collect())
    }

    // ─── Model Endpoint Management ────────────────────────────────────

    pub async fn list_endpoints(&self) -> Result<Vec<ModelEndpoint>, AdminError> {
        match &self.model_endpoint_store {
            Some(s) => logged("list_endpoints", s.list().await),
            None => Ok(vec![]),
        }
    }

    pub async fn list_endpoints_by_model(
        &self,
        model_id: &str,
    ) -> Result<Vec<ModelEndpoint>, AdminError> {
        match &self.model_endpoint_store {
            Some(s) => logged("list_endpoints_by_model", s.list_by_model(model_id).await),
            None => Ok(vec![]),
        }
    }

    pub async fn create_endpoint(
        &self,
        model_id: &str,
        request: CreateModelEndpointRequest,
    ) -> Result<ModelEndpoint, AdminError> {
        // An endpoint's base_url is fetched from the gateway, so guard it against
        // SSRF (cloud metadata / internal addresses) before persisting.
        if let Some(url) = &request.base_url {
            if let Err(reason) = himadri_core::provider_url_is_allowed(
                url,
                himadri_core::allow_private_provider_urls(),
            ) {
                tracing::warn!("Rejected endpoint base_url on create: {reason}");
                return Err(AdminError::Validation(reason));
            }
        }
        match &self.model_endpoint_store {
            Some(s) => logged("create_endpoint", s.create(model_id, request).await),
            None => Err(store_not_configured()),
        }
    }

    pub async fn get_endpoint(&self, id: &str) -> Result<Option<ModelEndpoint>, AdminError> {
        match &self.model_endpoint_store {
            Some(s) => logged("get_endpoint", s.get(id).await),
            None => Ok(None),
        }
    }

    pub async fn update_endpoint(
        &self,
        id: &str,
        request: UpdateModelEndpointRequest,
    ) -> Result<Option<ModelEndpoint>, AdminError> {
        // Outer Some(Some(url)) means "set base_url to url"; SSRF-check it.
        if let Some(Some(url)) = &request.base_url {
            if let Err(reason) = himadri_core::provider_url_is_allowed(
                url,
                himadri_core::allow_private_provider_urls(),
            ) {
                tracing::warn!("Rejected endpoint base_url on update: {reason}");
                return Err(AdminError::Validation(reason));
            }
        }
        match &self.model_endpoint_store {
            Some(s) => logged("update_endpoint", s.update(id, request).await),
            None => Err(store_not_configured()),
        }
    }

    /// See [`Self::delete_key`] on the `Ok(false)` / `Err` distinction.
    pub async fn delete_endpoint(&self, id: &str) -> Result<bool, AdminError> {
        match &self.model_endpoint_store {
            Some(s) => logged("delete_endpoint", s.delete(id).await),
            None => Ok(false),
        }
    }

    pub async fn toggle_endpoint(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<Option<ModelEndpoint>, AdminError> {
        match &self.model_endpoint_store {
            Some(s) => logged("toggle_endpoint", s.toggle(id, enabled).await),
            None => Err(store_not_configured()),
        }
    }
}
