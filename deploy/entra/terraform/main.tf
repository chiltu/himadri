# Azure AD / Entra ID OIDC + Role Setup for AI Gateway
# Prerequisites:
#   - terraform >= 1.0
#   - azuread provider configured
#   - AZURE_TENANT_ID set
#
# Usage:
#   export AZURE_TENANT_ID="<tenant-id>"
#   terraform apply

terraform {
  required_version = ">= 1.0"
  required_providers {
    azuread = {
      source  = "hashicorp/azuread"
      version = "~> 2.0"
    }
  }
}

provider "azuread" {
  tenant_id = var.azure_tenant_id
}

variable "azure_tenant_id" {
  type        = string
  description = "Azure tenant ID"
}

variable "gateway_domain" {
  type        = string
  default     = "localhost:8080"
  description = "AI Gateway domain for redirect URI"
}

variable "app_name" {
  type        = string
  default     = "AI Gateway"
  description = "Azure AD application name"
}

# Get current context (for display)
data "azuread_client_config" "current" {}

# Create Azure AD application
resource "azuread_application" "gateway" {
  display_name = var.app_name

  single_page_application {
    redirect_uris = ["http://${var.gateway_domain}/callback"]
  }

  web {
    redirect_uris = ["http://${var.gateway_domain}/callback"]

    implicit_grant {
      access_token_issuance_enabled = true
      id_token_issuance_enabled      = true
    }
  }

  required_resource_access {
    resource_app_id = "00000003-0000-0000-c000-000000000000" # Microsoft Graph

    resource_access {
      id   = "e1fe6dd8-ba31-4d61-89e7-88639da4683d"  # User.Read
      type = "Scope"
    }
  }
}

# Create service principal
resource "azuread_service_principal" "gateway" {
  object_id = azuread_application.gateway.object_id

  app_role_assignment_required = false
}

# Create client secret
resource "azuread_application_password" "gateway" {
  application_id    = azuread_application.gateway.id
  display_name      = "AI Gateway Secret"
  end_date_relative = "8760h" # 1 year
}

# Create app roles
resource "azuread_application_app_role" "admin" {
  application_id = azuread_application.gateway.id
  role_id        = "a64b0b7c-47d7-4ea5-9e0f-e9e6b8c9d8a1"
  display_name   = "admin"
  description    = "Admin role for AI Gateway"
  value          = "admin"
  allowed_member_types = [
    "User",
    "Application"
  ]
}

resource "azuread_application_app_role" "power_user" {
  application_id = azuread_application.gateway.id
  role_id        = "b64b0b7c-47d7-4ea5-9e0f-e9e6b8c9d8a2"
  display_name   = "power-user"
  description    = "Power User role for AI Gateway"
  value          = "power-user"
  allowed_member_types = [
    "User",
    "Application"
  ]
}

resource "azuread_application_app_role" "analyst" {
  application_id = azuread_application.gateway.id
  role_id        = "c64b0b7c-47d7-4ea5-9e0f-e9e6b8c9d8a3"
  display_name   = "analyst"
  description    = "Analyst role for AI Gateway"
  value          = "analyst"
  allowed_member_types = [
    "User",
    "Application"
  ]
}

resource "azuread_application_app_role" "user" {
  application_id = azuread_application.gateway.id
  role_id        = "d64b0b7c-47d7-4ea5-9e0f-e9e6b8c9d8a4"
  display_name   = "user"
  description    = "User role for AI Gateway"
  value          = "user"
  allowed_member_types = [
    "User",
    "Application"
  ]
}

# Create security groups for roles
resource "azuread_group" "admin_group" {
  display_name     = "Gateway-admin"
  description      = "Admin group for AI Gateway"
  owners           = [data.azuread_client_config.current.object_id]
  security_enabled = true
}

resource "azuread_group" "power_user_group" {
  display_name     = "Gateway-power-user"
  description      = "Power User group for AI Gateway"
  owners           = [data.azuread_client_config.current.object_id]
  security_enabled = true
}

resource "azuread_group" "analyst_group" {
  display_name     = "Gateway-analyst"
  description      = "Analyst group for AI Gateway"
  owners           = [data.azuread_client_config.current.object_id]
  security_enabled = true
}

resource "azuread_group" "user_group" {
  display_name     = "Gateway-user"
  description      = "User group for AI Gateway"
  owners           = [data.azuread_client_config.current.object_id]
  security_enabled = true
}

# Outputs
output "tenant_id" {
  value       = var.azure_tenant_id
  description = "Azure tenant ID"
}

output "issuer" {
  value       = "https://login.microsoftonline.com/${var.azure_tenant_id}/v2.0"
  description = "OAuth 2.0 token issuer (use as JWT_ISSUER)"
}

output "client_id" {
  value       = azuread_application.gateway.client_id
  description = "Application Client ID (use as JWT_AUDIENCE)"
}

output "client_secret" {
  value       = azuread_application_password.gateway.value
  sensitive   = true
  description = "Application Secret (store securely)"
}

output "application_id" {
  value       = azuread_application.gateway.id
  description = "Azure AD Application ID (Object ID)"
}

output "group_ids" {
  value = {
    admin      = azuread_group.admin_group.object_id
    power_user = azuread_group.power_user_group.object_id
    analyst    = azuread_group.analyst_group.object_id
    user       = azuread_group.user_group.object_id
  }
  description = "Group IDs for role-based assignment"
}

output "env_vars" {
  value = {
    JWT_ISSUER         = "https://login.microsoftonline.com/${var.azure_tenant_id}/v2.0"
    JWT_AUDIENCE       = azuread_application.gateway.client_id
    JWT_REQUIRED_ROLES = "user,analyst,power-user,admin"
  }
  description = "Environment variables for gateway configuration"
  sensitive   = false
}
