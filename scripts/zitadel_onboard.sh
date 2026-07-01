#!/usr/bin/env bash
#
# zitadel_onboard.sh — Onboard a gateway user into Zitadel via its REST APIs.
#
# Creates a human user, grants project roles (which the gateway maps into the
# request's AuthContext via the urn:zitadel:iam:org:project:roles claim), and
# optionally stamps per-user rate-limit / budget metadata that the gateway reads
# from the JWT (rate_limit_rpm / budget_limit_usd).
#
# It is intentionally idempotent-friendly: re-running with the same username will
# report the existing user rather than failing hard, and role grants are additive.
#
# ---------------------------------------------------------------------------
# Required environment:
#   ZITADEL_DOMAIN      Base URL, e.g. https://my-instance.zitadel.cloud
#   ZITADEL_PAT         Bearer token of a service user (Personal Access Token)
#                       with org-level user-management permissions
#                       (roles: ORG_OWNER or USER_MANAGER + PROJECT_OWNER for grants).
#
# Optional environment:
#   ZITADEL_PROJECT_ID  Default project for role grants (overridable via --project-id)
#   ZITADEL_ORG_ID      Target org; sent as x-zitadel-orgid when set
#
# Usage:
#   scripts/zitadel_onboard.sh \
#     --email jane@example.com --first-name Jane --last-name Doe \
#     --username jane \
#     [--password 'Initial#Pass1'] \
#     [--roles admin,user] \
#     [--project-id 3000...] \
#     [--rate-limit-rpm 600] \
#     [--budget-usd 50] \
#     [--verified]
#
# If --password is omitted, Zitadel sends the user an initialization email so
# they can set their own. The mapped gateway roles are whatever you grant here;
# "admin" grants AuthScope::Admin, "read-only"/"readonly"/"read" grant ReadOnly.
# ---------------------------------------------------------------------------

set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

# --- dependency + env checks ------------------------------------------------
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v jq   >/dev/null 2>&1 || die "jq is required"

: "${ZITADEL_DOMAIN:?set ZITADEL_DOMAIN (e.g. https://my-instance.zitadel.cloud)}"
: "${ZITADEL_PAT:?set ZITADEL_PAT (service-user bearer token)}"

ZITADEL_DOMAIN="${ZITADEL_DOMAIN%/}" # strip trailing slash

# --- arg parsing ------------------------------------------------------------
EMAIL="" FIRST_NAME="" LAST_NAME="" USERNAME="" PASSWORD=""
ROLES="" PROJECT_ID="${ZITADEL_PROJECT_ID:-}" RATE_LIMIT_RPM="" BUDGET_USD=""
VERIFIED="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --email)          EMAIL="$2"; shift 2 ;;
    --first-name)     FIRST_NAME="$2"; shift 2 ;;
    --last-name)      LAST_NAME="$2"; shift 2 ;;
    --username)       USERNAME="$2"; shift 2 ;;
    --password)       PASSWORD="$2"; shift 2 ;;
    --roles)          ROLES="$2"; shift 2 ;;
    --project-id)     PROJECT_ID="$2"; shift 2 ;;
    --rate-limit-rpm) RATE_LIMIT_RPM="$2"; shift 2 ;;
    --budget-usd)     BUDGET_USD="$2"; shift 2 ;;
    --verified)       VERIFIED="true"; shift ;;
    -h|--help)        sed -n '2,46p' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[[ -n "$EMAIL" ]]      || die "--email is required"
[[ -n "$USERNAME" ]]   || die "--username is required"
[[ -n "$FIRST_NAME" ]] || die "--first-name is required"
[[ -n "$LAST_NAME" ]]  || die "--last-name is required"

AUTH_HEADER="Authorization: Bearer ${ZITADEL_PAT}"
ORG_HEADER=()
[[ -n "${ZITADEL_ORG_ID:-}" ]] && ORG_HEADER=(-H "x-zitadel-orgid: ${ZITADEL_ORG_ID}")

