//! Wire-up and initialization logic for the gateway.
//!
//! This module contains high-level setup functions for complex subsystems
//! like the provider registry and plugin pipeline.

pub mod mode;
pub mod plugins;
pub mod providers;

pub use providers::build_provider_registry;
