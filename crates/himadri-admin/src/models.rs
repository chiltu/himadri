use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Provider configuration stored in database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub weight: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to create a provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProviderRequest {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_true() -> bool {
    true
}

fn default_weight() -> f64 {
    1.0
}

/// Request to update a provider
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub api_key: Option<Option<String>>,
    pub base_url: Option<Option<String>>,
    pub weight: Option<f64>,
}

/// Model configuration stored in database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider_id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to create a model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateModelRequest {
    pub name: String,
    pub provider_id: String,
    pub display_name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Request to update a model
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateModelRequest {
    pub name: Option<String>,
    pub provider_id: Option<String>,
    pub display_name: Option<Option<String>>,
    pub enabled: Option<bool>,
}

impl Provider {
    pub fn new(request: CreateProviderRequest) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: request.name,
            enabled: request.enabled,
            api_key: request.api_key,
            base_url: request.base_url,
            weight: request.weight,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

impl Model {
    pub fn new(request: CreateModelRequest) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: request.name,
            provider_id: request.provider_id,
            display_name: request.display_name,
            enabled: request.enabled,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}
