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

impl UpdateModelEndpointRequest {
    /// The `(provider_type, base_url)` pair this update produces against
    /// `current` — the one definition of the merge.
    ///
    /// Both store backends persist this pair and the admin API validates it
    /// before the write, so all three must agree on the `Option<Option<_>>`
    /// semantics (`None` leaves the field alone, `Some(None)` clears it). When
    /// each derived the merge itself, a change to one silently validated a
    /// different pair than the one written.
    pub fn effective_routing_pair(&self, current: &ModelEndpoint) -> (String, Option<String>) {
        (
            self.provider_type
                .clone()
                .unwrap_or_else(|| current.provider_type.clone()),
            self.base_url
                .clone()
                .unwrap_or_else(|| current.base_url.clone()),
        )
    }
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

#[cfg(test)]
mod merge_tests {
    use super::*;
    use chrono::Utc;

    fn endpoint(provider_type: &str, base_url: Option<&str>) -> ModelEndpoint {
        ModelEndpoint {
            id: "ep".to_string(),
            model_id: "m".to_string(),
            provider_type: provider_type.to_string(),
            base_url: base_url.map(str::to_string),
            api_key: None,
            weight: 1.0,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// The `Option<Option<_>>` semantics both backends persist and the admin
    /// API validates against: `None` leaves a field alone, `Some(None)` clears
    /// it, `Some(Some(v))` sets it.
    #[test]
    fn merge_distinguishes_unchanged_from_cleared() {
        let current = endpoint("openai", Some("https://stored/v1"));

        let untouched = UpdateModelEndpointRequest::default();
        assert_eq!(
            untouched.effective_routing_pair(&current),
            ("openai".to_string(), Some("https://stored/v1".to_string())),
            "an empty update must reproduce the stored pair"
        );

        let cleared = UpdateModelEndpointRequest {
            base_url: Some(None),
            ..Default::default()
        };
        assert_eq!(
            cleared.effective_routing_pair(&current),
            ("openai".to_string(), None),
            "Some(None) must clear the base_url"
        );

        let switched = UpdateModelEndpointRequest {
            provider_type: Some("my-vllm".to_string()),
            ..Default::default()
        };
        assert_eq!(
            switched.effective_routing_pair(&current),
            ("my-vllm".to_string(), Some("https://stored/v1".to_string())),
            "changing the type must keep the stored base_url"
        );

        let both = UpdateModelEndpointRequest {
            provider_type: Some("groq".to_string()),
            base_url: Some(Some("https://new/v1".to_string())),
            ..Default::default()
        };
        assert_eq!(
            both.effective_routing_pair(&current),
            ("groq".to_string(), Some("https://new/v1".to_string()))
        );
    }
}
