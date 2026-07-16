use async_trait::async_trait;

use crate::error::AdminError;
use crate::models::{
    CreateModelEndpointRequest, CreateModelRequest, Model, ModelEndpoint,
    UpdateModelEndpointRequest, UpdateModelRequest,
};

#[async_trait]
pub trait ModelStore: Send + Sync {
    async fn create(&self, request: CreateModelRequest) -> Result<Model, AdminError>;
    async fn get(&self, id: &str) -> Result<Option<Model>, AdminError>;
    async fn list(&self) -> Result<Vec<Model>, AdminError>;
    async fn list_enabled(&self) -> Result<Vec<Model>, AdminError>;
    async fn update(
        &self,
        id: &str,
        request: UpdateModelRequest,
    ) -> Result<Option<Model>, AdminError>;
    async fn delete(&self, id: &str) -> Result<bool, AdminError>;
    async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<Model>, AdminError>;
}

#[async_trait]
pub trait ModelEndpointStore: Send + Sync {
    async fn create(
        &self,
        model_id: &str,
        request: CreateModelEndpointRequest,
    ) -> Result<ModelEndpoint, AdminError>;
    async fn get(&self, id: &str) -> Result<Option<ModelEndpoint>, AdminError>;
    async fn list(&self) -> Result<Vec<ModelEndpoint>, AdminError>;
    async fn list_by_model(&self, model_id: &str) -> Result<Vec<ModelEndpoint>, AdminError>;
    async fn update(
        &self,
        id: &str,
        request: UpdateModelEndpointRequest,
    ) -> Result<Option<ModelEndpoint>, AdminError>;
    async fn delete(&self, id: &str) -> Result<bool, AdminError>;
    async fn toggle(&self, id: &str, enabled: bool) -> Result<Option<ModelEndpoint>, AdminError>;
}
