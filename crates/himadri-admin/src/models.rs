use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn default_true() -> bool {
    true
}

fn default_weight() -> f64 {
    1.0
}

/// A model is a first-party entity: the thing a client requests by name. It
/// owns one or more [`ModelEndpoint`]s (provider routes). A model with no
/// enabled endpoint is inactive — nothing can serve it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to create a model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateModelRequest {
    pub name: String,
    pub display_name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Request to update a model
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateModelRequest {
    pub name: Option<String>,
    pub display_name: Option<Option<String>>,
    pub enabled: Option<bool>,
}

/// One provider route for a model. Credentials/base URL are denormalized onto
/// the endpoint so the same model can be served by several providers, each with
/// its own key and weight. `provider_type` selects the client adapter
/// (openai / anthropic / gemini / openrouter / …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEndpoint {
    pub id: String,
    pub model_id: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    /// Routing weight among the model's endpoints. Higher wins weighted
    /// load-balancing; lower wins cost-optimized ordering. Defaults to 1.0.
    pub weight: f64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to create a model endpoint. `model_id` is taken from the route path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateModelEndpointRequest {
    pub provider_type: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Request to update a model endpoint. `api_key: Some(None)` clears the key;
/// `None` leaves it unchanged (mirroring the provider store's key semantics).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateModelEndpointRequest {
    pub provider_type: Option<String>,
    pub base_url: Option<Option<String>>,
    pub api_key: Option<Option<String>>,
    pub weight: Option<f64>,
    pub enabled: Option<bool>,
}

impl Model {
    pub fn new(request: CreateModelRequest) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: request.name,
            display_name: request.display_name,
            enabled: request.enabled,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

impl ModelEndpoint {
    pub fn new(model_id: String, request: CreateModelEndpointRequest) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            model_id,
            provider_type: request.provider_type,
            base_url: request.base_url,
            api_key: request.api_key,
            weight: request.weight,
            enabled: request.enabled,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}
