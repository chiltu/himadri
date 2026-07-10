#!/bin/bash
# Okta OIDC + Role Setup for AI Gateway
#
# Prerequisites:
#   - Okta tenant created
#   - okta CLI installed: https://github.com/okta/okta-cli
#   - OKTA_DOMAIN, OKTA_API_TOKEN set
#
# Usage:
#   export OKTA_DOMAIN="https://dev-xxxxx.okta.com"
#   export OKTA_API_TOKEN="<token>"
#   bash deploy/okta/setup.sh

set -e

OKTA_DOMAIN="${OKTA_DOMAIN:-$(read -p 'Okta domain (e.g. https://dev-xxxxx.okta.com): ' d; echo $d)}"
OKTA_API_TOKEN="${OKTA_API_TOKEN:-$(read -sp 'Okta API token: ' t; echo $t)}"
GATEWAY_DOMAIN="${GATEWAY_DOMAIN:-localhost:8080}"

if [[ -z "$OKTA_DOMAIN" ]] || [[ -z "$OKTA_API_TOKEN" ]]; then
  echo "Error: OKTA_DOMAIN and OKTA_API_TOKEN are required"
  exit 1
fi

# Normalize domain
OKTA_DOMAIN="${OKTA_DOMAIN%/}"

echo "Setting up Okta OIDC for AI Gateway..."
echo "  Okta domain: $OKTA_DOMAIN"
echo "  Gateway domain: $GATEWAY_DOMAIN"

# Create OAuth application
echo ""
echo "Step 1: Creating OAuth 2.0 OIDC application..."
APP_RESPONSE=$(curl -s -X POST "$OKTA_DOMAIN/api/v1/apps" \
  -H "Authorization: Bearer $OKTA_API_TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d "{
    \"name\": \"oidc_client\",
    \"label\": \"AI Gateway\",
    \"signOnMode\": \"OPENID_CONNECT\",
    \"settings\": {
      \"oauthClient\": {
        \"client_uri\": \"http://$GATEWAY_DOMAIN\",
        \"redirect_uris\": [\"http://$GATEWAY_DOMAIN/callback\"],
        \"post_logout_redirect_uris\": [\"http://$GATEWAY_DOMAIN/logout\"],
        \"response_types\": [\"code\", \"id_token\"],
        \"grant_types\": [\"authorization_code\"],
        \"application_type\": \"web\",
        \"token_endpoint_auth_method\": \"client_secret_basic\"
      }
    }
  }")

APP_ID=$(echo "$APP_RESPONSE" | jq -r '.id // empty')
CLIENT_ID=$(echo "$APP_RESPONSE" | jq -r '.credentials.oauthClient.client_id // empty')
CLIENT_SECRET=$(echo "$APP_RESPONSE" | jq -r '.credentials.oauthClient.client_secret // empty')

if [[ -z "$APP_ID" ]] || [[ -z "$CLIENT_ID" ]]; then
  echo "  ✗ Failed to create application"
  echo "$APP_RESPONSE" | jq '.' 2>/dev/null || echo "$APP_RESPONSE"
  exit 1
fi

echo "  ✓ Application created: $APP_ID"
echo "  ✓ Client ID: $CLIENT_ID"

# Create standard roles
echo ""
echo "Step 2: Creating standard roles..."
ROLES=("admin" "power-user" "analyst" "user")

for role_name in "${ROLES[@]}"; do
  curl -s -X POST "$OKTA_DOMAIN/api/v1/iam/roles" \
    -H "Authorization: Bearer $OKTA_API_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"type\": \"CUSTOM\",
      \"label\": \"$role_name\",
      \"description\": \"$role_name role for AI Gateway\"
    }" > /dev/null 2>&1 || true
  echo "  ✓ Role: $role_name"
done

# Create a group for each role
echo ""
echo "Step 3: Creating role-based groups..."
for role_name in "${ROLES[@]}"; do
  curl -s -X POST "$OKTA_DOMAIN/api/v1/groups" \
    -H "Authorization: Bearer $OKTA_API_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"profile\": {
        \"name\": \"Gateway-$role_name\",
        \"description\": \"Users with $role_name role for AI Gateway\"
      }
    }" > /dev/null 2>&1 || true
  echo "  ✓ Group: Gateway-$role_name"
done

# Output environment variables
echo ""
echo "=========================================="
echo "✓ Okta OIDC Setup Complete"
echo "=========================================="
echo ""
echo "Add these environment variables to your .env file:"
echo ""
echo "# OIDC Configuration"
echo "export JWT_ISSUER=\"$OKTA_DOMAIN/oauth2/default\""
echo "export JWT_AUDIENCE=\"$CLIENT_ID\""
echo "export JWT_REQUIRED_ROLES=\"user,analyst,power-user,admin\""
echo ""
echo "Save this for reference:"
echo "  Okta Domain: $OKTA_DOMAIN"
echo "  Application ID: $APP_ID"
echo "  Client ID: $CLIENT_ID"
echo "  Client Secret: $CLIENT_SECRET"
echo ""
echo "Next steps:"
echo "  1. Create users in Okta"
echo "  2. Assign users to groups (Gateway-user, Gateway-analyst, etc.)"
echo "  3. Configure app to emit group names as roles claim"
echo "  4. Test JWT token generation"
echo ""
echo "To configure groups as claims:"
echo "  1. In Okta Admin Console, go to Apps > Your App > Sign On"
echo "  2. In 'OpenID Connect ID Token' section, click 'Edit'"
echo "  3. Add Groups claim with filter: startsWith(\"Gateway-\")"
echo "  4. Map to 'roles' claim in the token"
echo ""
