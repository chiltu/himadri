use crate::store::StoreBackend;
use crate::{ApiKey, CreateApiKeyRequest, UpdateApiKeyRequest};

pub struct AdminHandlers {
    store: StoreBackend,
    _master_key: Option<String>,
}

impl AdminHandlers {
    pub fn new(store: StoreBackend, master_key: Option<String>) -> Self {
        Self {
            store,
            _master_key: master_key,
        }
    }

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
}
