#!/bin/bash
# Auth0 OIDC + Role Setup for AI Gateway
#
# Prerequisites:
#   - Auth0 tenant created
#   - auth0 CLI installed: https://github.com/auth0/auth0-cli
#   - AUTH0_DOMAIN, AUTH0_CLIENT_ID, AUTH0_CLIENT_SECRET set
#
# Usage:
#   export AUTH0_DOMAIN="your-tenant.auth0.com"
#   export AUTH0_CLIENT_ID="<management-api-client-id>"
#   export AUTH0_CLIENT_SECRET="<management-api-client-secret>"
#   bash deploy/auth0/setup.sh

set -e

DOMAIN="${AUTH0_DOMAIN:-$(read -p 'Auth0 domain (e.g. your-tenant.auth0.com): ' d; echo $d)}"
CLIENT_ID="${AUTH0_CLIENT_ID:-$(read -p 'Auth0 Management API Client ID: ' id; echo $id)}"
CLIENT_SECRET="${AUTH0_CLIENT_SECRET:-$(read -sp 'Auth0 Management API Client Secret: ' secret; echo $secret)}"
GATEWAY_DOMAIN="${GATEWAY_DOMAIN:-localhost:8080}"

if [[ -z "$DOMAIN" ]] || [[ -z "$CLIENT_ID" ]] || [[ -z "$CLIENT_SECRET" ]]; then
  echo "Error: AUTH0_DOMAIN, AUTH0_CLIENT_ID, and AUTH0_CLIENT_SECRET are required"
  exit 1
fi

echo "Setting up Auth0 OIDC for AI Gateway..."
echo "  Auth0 domain: $DOMAIN"
echo "  Gateway domain: $GATEWAY_DOMAIN"

# Get access token for Management API
echo ""
echo "Step 1: Authenticating with Auth0 Management API..."
ACCESS_TOKEN=$(curl -s -X POST "https://$DOMAIN/oauth/token" \
  -H "Content-Type: application/json" \
  -d "{
    \"client_id\": \"$CLIENT_ID\",
    \"client_secret\": \"$CLIENT_SECRET\",
    \"audience\": \"https://$DOMAIN/api/v2/\",
    \"grant_type\": \"client_credentials\"
  }" | jq -r '.access_token // empty')

if [[ -z "$ACCESS_TOKEN" ]]; then
  echo "  ✗ Failed to authenticate"
  exit 1
fi

echo "  ✓ Authentication successful"

# Create API audience (for access tokens)
echo ""
echo "Step 2: Creating API audience..."
API_IDENTIFIER="https://api.ai-gateway.local"
curl -s -X POST "https://$DOMAIN/api/v2/resource-servers" \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"identifier\": \"$API_IDENTIFIER\",
    \"name\": \"AI Gateway API\",
    \"scopes\": [
      {\"value\": \"read\", \"description\": \"Read access\"},
      {\"value\": \"write\", \"description\": \"Write access\"},
      {\"value\": \"admin\", \"description\": \"Admin access\"}
    ]
  }" > /dev/null 2>&1 || true

echo "  ✓ API audience created"

# Create OIDC application
APP_NAME="AI Gateway"
echo ""
echo "Step 3: Creating OIDC application '$APP_NAME'..."
APP_RESPONSE=$(curl -s -X POST "https://$DOMAIN/api/v2/clients" \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"name\": \"$APP_NAME\",
    \"description\": \"OAuth 2.0 / OIDC server for AI Gateway\",
    \"client_type\": \"regular_web\",
    \"app_type\": \"regular_web\",
    \"callbacks\": [\"http://$GATEWAY_DOMAIN/callback\"],
    \"allowed_logout_urls\": [\"http://$GATEWAY_DOMAIN/logout\"],
    \"token_endpoint_auth_method\": \"client_secret_basic\",
    \"oidc_conformant\": true,
    \"jwt_configuration\": {
      \"lifetime_in_seconds\": 36000,
      \"secret_encoded\": false
    }
  }")

NEW_CLIENT_ID=$(echo "$APP_RESPONSE" | jq -r '.client_id // empty')
NEW_CLIENT_SECRET=$(echo "$APP_RESPONSE" | jq -r '.client_secret // empty')

if [[ -z "$NEW_CLIENT_ID" ]]; then
  echo "  ✗ Failed to create application"
  exit 1
fi

echo "  ✓ Application ID: $NEW_CLIENT_ID"

# Create standard roles
echo ""
echo "Step 4: Creating standard roles..."
ROLES=("admin" "power-user" "analyst" "user")

for role_name in "${ROLES[@]}"; do
  ROLE_RESPONSE=$(curl -s -X POST "https://$DOMAIN/api/v2/roles" \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"name\": \"$role_name\",
      \"description\": \"$role_name role for AI Gateway\"
    }")
  echo "  ✓ Role: $role_name"
done

# Create a rule to add roles as custom claim
echo ""
echo "Step 5: Creating rule to emit roles in ID token..."
RULE_SCRIPT='
function (user, context, callback) {
  var roles = user.roles || [];
  context.idToken = context.idToken || {};
  context.idToken.roles = roles;
  context.idToken["custom:billing_tier"] = "pro";
  callback(null, user, context);
}
'

curl -s -X POST "https://$DOMAIN/api/v2/rules" \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"name\": \"add_roles_to_jwt\",
    \"script\": $RULE_SCRIPT,
    \"order\": 1,
    \"enabled\": true
  }" > /dev/null 2>&1 || true

echo "  ✓ Rule created"

# Output environment variables
echo ""
echo "=========================================="
echo "✓ Auth0 OIDC Setup Complete"
echo "=========================================="
echo ""
echo "Add these environment variables to your .env file:"
echo ""
echo "# OIDC Configuration"
echo "export JWT_ISSUER=\"https://$DOMAIN/\""
echo "export JWT_AUDIENCE=\"$NEW_CLIENT_ID\""
echo "export JWT_REQUIRED_ROLES=\"user,analyst,power-user,admin\""
echo ""
echo "Save this for reference:"
echo "  Auth0 Domain: $DOMAIN"
echo "  Application ID (Client ID): $NEW_CLIENT_ID"
echo "  Application Secret: $NEW_CLIENT_SECRET"
echo "  API Identifier: $API_IDENTIFIER"
echo ""
echo "Next steps:"
echo "  1. Create users in Auth0"
echo "  2. Assign roles to users (user, analyst, power-user, admin)"
echo "  3. Verify JWT tokens include roles claim"
echo "  4. Test with gateway"
echo ""