# api METHOD PATH [JSON_BODY] -> echoes response body; exits non-zero on HTTP >=400
api() {
  local method="$1" path="$2" body="${3:-}"
  local args=(-sS -X "$method" "${ZITADEL_DOMAIN}${path}"
    -H "$AUTH_HEADER" -H "Content-Type: application/json"
    -w $'\n%{http_code}')
  [[ ${#ORG_HEADER[@]} -gt 0 ]] && args+=("${ORG_HEADER[@]}")
  [[ -n "$body" ]] && args+=(--data "$body")

  local out code
  out="$(curl "${args[@]}")"
  code="$(tail -n1 <<<"$out")"
  out="$(sed '$d' <<<"$out")"
  if [[ "$code" -ge 400 ]]; then
    echo "  HTTP $code from $method $path:" >&2
    echo "$out" | jq . >&2 2>/dev/null || echo "$out" >&2
    return 1
  fi
  echo "$out"
}

# --- 1. create the human user (Zitadel v2 user service) ---------------------
echo "==> Creating user '${USERNAME}' <${EMAIL}>"

pw_block="null"
if [[ -n "$PASSWORD" ]]; then
  pw_block="$(jq -n --arg p "$PASSWORD" '{password:{password:$p, changeRequired:true}}')"
fi

create_body="$(jq -n \
  --arg u "$USERNAME" --arg g "$FIRST_NAME" --arg f "$LAST_NAME" \
  --arg e "$EMAIL" --argjson v "$VERIFIED" --argjson pw "$pw_block" \
  '{
     username: $u,
     profile: { givenName: $g, familyName: $f },
     email: ( { email: $e, isVerified: $v }
              + ( if $v then {} else { sendCode: {} } end ) )
   } + ( if $pw == null then {} else $pw end )')"

if create_resp="$(api POST "/v2/users/human" "$create_body" 2>/tmp/zitadel_err)"; then
  USER_ID="$(jq -r '.userId // .user_id' <<<"$create_resp")"
  echo "  created userId=${USER_ID}"
else
  # Likely already exists — try to resolve the id so the rest still applies.
  if grep -qiE 'already exists|AlreadyExists|9 ' /tmp/zitadel_err; then
    echo "  user already exists; resolving id by username"
    search_body="$(jq -n --arg u "$USERNAME" \
      '{queries:[{userNameQuery:{userName:$u, method:"TEXT_QUERY_METHOD_EQUALS"}}]}')"
    search_resp="$(api POST "/v2/users" "$search_body")"
    USER_ID="$(jq -r '.result[0].userId // .result[0].user_id // empty' <<<"$search_resp")"
    [[ -n "$USER_ID" ]] || { cat /tmp/zitadel_err >&2; die "could not resolve existing user id"; }
    echo "  resolved userId=${USER_ID}"
  else
    cat /tmp/zitadel_err >&2
    die "user creation failed"
  fi
fi

# --- 2. grant project roles -------------------------------------------------
if [[ -n "$ROLES" ]]; then
  [[ -n "$PROJECT_ID" ]] || die "--roles requires --project-id or ZITADEL_PROJECT_ID"
  echo "==> Granting roles [${ROLES}] on project ${PROJECT_ID}"

  role_json="$(jq -cn --arg r "$ROLES" '($r|split(","))|map(gsub("^ +| +$";""))')"
  grant_body="$(jq -n --arg p "$PROJECT_ID" --argjson rk "$role_json" \
    '{projectId:$p, roleKeys:$rk}')"

  if api POST "/management/v1/users/${USER_ID}/grants" "$grant_body" >/dev/null 2>/tmp/zitadel_err; then
    echo "  granted"
  elif grep -qiE 'already exists|AlreadyExists' /tmp/zitadel_err; then
    echo "  grant already present (skipping)"
  else
    cat /tmp/zitadel_err >&2
    die "role grant failed"
  fi
fi

# --- 3. stamp gateway metadata (rate limit / budget) ------------------------
# Zitadel metadata values must be base64-encoded. The gateway reads these from
# the token's custom claims (rate_limit_rpm / budget_limit_usd) when the project
# is configured to project user metadata into tokens.
set_metadata() {
  local key="$1" value="$2"
  local b64; b64="$(printf '%s' "$value" | base64 | tr -d '\n')"
  local body; body="$(jq -n --arg v "$b64" '{value:$v}')"
  api POST "/management/v1/users/${USER_ID}/metadata/${key}" "$body" >/dev/null
  echo "  set ${key}=${value}"
}

if [[ -n "$RATE_LIMIT_RPM" || -n "$BUDGET_USD" ]]; then
  echo "==> Setting gateway metadata"
  [[ -n "$RATE_LIMIT_RPM" ]] && set_metadata "rate_limit_rpm" "$RATE_LIMIT_RPM"
  [[ -n "$BUDGET_USD" ]]     && set_metadata "budget_limit_usd" "$BUDGET_USD"
fi

echo "==> Done. userId=${USER_ID}"
[[ "$VERIFIED" == "false" && -z "$PASSWORD" ]] && \
  echo "    (initialization email sent to ${EMAIL})"
