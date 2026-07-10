# Azure AD / Entra ID OIDC + Role Setup for AI Gateway
#
# Prerequisites:
#   - Azure CLI installed and authenticated (az login)
#   - Microsoft.Graph PowerShell module installed
#   - Appropriate permissions in Entra ID
#
# Usage:
#   .\deploy\entra\setup.ps1 -TenantId <tenant-id> -GatewayDomain "localhost:8080"

param(
    [Parameter(Mandatory=$true)]
    [string]$TenantId,

    [Parameter(Mandatory=$false)]
    [string]$GatewayDomain = "localhost:8080",

    [Parameter(Mandatory=$false)]
    [string]$AppName = "AI Gateway",

    [Parameter(Mandatory=$false)]
    [string]$AppGroupName = "Gateway Users"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Write-Host "Setting up Azure AD / Entra ID OIDC for AI Gateway..." -ForegroundColor Green
Write-Host "  Tenant ID: $TenantId"
Write-Host "  Gateway Domain: $GatewayDomain"
Write-Host ""

# Connect to Microsoft Graph
Write-Host "Step 1: Connecting to Microsoft Graph..." -ForegroundColor Cyan
try {
    Connect-MgGraph -TenantId $TenantId -Scopes "Application.ReadWrite.All,AppRoleAssignment.ReadWrite.All,Group.ReadWrite.All" -ErrorAction Stop | Out-Null
    Write-Host "✓ Connected to Microsoft Graph" -ForegroundColor Green
} catch {
    Write-Error "Failed to connect to Microsoft Graph: $_"
    exit 1
}

# Create Azure AD application
Write-Host ""
Write-Host "Step 2: Creating Azure AD application..." -ForegroundColor Cyan
$appParams = @{
    DisplayName = $AppName
    SignInAudience = "AzureADMyOrg"
    Web = @{
        RedirectUris = @("http://$GatewayDomain/callback")
        ImplicitGrantSettings = @{
            EnableIdTokenIssuance = $true
            EnableAccessTokenIssuance = $false
        }
    }
    RequiredResourceAccess = @()
}

try {
    $app = New-MgApplication @appParams
    $appId = $app.Id
    $clientId = $app.AppId
    Write-Host "✓ Application created: $appId" -ForegroundColor Green
    Write-Host "  Client ID: $clientId" -ForegroundColor Green
} catch {
    Write-Error "Failed to create application: $_"
    exit 1
}

# Create service principal
Write-Host ""
Write-Host "Step 3: Creating service principal..." -ForegroundColor Cyan
try {
    $servicePrincipal = New-MgServicePrincipal -AppId $clientId
    $spId = $servicePrincipal.Id
    Write-Host "✓ Service principal created: $spId" -ForegroundColor Green
} catch {
    Write-Error "Failed to create service principal: $_"
    exit 1
}

# Create client secret
Write-Host ""
Write-Host "Step 4: Creating client secret..." -ForegroundColor Cyan
try {
    $secretParams = @{
        DisplayName = "AI Gateway Secret"
        EndDateTime = (Get-Date).AddYears(1)
    }
    $clientSecret = Add-MgApplicationPassword -ApplicationId $appId @secretParams
    $secret = $clientSecret.SecretText
    Write-Host "✓ Client secret created (valid for 1 year)" -ForegroundColor Green
} catch {
    Write-Error "Failed to create client secret: $_"
    exit 1
}

# Create app roles
Write-Host ""
Write-Host "Step 5: Creating app roles..." -ForegroundColor Cyan
$roles = @("admin", "power-user", "analyst", "user")
$appRoles = @()

foreach ($roleName in $roles) {
    $roleId = [System.Guid]::NewGuid().ToString()
    $appRoles += @{
        AllowedMemberTypes = @("User", "Application")
        Description = "$roleName role for AI Gateway"
        DisplayName = $roleName
        Id = $roleId
        IsEnabled = $true
        Value = $roleName
    }
    Write-Host "✓ Role: $roleName (ID: $roleId)" -ForegroundColor Green
}

try {
    Update-MgApplication -ApplicationId $appId -AppRoles $appRoles
} catch {
    Write-Error "Failed to create app roles: $_"
    exit 1
}

# Create security groups for roles
Write-Host ""
Write-Host "Step 6: Creating security groups for roles..." -ForegroundColor Cyan
$groupIds = @{}

foreach ($roleName in $roles) {
    try {
        $group = New-MgGroup -DisplayName "Gateway-$roleName" `
                           -Description "$roleName group for AI Gateway" `
                           -GroupTypes @() `
                           -MailEnabled $false `
                           -SecurityEnabled $true `
                           -MailNickname "gateway-$roleName"
        $groupIds[$roleName] = $group.Id
        Write-Host "✓ Group: Gateway-$roleName (ID: $($group.Id))" -ForegroundColor Green
    } catch {
        Write-Error "Failed to create group Gateway-$roleName: $_"
    }
}

# Output environment variables
Write-Host ""
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "✓ Azure AD / Entra ID OIDC Setup Complete" -ForegroundColor Green
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "Add these environment variables to your .env file:" -ForegroundColor Cyan
Write-Host ""
Write-Host "# OIDC Configuration"
Write-Host "export JWT_ISSUER=`"https://login.microsoftonline.com/$TenantId/v2.0`""
Write-Host "export JWT_AUDIENCE=`"$clientId`""
Write-Host "export JWT_REQUIRED_ROLES=`"user,analyst,power-user,admin`""
Write-Host ""
Write-Host "Save this for reference:" -ForegroundColor Cyan
Write-Host "  Tenant ID: $TenantId"
Write-Host "  Application ID: $appId"
Write-Host "  Client ID: $clientId"
Write-Host "  Client Secret: $secret"
Write-Host ""
Write-Host "Group IDs:" -ForegroundColor Cyan
foreach ($role in $roles) {
    Write-Host "  $role: $($groupIds[$role])"
}
Write-Host ""
Write-Host "Next steps:" -ForegroundColor Cyan
Write-Host "  1. Create users in Entra ID"
Write-Host "  2. Assign users to groups (Gateway-user, Gateway-analyst, etc.)"
Write-Host "  3. Configure app to emit group names as roles claim"
Write-Host "  4. Test JWT token generation"
Write-Host ""

# Disconnect from Microsoft Graph
Disconnect-MgGraph | Out-Null
