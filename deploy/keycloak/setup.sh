#!/bin/bash
# Keycloak OIDC + Role Setup for AI Gateway
#
# Prerequisites:
#   - Keycloak instance running
#   - KEYCLOAK_URL set (e.g., http://localhost:8080/auth)
#   - KEYCLOAK_ADMIN, KEYCLOAK_PASSWORD set
#
# Usage:
#   export KEYCLOAK_URL="http://localhost:8080"
#   export KEYCLOAK_ADMIN="admin"
#   export KEYCLOAK_PASSWORD="<password>"
#   bash deploy/keycloak/setup.sh

set -e

KEYCLOAK_URL="${KEYCLOAK_URL:-$(read -p 'Keycloak URL (e.g. http://localhost:8080): ' u; echo $u)}"
KEYCLOAK_ADMIN="${KEYCLOAK_ADMIN:-$(read -p 'Keycloak admin username: ' u; echo $u)}"
KEYCLOAK_PASSWORD="${KEYCLOAK_PASSWORD:-$(read -sp 'Keycloak admin password: ' p; echo $p)}"
GATEWAY_DOMAIN="${GATEWAY_DOMAIN:-localhost:8080}"
REALM_NAME="${REALM_NAME:-ai-gateway}"

if [[ -z "$KEYCLOAK_URL" ]] || [[ -z "$KEYCLOAK_ADMIN" ]] || [[ -z "$KEYCLOAK_PASSWORD" ]]; then
  echo "Error: KEYCLOAK_URL, KEYCLOAK_ADMIN, and KEYCLOAK_PASSWORD are required"
  exit 1
fi

echo "Setting up Keycloak OIDC for AI Gateway..."
echo "  Keycloak URL: $KEYCLOAK_URL"
echo "  Realm: $REALM_NAME"
echo "  Gateway domain: $GATEWAY_DOMAIN"

# Get access token
echo ""
echo "Step 1: Authenticating with Keycloak..."
TOKEN=$(curl -s -X POST "$KEYCLOAK_URL/realms/master/protocol/openid-connect/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "grant_type=password&client_id=admin-cli&username=$KEYCLOAK_ADMIN&password=$KEYCLOAK_PASSWORD" \
  | jq -r '.access_token // empty')

if [[ -z "$TOKEN" ]]; then
  echo "  ✗ Failed to authenticate"
  exit 1
fi

echo "  ✓ Authentication successful"

# Create realm
echo ""
echo "Step 2: Creating realm '$REALM_NAME'..."
REALM_RESPONSE=$(curl -s -X POST "$KEYCLOAK_URL/admin/realms" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"realm\": \"$REALM_NAME\",
    \"enabled\": true,
    \"accessTokenLifespan\": 36000,
    \"accessTokenLifespanForImplicitFlow\": 900,
    \"actionTokenGeneratedByAdminLifespan\": 43200,
    \"actionTokenGeneratedByUserLifespan\": 300
  }")

echo "  ✓ Realm created"

# Create client scope
echo ""
echo "Step 3: Creating client scope..."
SCOPE_ID=$(curl -s -X POST "$KEYCLOAK_URL/admin/realms/$REALM_NAME/client-scopes" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"name\": \"roles\",
    \"description\": \"OpenID Connect scope for roles\",
    \"protocol\": \"openid-connect\",
    \"attributes\": {
      \"display.on.consent.screen\": \"false\"
    }
  }" | jq -r '.id // empty')

echo "  ✓ Client scope created"

# Create OIDC client
echo ""
echo "Step 4: Creating OIDC client..."
CLIENT_RESPONSE=$(curl -s -X POST "$KEYCLOAK_URL/admin/realms/$REALM_NAME/clients" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"clientId\": \"ai-gateway\",
    \"name\": \"AI Gateway\",
    \"description\": \"OAuth 2.0 / OIDC server for AI Gateway\",
    \"enabled\": true,
    \"publicClient\": false,
    \"authenticatorFlowBindingOverrides\": {},
    \"redirectUris\": [\"http://$GATEWAY_DOMAIN/callback\"],
    \"webOrigins\": [\"http://$GATEWAY_DOMAIN\"],
    \"notBefore\": 0,
    \"bearerOnly\": false,
    \"consentRequired\": false,
    \"clientAuthenticatorType\": \"client-secret\",
    \"defaultClientScopes\": [\"web-origins\", \"profile\", \"email\", \"roles\"],
    \"optionalClientScopes\": [\"address\", \"phone\", \"offline_access\"],
    \"access\": {
      \"view\": true,
      \"configure\": true,
      \"manage\": true
    }
  }")

CLIENT_ID=$(echo "$CLIENT_RESPONSE" | jq -r '.id // empty')
NEW_CLIENT_ID=$(echo "$CLIENT_RESPONSE" | jq -r '.clientId // empty')

if [[ -z "$CLIENT_ID" ]]; then
  echo "  ✗ Failed to create client"
  exit 1
fi

echo "  ✓ Client created: $NEW_CLIENT_ID"

# Get client secret
echo ""
echo "Step 5: Retrieving client credentials..."
CLIENT_SECRET=$(curl -s -X GET "$KEYCLOAK_URL/admin/realms/$REALM_NAME/clients/$CLIENT_ID/client-secret" \
  -H "Authorization: Bearer $TOKEN" \
  | jq -r '.value // empty')

if [[ -z "$CLIENT_SECRET" ]]; then
  echo "  ✗ Failed to retrieve client secret"
  exit 1
fi

echo "  ✓ Client secret retrieved"

# Create roles
echo ""
echo "Step 6: Creating standard roles..."
ROLES=("admin" "power-user" "analyst" "user")

for role_name in "${ROLES[@]}"; do
  curl -s -X POST "$KEYCLOAK_URL/admin/realms/$REALM_NAME/roles" \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"name\": \"$role_name\",
      \"description\": \"$role_name role for AI Gateway\"
    }" > /dev/null 2>&1 || true
  echo "  ✓ Role: $role_name"
done

# Output environment variables
echo ""
echo "=========================================="
echo "✓ Keycloak OIDC Setup Complete"
echo "=========================================="
echo ""
echo "Add these environment variables to your .env file:"
echo ""
echo "# OIDC Configuration"
echo "export JWT_ISSUER=\"$KEYCLOAK_URL/realms/$REALM_NAME\""
echo "export JWT_AUDIENCE=\"$NEW_CLIENT_ID\""
echo "export JWT_REQUIRED_ROLES=\"user,analyst,power-user,admin\""
echo ""
echo "Save this for reference:"
echo "  Keycloak URL: $KEYCLOAK_URL"
echo "  Realm: $REALM_NAME"
echo "  Client ID: $NEW_CLIENT_ID"
echo "  Client Secret: $CLIENT_SECRET"
echo ""
echo "Next steps:"
echo "  1. Create users in Keycloak"
echo "  2. Assign roles to users"
echo "  3. Test JWT token generation"
echo "  4. Deploy gateway with above environment variables"
echo ""
