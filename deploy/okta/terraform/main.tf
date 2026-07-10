# Okta OIDC + Role Setup for AI Gateway
# Prerequisites:
#   - terraform >= 1.0
#   - okta provider configured
#   - OKTA_ORG_NAME, OKTA_API_TOKEN env vars set
#
# Usage:
#   export OKTA_ORG_NAME="your-domain"
#   export OKTA_API_TOKEN="<token>"
#   terraform apply

terraform {
  required_version = ">= 1.0"
  required_providers {
    okta = {
      source  = "okta/okta"
      version = "~> 4.0"
    }
  }
}

variable "okta_org_name" {
  type        = string
  description = "Okta organization name (subdomain)"
}

variable "okta_api_token" {
  type        = string
  sensitive   = true
  description = "Okta API token"
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

provider "okta" {
  org_name   = var.okta_org_name
  api_token  = var.okta_api_token
  base_url   = "https://${var.okta_org_name}.okta.com"
}

# Create OAuth 2.0 OIDC application
resource "okta_app_oauth" "gateway" {
  label              = var.app_name
  type               = "web"
  grant_types        = ["authorization_code"]
  response_types     = ["code", "id_token"]
  redirect_uris      = ["http://${var.gateway_domain}/callback"]
  post_logout_redirect_uris = ["http://${var.gateway_domain}/logout"]
  client_uri         = "http://${var.gateway_domain}"
  login_uri          = "http://${var.gateway_domain}"
  token_endpoint_auth_method = "client_secret_basic"
}

# Create role-based groups
resource "okta_group" "admin_group" {
  name        = "Gateway-admin"
  description = "Users with admin role for AI Gateway"
}

resource "okta_group" "power_user_group" {
  name        = "Gateway-power-user"
  description = "Users with power-user role for AI Gateway"
}

resource "okta_group" "analyst_group" {
  name        = "Gateway-analyst"
  description = "Users with analyst role for AI Gateway"
}

resource "okta_group" "user_group" {
  name        = "Gateway-user"
  description = "Users with user role for AI Gateway"
}

# Assign application to groups
resource "okta_app_group_assignment" "gateway_admin" {
  app_id   = okta_app_oauth.gateway.id
  group_id = okta_group.admin_group.id
}

resource "okta_app_group_assignment" "gateway_power_user" {
  app_id   = okta_app_oauth.gateway.id
  group_id = okta_group.power_user_group.id
}

resource "okta_app_group_assignment" "gateway_analyst" {
  app_id   = okta_app_oauth.gateway.id
  group_id = okta_group.analyst_group.id
}

resource "okta_app_group_assignment" "gateway_user" {
  app_id   = okta_app_oauth.gateway.id
  group_id = okta_group.user_group.id
}

# Outputs
output "okta_domain" {
  value       = "https://${var.okta_org_name}.okta.com"
  description = "Okta domain"
}

output "issuer" {
  value       = "https://${var.okta_org_name}.okta.com/oauth2/default"
  description = "OAuth 2.0 authorization server issuer (use as JWT_ISSUER)"
}

output "client_id" {
  value       = okta_app_oauth.gateway.client_id
  description = "OIDC Client ID (use as JWT_AUDIENCE)"
}

output "client_secret" {
  value       = okta_app_oauth.gateway.client_secret
  sensitive   = true
  description = "OIDC Client Secret (store securely)"
}

output "env_vars" {
  value = {
    JWT_ISSUER         = "https://${var.okta_org_name}.okta.com/oauth2/default"
    JWT_AUDIENCE       = okta_app_oauth.gateway.client_id
    JWT_REQUIRED_ROLES = "user,analyst,power-user,admin"
  }
  description = "Environment variables for gateway configuration"
  sensitive   = false
}
