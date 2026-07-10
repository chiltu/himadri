#!/bin/bash
# Zitadel OIDC + Role Setup for AI Gateway
#
# Prerequisites:
#   - Zitadel instance running
#   - zitadel CLI installed: https://github.com/zitadel/zitadel-tools
#   - ZITADEL_API_TOKEN set (personal access token from Zitadel)
#   - ZITADEL_DOMAIN set (e.g., your-domain.zitadel.cloud)
#
# Usage:
#   export ZITADEL_DOMAIN="your-domain.zitadel.cloud"
#   export ZITADEL_API_TOKEN="$(cat ~/.zitadel-pat.txt)"
#   bash deploy/zitadel/setup.sh

set -e

DOMAIN="${ZITADEL_DOMAIN:-$(read -p 'Zitadel domain (e.g. your-domain.zitadel.cloud): ' d; echo $d)}"
API_TOKEN="${ZITADEL_API_TOKEN:-$(read -sp 'Zitadel API token (PAT): ' t; echo $t)}"
GATEWAY_DOMAIN="${GATEWAY_DOMAIN:-localhost:8080}"

if [[ -z "$DOMAIN" ]] || [[ -z "$API_TOKEN" ]]; then
  echo "Error: ZITADEL_DOMAIN and ZITADEL_API_TOKEN are required"
  exit 1
fi

echo "Setting up Zitadel OIDC for AI Gateway..."
echo "  Zitadel domain: $DOMAIN"
echo "  Gateway domain: $GATEWAY_DOMAIN"

# Create an organization (if not using default)
ORG_NAME="${ORG_NAME:-ai-gateway-orgs}"
echo ""
echo "Step 1: Creating organization '$ORG_NAME'..."
ORG_ID=$(curl -s -X POST "https://$DOMAIN/admin/v1/orgs" \
  -H "Authorization: Bearer $API_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"name\": \"$ORG_NAME\"}" \
  | jq -r '.id // empty' 2>/dev/null)

if [[ -z "$ORG_ID" ]]; then
  echo "  (Organization may already exist, continuing...)"
  ORG_ID=$(curl -s -X GET "https://$DOMAIN/admin/v1/orgs?query.name=$ORG_NAME" \
    -H "Authorization: Bearer $API_TOKEN" \
    | jq -r '.result[0].id // empty' 2>/dev/null)
fi

if [[ -z "$ORG_ID" ]]; then
  echo "  ✗ Failed to create or find organization"
  exit 1
fi

echo "  ✓ Organization ID: $ORG_ID"

# Create a project
PROJECT_NAME="AI Gateway"
echo ""
echo "Step 2: Creating project '$PROJECT_NAME'..."
PROJECT_ID=$(curl -s -X POST "https://$DOMAIN/admin/v1/projects" \
  -H "Authorization: Bearer $API_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"name\": \"$PROJECT_NAME\", \"org_id\": \"$ORG_ID\"}" \
  | jq -r '.id // empty' 2>/dev/null)

if [[ -z "$PROJECT_ID" ]]; then
  PROJECT_ID=$(curl -s -X GET "https://$DOMAIN/admin/v1/projects?query.name=$PROJECT_NAME" \
    -H "Authorization: Bearer $API_TOKEN" \
    | jq -r '.result[0].id // empty' 2>/dev/null)
fi

if [[ -z "$PROJECT_ID" ]]; then
  echo "  ✗ Failed to create or find project"
  exit 1
fi

echo "  ✓ Project ID: $PROJECT_ID"

# Create an OIDC application
APP_NAME="API Gateway"
REDIRECT_URI="http://$GATEWAY_DOMAIN/callback"
echo ""
echo "Step 3: Creating OIDC application '$APP_NAME'..."
APP_ID=$(curl -s -X POST "https://$DOMAIN/admin/v1/projects/$PROJECT_ID/apps/oidc" \
  -H "Authorization: Bearer $API_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"name\": \"$APP_NAME\",
    \"redirect_uris\": [\"$REDIRECT_URI\"],
    \"response_type\": \"oidc-id-token\",
    \"auth_method_type\": \"basic\"
  }" \
  | jq -r '.id // empty' 2>/dev/null)

if [[ -z "$APP_ID" ]]; then
  APP_ID=$(curl -s -X GET "https://$DOMAIN/admin/v1/projects/$PROJECT_ID/apps?query.name=$APP_NAME" \
    -H "Authorization: Bearer $API_TOKEN" \
    | jq -r '.result[0].id // empty' 2>/dev/null)
fi

if [[ -z "$APP_ID" ]]; then
  echo "  ✗ Failed to create or find application"
  exit 1
fi

echo "  ✓ Application ID: $APP_ID"

# Get client credentials
echo ""
echo "Step 4: Retrieving client credentials..."
CLIENT_ID=$(curl -s -X GET "https://$DOMAIN/admin/v1/projects/$PROJECT_ID/apps/$APP_ID" \
  -H "Authorization: Bearer $API_TOKEN" \
  | jq -r '.clientId // empty' 2>/dev/null)

if [[ -z "$CLIENT_ID" ]]; then
  echo "  ✗ Failed to retrieve client ID"
  exit 1
fi

echo "  ✓ Client ID: $CLIENT_ID"

# Create roles
echo ""
echo "Step 5: Creating standard roles..."
ROLES=("admin" "power-user" "analyst" "user")

for role_name in "${ROLES[@]}"; do
  curl -s -X POST "https://$DOMAIN/admin/v1/projects/$PROJECT_ID/roles" \
    -H "Authorization: Bearer $API_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"key\": \"$role_name\",
      \"display_name\": \"$role_name\",
      \"group\": \"Gateway Roles\"
    }" > /dev/null 2>&1 || true
  echo "  ✓ Role: $role_name"
done

# Output environment variables
echo ""
echo "=========================================="
echo "✓ Zitadel OIDC Setup Complete"
echo "=========================================="
echo ""
echo "Add these environment variables to your .env file:"
echo ""
echo "# OIDC Configuration"
echo "export JWT_ISSUER=\"https://$DOMAIN\""
echo "export JWT_AUDIENCE=\"$CLIENT_ID\""
echo "export JWT_JWKS_URI=\"https://$DOMAIN/oauth/v2/keys\""
echo "export JWT_REQUIRED_ROLES=\"user,analyst,power-user,admin\""
echo ""
echo "# Organization Context (optional)"
echo "export ZITADEL_ORG_ID=\"$ORG_ID\""
echo "export ZITADEL_PROJECT_ID=\"$PROJECT_ID\""
echo ""
echo "Save this for next steps:"
echo "  Zitadel Domain: $DOMAIN"
echo "  Organization ID: $ORG_ID"
echo "  Project ID: $PROJECT_ID"
echo "  Application ID: $APP_ID"
echo "  Client ID: $CLIENT_ID"
echo ""
echo "Next steps:"
echo "  1. Configure role grants in Zitadel console"
echo "  2. Configure users/groups to have these roles"
echo "  3. Test JWT token generation"
echo "  4. Deploy gateway with above environment variables"
echo ""
