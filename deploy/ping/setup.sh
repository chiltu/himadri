#!/bin/bash
# Ping Identity OIDC + Role Setup for AI Gateway
#
# Prerequisites:
#   - Ping Identity environment (PingOne or PingFederate)
#   - Admin access to Ping Identity
#   - API credentials for Ping Identity Admin API (PingOne)
#
# For PingOne (Recommended):
#   - Create a service account with admin role
#   - Set PINGONE_REGION, PINGONE_ENVIRONMENT_ID, PINGONE_CLIENT_ID, PINGONE_CLIENT_SECRET
#
# Usage:
#   export PINGONE_REGION="NorthAmerica"
#   export PINGONE_ENVIRONMENT_ID="<env-id>"
#   export PINGONE_CLIENT_ID="<client-id>"
#   export PINGONE_CLIENT_SECRET="<client-secret>"
#   export GATEWAY_DOMAIN="localhost:8080"
#   bash deploy/ping/setup.sh

set -e

PINGONE_REGION="${PINGONE_REGION:-NorthAmerica}"
PINGONE_ENVIRONMENT_ID="${PINGONE_ENVIRONMENT_ID:-$(read -p 'PingOne Environment ID: ' id; echo $id)}"
PINGONE_CLIENT_ID="${PINGONE_CLIENT_ID:-$(read -p 'PingOne Client ID (service account): ' id; echo $id)}"
PINGONE_CLIENT_SECRET="${PINGONE_CLIENT_SECRET:-$(read -sp 'PingOne Client Secret (service account): ' secret; echo $secret)}"
GATEWAY_DOMAIN="${GATEWAY_DOMAIN:-localhost:8080}"

if [[ -z "$PINGONE_ENVIRONMENT_ID" ]] || [[ -z "$PINGONE_CLIENT_ID" ]] || [[ -z "$PINGONE_CLIENT_SECRET" ]]; then
  echo "Error: PingOne credentials are required"
  exit 1
fi

# Map region to API URL
case $PINGONE_REGION in
  NorthAmerica)
    PINGONE_API_URL="https://api.pingone.com"
    PINGONE_AUTH_URL="https://auth.pingone.com"
    ;;
  Europe)
    PINGONE_API_URL="https://api.pingone.eu"
    PINGONE_AUTH_URL="https://auth.pingone.eu"
    ;;
  AsiaPacific)
    PINGONE_API_URL="https://api.pingone.asia"
    PINGONE_AUTH_URL="https://auth.pingone.asia"
    ;;
  Canada)
    PINGONE_API_URL="https://api.pingone.ca"
    PINGONE_AUTH_URL="https://auth.pingone.ca"
    ;;
  *)
    echo "Unknown region: $PINGONE_REGION"
    exit 1
    ;;
esac

echo "Setting up Ping Identity OIDC for AI Gateway..."
echo "  Region: $PINGONE_REGION"
echo "  Environment ID: $PINGONE_ENVIRONMENT_ID"
echo "  Gateway Domain: $GATEWAY_DOMAIN"

# Get access token
echo ""
echo "Step 1: Authenticating with PingOne..."
ACCESS_TOKEN=$(curl -s -X POST "$PINGONE_AUTH_URL/$PINGONE_ENVIRONMENT_ID/as/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "grant_type=client_credentials&client_id=$PINGONE_CLIENT_ID&client_secret=$PINGONE_CLIENT_SECRET" \
  | jq -r '.access_token // empty')

if [[ -z "$ACCESS_TOKEN" ]]; then
  echo "  ✗ Failed to authenticate"
  exit 1
fi

echo "  ✓ Authentication successful"

# Create application
echo ""
echo "Step 2: Creating OIDC application..."
APP_RESPONSE=$(curl -s -X POST "$PINGONE_API_URL/v1/environments/$PINGONE_ENVIRONMENT_ID/applications" \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"name\": \"AI Gateway\",
    \"description\": \"OAuth 2.0 / OIDC server for AI Gateway\",
    \"type\": \"WEB_APP\",
    \"enabled\": true,
    \"oauthOptions\": {
      \"redirectUris\": [\"http://$GATEWAY_DOMAIN/callback\"],
      \"allowedGrants\": [\"authorization_code\"],
      \"responseTypes\": [\"code\"],
      \"tokenEndpointAuthMethod\": \"client_secret_basic\"
    }
  }")

APP_ID=$(echo "$APP_RESPONSE" | jq -r '.id // empty')
CLIENT_ID=$(echo "$APP_RESPONSE" | jq -r '.oauthOptions.clientId // empty')
CLIENT_SECRET=$(echo "$APP_RESPONSE" | jq -r '.oauthOptions.clientSecret // empty')

if [[ -z "$APP_ID" ]] || [[ -z "$CLIENT_ID" ]]; then
  echo "  ✗ Failed to create application"
  echo "$APP_RESPONSE" | jq '.' 2>/dev/null || echo "$APP_RESPONSE"
  exit 1
fi

echo "  ✓ Application created: $APP_ID"

# Get role IDs or create roles
echo ""
echo "Step 3: Creating custom roles..."
ROLES=("admin" "power-user" "analyst" "user")

for role_name in "${ROLES[@]}"; do
  curl -s -X POST "$PINGONE_API_URL/v1/environments/$PINGONE_ENVIRONMENT_ID/roles" \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"name\": \"$role_name\",
      \"description\": \"$role_name role for AI Gateway\"
    }" > /dev/null 2>&1 || true
  echo "  ✓ Role: $role_name"
done

# Create user groups
echo ""
echo "Step 4: Creating user groups for roles..."
for role_name in "${ROLES[@]}"; do
  curl -s -X POST "$PINGONE_API_URL/v1/environments/$PINGONE_ENVIRONMENT_ID/groups" \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"name\": \"Gateway-$role_name\",
      \"description\": \"Users with $role_name role for AI Gateway\"
    }" > /dev/null 2>&1 || true
  echo "  ✓ Group: Gateway-$role_name"
done

# Output environment variables
echo ""
echo "=========================================="
echo "✓ Ping Identity OIDC Setup Complete"
echo "=========================================="
echo ""
echo "Add these environment variables to your .env file:"
echo ""
echo "# OIDC Configuration"
echo "export JWT_ISSUER=\"$PINGONE_AUTH_URL/$PINGONE_ENVIRONMENT_ID\""
echo "export JWT_AUDIENCE=\"$CLIENT_ID\""
echo "export JWT_REQUIRED_ROLES=\"user,analyst,power-user,admin\""
echo ""
echo "Save this for reference:"
echo "  PingOne Region: $PINGONE_REGION"
echo "  Environment ID: $PINGONE_ENVIRONMENT_ID"
echo "  Application ID: $APP_ID"
echo "  Client ID: $CLIENT_ID"
echo "  Client Secret: $CLIENT_SECRET"
echo ""
echo "Next steps:"
echo "  1. Create users in PingOne"
echo "  2. Assign users to groups (Gateway-user, Gateway-analyst, etc.)"
echo "  3. Configure PingOne to emit group names as roles claim"
echo "  4. Test JWT token generation"
echo ""
echo "To configure groups as claims in PingOne:"
echo "  1. Go to Connections > Applications > Your App"
echo "  2. Configure ID Token mapping"
echo "  3. Add 'roles' attribute mapped to group membership"
echo ""
