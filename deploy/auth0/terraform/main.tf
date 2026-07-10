# Auth0 OIDC + Role Setup for AI Gateway
# Prerequisites:
#   - terraform >= 1.0
#   - auth0 provider configured
#   - AUTH0_DOMAIN, AUTH0_CLIENT_ID, AUTH0_CLIENT_SECRET env vars set
#
# Usage:
#   export AUTH0_DOMAIN="your-tenant.auth0.com"
#   export AUTH0_CLIENT_ID="<management-api-client-id>"
#   export AUTH0_CLIENT_SECRET="<management-api-client-secret>"
#   terraform apply

terraform {
  required_version = ">= 1.0"
  required_providers {
    auth0 = {
      source  = "auth0/auth0"
      version = "~> 1.0"
    }
  }
}

provider "auth0" {
  domain        = var.auth0_domain
  client_id     = var.auth0_client_id
  client_secret = var.auth0_client_secret
}

variable "auth0_domain" {
  type        = string
  description = "Auth0 tenant domain (e.g., your-tenant.auth0.com)"
}

variable "auth0_client_id" {
  type        = string
  sensitive   = true
  description = "Auth0 Management API Client ID"
}

variable "auth0_client_secret" {
  type        = string
  sensitive   = true
  description = "Auth0 Management API Client Secret"
}

variable "gateway_domain" {
  type        = string
  default     = "localhost:8080"
  description = "AI Gateway domain for redirect URI"
}

variable "app_name" {
  type        = string
  default     = "AI Gateway"
  description = "OIDC application name"
}

# Create API audience
resource "auth0_resource_server" "gateway_api" {
  identifier = "https://api.ai-gateway.local"
  name       = "AI Gateway API"
  token_lifetime = 36000

  scopes {
    name        = "read"
    description = "Read access"
  }

  scopes {
    name        = "write"
    description = "Write access"
  }

  scopes {
    name        = "admin"
    description = "Admin access"
  }
}

# Create OIDC application
resource "auth0_client" "gateway" {
  name              = var.app_name
  description       = "OAuth 2.0 / OIDC server for AI Gateway"
  app_type          = "regular_web"
  callbacks         = ["http://${var.gateway_domain}/callback"]
  allowed_logout_urls = ["http://${var.gateway_domain}/logout"]

  oidc_conformant = true

  token_endpoint_auth_method = "client_secret_basic"

  jwt_configuration {
    lifetime_in_seconds = 36000
    secret_encoded      = false
  }
}

# Create standard roles
resource "auth0_role" "user" {
  name        = "user"
  description = "User role for AI Gateway"
}

resource "auth0_role" "power_user" {
  name        = "power-user"
  description = "Power User role for AI Gateway"
}

resource "auth0_role" "analyst" {
  name        = "analyst"
  description = "Analyst role for AI Gateway"
}

resource "auth0_role" "admin" {
  name        = "admin"
  description = "Admin role for AI Gateway"
}

# Create rule to add roles to JWT
resource "auth0_rule" "add_roles_to_jwt" {
  name    = "add_roles_to_jwt"
  script  = <<-EOT
    function (user, context, callback) {
      var roles = user.roles || [];
      context.idToken = context.idToken || {};
      context.idToken.roles = roles;
      context.idToken["custom:billing_tier"] = "pro";
      callback(null, user, context);
    }
  EOT
  enabled = true
  order   = 1
}

# Outputs
output "auth0_domain" {
  value       = var.auth0_domain
  description = "Auth0 domain (use as JWT_ISSUER with trailing /)"
}

output "client_id" {
  value       = auth0_client.gateway.client_id
  description = "OIDC Client ID (use as JWT_AUDIENCE)"
}

output "client_secret" {
  value       = auth0_client.gateway.client_secret
  sensitive   = true
  description = "OIDC Client Secret (store securely)"
}

output "api_identifier" {
  value       = auth0_resource_server.gateway_api.identifier
  description = "API Identifier (audience for access tokens)"
}

output "env_vars" {
  value = {
    JWT_ISSUER         = "https://${var.auth0_domain}/"
    JWT_AUDIENCE       = auth0_client.gateway.client_id
    JWT_REQUIRED_ROLES = "user,analyst,power-user,admin"
  }
  description = "Environment variables for gateway configuration"
  sensitive   = false
}
