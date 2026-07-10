# Zitadel OIDC + Role Setup for AI Gateway
# Prerequisites:
#   - terraform >= 1.0
#   - zitadel provider configured
#   - ZITADEL_TOKEN, ZITADEL_DOMAIN env vars set
#
# Usage:
#   export ZITADEL_DOMAIN="your-domain.zitadel.cloud"
#   export ZITADEL_TOKEN="<pat>"
#   terraform apply

terraform {
  required_version = ">= 1.0"
  required_providers {
    zitadel = {
      source  = "zitadel/zitadel"
      version = "~> 1.0"
    }
  }
}

provider "zitadel" {
  domain = var.zitadel_domain
  token  = var.zitadel_token
}

variable "zitadel_domain" {
  type        = string
  description = "Zitadel instance domain (e.g., your-domain.zitadel.cloud)"
}

variable "zitadel_token" {
  type        = string
  sensitive   = true
  description = "Zitadel personal access token (PAT)"
}

variable "gateway_domain" {
  type        = string
  default     = "localhost:8080"
  description = "AI Gateway domain for redirect URI"
}

variable "org_name" {
  type        = string
  default     = "ai-gateway-orgs"
  description = "Zitadel organization name"
}

variable "project_name" {
  type        = string
  default     = "AI Gateway"
  description = "Zitadel project name"
}

variable "app_name" {
  type        = string
  default     = "API Gateway"
  description = "OIDC application name"
}

# Create organization
resource "zitadel_org" "gateway" {
  name = var.org_name
}

# Create project
resource "zitadel_project" "gateway" {
  org_id = zitadel_org.gateway.id
  name   = var.project_name
}

# Create OIDC application
resource "zitadel_application_oidc" "gateway" {
  org_id              = zitadel_org.gateway.id
  project_id          = zitadel_project.gateway.id
  name                = var.app_name
  redirect_uris       = ["http://${var.gateway_domain}/callback"]
  response_type       = ["CODE", "ID_TOKEN"]
  auth_method         = "BASIC"
  grant_types         = ["AUTHORIZATION_CODE"]
  post_logout_uri     = "http://${var.gateway_domain}/logout"
}

# Create standard roles
resource "zitadel_project_role" "user" {
  org_id       = zitadel_org.gateway.id
  project_id   = zitadel_project.gateway.id
  role_key     = "user"
  display_name = "User"
  group        = "Gateway Roles"
}

resource "zitadel_project_role" "power_user" {
  org_id       = zitadel_org.gateway.id
  project_id   = zitadel_project.gateway.id
  role_key     = "power-user"
  display_name = "Power User"
  group        = "Gateway Roles"
}

resource "zitadel_project_role" "analyst" {
  org_id       = zitadel_org.gateway.id
  project_id   = zitadel_project.gateway.id
  role_key     = "analyst"
  display_name = "Analyst"
  group        = "Gateway Roles"
}

resource "zitadel_project_role" "admin" {
  org_id       = zitadel_org.gateway.id
  project_id   = zitadel_project.gateway.id
  role_key     = "admin"
  display_name = "Admin"
  group        = "Gateway Roles"
}

# Outputs
output "zitadel_issuer" {
  value       = "https://${var.zitadel_domain}"
  description = "Zitadel issuer URL (use as JWT_ISSUER)"
}

output "client_id" {
  value       = zitadel_application_oidc.gateway.client_id
  description = "OIDC client ID (use as JWT_AUDIENCE)"
}

output "jwks_uri" {
  value       = "https://${var.zitadel_domain}/oauth/v2/keys"
  description = "JWKS endpoint (use as JWT_JWKS_URI)"
}

output "organization_id" {
  value       = zitadel_org.gateway.id
  description = "Zitadel organization ID"
}

output "project_id" {
  value       = zitadel_project.gateway.id
  description = "Zitadel project ID"
}

output "application_id" {
  value       = zitadel_application_oidc.gateway.id
  description = "OIDC application ID"
}

output "env_vars" {
  value = {
    JWT_ISSUER         = "https://${var.zitadel_domain}"
    JWT_AUDIENCE       = zitadel_application_oidc.gateway.client_id
    JWT_JWKS_URI       = "https://${var.zitadel_domain}/oauth/v2/keys"
    JWT_REQUIRED_ROLES = "user,analyst,power-user,admin"
  }
  description = "Environment variables for gateway configuration"
  sensitive   = false
}
