// Azure AD / Entra ID OIDC + Role Setup for AI Gateway
// Prerequisites:
//   - Azure CLI installed and authenticated (az login)
//   - Appropriate permissions in Entra ID
//
// Usage:
//   az deployment tenant create \
//     --template-file deploy/entra/bicep/main.bicep \
//     --location eastus \
//     --parameters \
//       displayName='AI Gateway' \
//       tenantId=$(az account show --query tenantId -o tsv) \
//       gatewayDomain='localhost:8080'

param displayName string = 'AI Gateway'
param tenantId string
param gatewayDomain string = 'localhost:8080'
param location string = 'eastus'

var appRoles = [
  {
    allowedMemberTypes: [
      'User'
      'Application'
    ]
    description: 'Admin role for AI Gateway'
    displayName: 'admin'
    id: 'a64b0b7c-47d7-4ea5-9e0f-e9e6b8c9d8a1'
    isEnabled: true
    value: 'admin'
  }
  {
    allowedMemberTypes: [
      'User'
      'Application'
    ]
    description: 'Power User role for AI Gateway'
    displayName: 'power-user'
    id: 'b64b0b7c-47d7-4ea5-9e0f-e9e6b8c9d8a2'
    isEnabled: true
    value: 'power-user'
  }
  {
    allowedMemberTypes: [
      'User'
      'Application'
    ]
    description: 'Analyst role for AI Gateway'
    displayName: 'analyst'
    id: 'c64b0b7c-47d7-4ea5-9e0f-e9e6b8c9d8a3'
    isEnabled: true
    value: 'analyst'
  }
  {
    allowedMemberTypes: [
      'User'
      'Application'
    ]
    description: 'User role for AI Gateway'
    displayName: 'user'
    id: 'd64b0b7c-47d7-4ea5-9e0f-e9e6b8c9d8a4'
    isEnabled: true
    value: 'user'
  }
]

resource app 'Microsoft.Graph/applications@v1.0' = {
  displayName: displayName
  description: 'OAuth 2.0 / OIDC server for AI Gateway'
  signInAudience: 'AzureADMyOrg'
  appRoles: appRoles
  web: {
    redirectUris: [
      'http://${gatewayDomain}/callback'
    ]
    implicitGrantSettings: {
      enableAccessTokenIssuance: false
      enableIdTokenIssuance: true
    }
  }
  requiredResourceAccess: [
    {
      resourceAppId: '00000003-0000-0000-c000-000000000000' // Microsoft Graph
      resourceAccess: [
        {
          id: 'e1fe6dd8-ba31-4d61-89e7-88639da4683d' // User.Read
          type: 'Scope'
        }
      ]
    }
  ]
}

output displayName string = app.displayName
output appId string = app.appId
output objectId string = app.id
output issuer string = 'https://login.microsoftonline.com/${tenantId}/v2.0'
output requiredEnvVars object = {
  JWT_ISSUER: 'https://login.microsoftonline.com/${tenantId}/v2.0'
  JWT_AUDIENCE: app.appId
  JWT_REQUIRED_ROLES: 'user,analyst,power-user,admin'
}
