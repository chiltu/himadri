const API_BASE = process.env.NEXT_PUBLIC_API_URL || "http://localhost:8080";

/** Browser storage key for the admin session token (a short-lived JWT from
 * `POST /auth/admin/login`). The legacy name is kept so sessions created
 * before the master-key login was removed migrate cleanly. */
const AUTH_TOKEN_KEY = "himadri_master_key";

/**
 * Read the admin session token.
 *
 * Prefer `sessionStorage` so the token is not written to durable disk storage
 * and is cleared when the tab/window closes. Fall back to (and migrate away
 * from) legacy `localStorage` entries so existing sessions keep working once.
 *
 * Note: any token readable from JS remains XSS-sensitive; httpOnly cookie
 * sessions are the proper long-term fix for production multi-user dashboards.
 * The blast radius is bounded now: this is a short-lived login JWT, not the
 * gateway master key.
 */
function getAuthToken(): string | null {
  if (typeof window === "undefined") return null;
  const fromSession = sessionStorage.getItem(AUTH_TOKEN_KEY);
  if (fromSession) return fromSession;

  const legacy = localStorage.getItem(AUTH_TOKEN_KEY);
  if (legacy) {
    sessionStorage.setItem(AUTH_TOKEN_KEY, legacy);
    localStorage.removeItem(AUTH_TOKEN_KEY);
    return legacy;
  }
  return null;
}

export function setAuthToken(key: string) {
  sessionStorage.setItem(AUTH_TOKEN_KEY, key);
  // Drop any prior durable copy so the secret is not left on disk.
  localStorage.removeItem(AUTH_TOKEN_KEY);
}

export function clearAuthToken() {
  sessionStorage.removeItem(AUTH_TOKEN_KEY);
  localStorage.removeItem(AUTH_TOKEN_KEY);
}

export function isAuthenticated(): boolean {
  return getAuthToken() !== null;
}

/** Exported for non-`request()` callers (e.g. streaming playground fetch). */
export function getSessionToken(): string | null {
  return getAuthToken();
}

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const token = getAuthToken();
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    ...((options?.headers as Record<string, string>) || {}),
  };
  if (token) {
    headers["Authorization"] = `Bearer ${token}`;
  }

  const res = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers,
  });

  if (res.status === 401) {
    clearAuthToken();
    throw new Error("Unauthorized — please log in again");
  }

  if (!res.ok) {
    const body = await res.text();
    throw new Error(`API error ${res.status}: ${body}`);
  }

  // 204 No Content (e.g. successful DELETE) and other empty-body responses have
  // no JSON to parse — calling res.json() on them throws "Unexpected end of JSON
  // input". Return undefined so callers expecting no payload resolve cleanly.
  if (res.status === 204 || res.headers.get("content-length") === "0") {
    return undefined as T;
  }
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

export interface ApiKey {
  id: string;
  name: string;
  key: string;
  scopes: string[];
  enabled: boolean;
  created_at: string;
  last_used_at?: string;
  expires_at?: string;
  usage_count: number;
  metadata?: Record<string, unknown>;
  org_id?: string;
  team_id?: string;
  user_id?: string;
  models?: string[];
  rate_limit_override?: {
    requests_per_second?: number;
    burst_size?: number;
  };
  token_budget?: {
    max_tokens_per_request?: number;
    max_tokens_per_day?: number;
    max_tokens_per_month?: number;
    cost_limit_per_day?: number;
    cost_limit_per_month?: number;
  };
}

export interface CreateApiKeyRequest {
  name: string;
  scopes?: string[];
  expires_at?: string;
  metadata?: Record<string, unknown>;
  org_id?: string;
  team_id?: string;
  user_id?: string;
  models?: string[];
  rate_limit_override?: {
    requests_per_second?: number;
    burst_size?: number;
  };
  token_budget?: {
    max_tokens_per_request?: number;
    max_tokens_per_day?: number;
    max_tokens_per_month?: number;
    cost_limit_per_day?: number;
    cost_limit_per_month?: number;
  };
}

export interface UpdateApiKeyRequest {
  name?: string;
  scopes?: string[];
  enabled?: boolean;
  org_id?: string | null;
  team_id?: string | null;
  user_id?: string | null;
  models?: string[] | null;
  rate_limit_override?: {
    requests_per_second?: number;
    burst_size?: number;
  } | null;
  token_budget?: {
    max_tokens_per_request?: number;
    max_tokens_per_day?: number;
    max_tokens_per_month?: number;
    cost_limit_per_day?: number;
    cost_limit_per_month?: number;
  } | null;
}

/** A model is a first-party entity that owns one or more provider endpoints.
 *  It is "active" (routable) only when it has at least one enabled endpoint. */
export interface Model {
  id: string;
  name: string;
  display_name?: string;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface CreateModelRequest {
  name: string;
  display_name?: string;
  enabled?: boolean;
}

export interface UpdateModelRequest {
  name?: string;
  display_name?: string;
  enabled?: boolean;
}

/** One provider route for a model: a provider type + credentials + weight. */
export interface ModelEndpoint {
  id: string;
  model_id: string;
  provider_type: string;
  base_url?: string;
  api_key?: string;
  /** Routing weight among the model's endpoints. Defaults to 1.0. */
  weight: number;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface CreateModelEndpointRequest {
  provider_type: string;
  base_url?: string;
  api_key?: string;
  weight?: number;
  enabled?: boolean;
}

export interface UpdateModelEndpointRequest {
  provider_type?: string;
  base_url?: string;
  api_key?: string;
  weight?: number;
  enabled?: boolean;
}

export interface DashboardSummary {
  total_requests: number;
  total_tokens: number;
  total_cost_usd: number;
  avg_latency_ms: number;
  error_rate: number;
  top_models: ModelUsage[];
  top_providers: ProviderUsage[];
  recent_errors: UsageRecord[];
}

export interface ModelUsage {
  model: string;
  requests: number;
  tokens: number;
  cost_usd: number;
}

export interface ProviderUsage {
  provider: string;
  requests: number;
  tokens: number;
  cost_usd: number;
}

export interface UsageRecord {
  request_id: string;
  api_key_id?: string;
  model: string;
  provider: string;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cost_usd: number;
  latency_ms: number;
  created_at: string;
  success: boolean;
  error_message?: string;
}

export interface UsageStats {
  total_requests: number;
  total_tokens: number;
  total_cost_usd: number;
  avg_latency_ms: number;
  error_rate: number;
}

export type StrategyMode =
  | "single"
  | "fallback"
  | "load_balance"
  | "least_latency"
  | "cost_optimized"
  | "conditional"
  | "content_based"
  | "ab_test";

export interface StrategyConfig {
  mode: StrategyMode;
  fallback_timeout_ms: number;
  conditional_rules: ConditionalRuleConfig[];
  content_rules: ContentRuleConfig[];
  ab_variants: ABVariantConfig[];
  strategy_fallback?: Target | null;
}

export interface ConditionalRuleConfig {
  key: "model" | "model_prefix";
  value: string;
  target: Target;
}

export interface ContentRuleConfig {
  condition_type: "prompt_contains" | "prompt_not_contains" | "prompt_regex";
  value: string;
  target: Target;
}

export interface ABVariantConfig {
  target: Target;
  weight: number;
  label: string;
}

export interface RateLimitConfig {
  enabled: boolean;
  requests_per_second: number;
  burst_size: number;
}

export interface RolePolicy {
  models?: string[] | null;
  providers?: string[] | null;
}

export interface RbacConfig {
  enabled: boolean;
  roles: Record<string, RolePolicy>;
  default_role?: string | null;
}

export interface OrgTokenBudget {
  max_tokens_per_request?: number;
  max_tokens_per_day?: number;
  max_tokens_per_month?: number;
  cost_limit_per_day?: number;
  cost_limit_per_month?: number;
}

export interface ContentFilterConfig {
  enabled: boolean;
  /** Deprecated: use PiiGuardrailConfig (guardrails.pii) with mode "block". */
  block_pii: boolean;
  block_toxicity: boolean;
  custom_patterns: string[];
}

export type PiiMode = "redact" | "block" | "observe";
export type PiiStrategy = "replace" | "mask" | "hash" | "encrypt" | "remove";
export type PiiResponseMode = "off" | "observe" | "redact" | "block";

/** PII detection/redaction settings (global default on GatewayConfig.guardrails,
 * per-scope override on org/team guardrails.pii — a present override replaces
 * the global settings wholesale, including enabled=false to opt out). */
export interface PiiGuardrailConfig {
  enabled: boolean;
  mode: PiiMode;
  strategy: PiiStrategy;
  /** Entity types to act on (e.g. EMAIL_ADDRESS, US_SSN); null/absent = all. */
  entities?: string[] | null;
  min_confidence: number;
  /** Message roles scanned (user/system/assistant/tool). */
  apply_to: string[];
  scan_tool_arguments: boolean;
  fail_open: boolean;
  /** Model-output scanning; enforced on non-streaming responses
   * (streams: post-hoc at end-of-stream only). */
  response_mode: PiiResponseMode;
}

export interface GuardrailsConfig {
  pii: PiiGuardrailConfig;
}

export const DEFAULT_PII_GUARDRAIL_CONFIG: PiiGuardrailConfig = {
  enabled: false,
  mode: "redact",
  strategy: "replace",
  entities: null,
  min_confidence: 0.6,
  apply_to: ["user", "system", "tool"],
  scan_tool_arguments: false,
  fail_open: false,
  response_mode: "off",
};

export interface AuditConfig {
  enabled: boolean;
  log_requests: boolean;
  log_responses: boolean;
  redact_pii: boolean;
  retention_days?: number;
}

export interface OrgGuardrailConfig {
  enabled: boolean;
  blocked_words: string[];
  max_tokens_per_request?: number;
  content_filter?: ContentFilterConfig | null;
  /** Org/team-scope PII override; replaces the global guardrails.pii wholesale. */
  pii?: PiiGuardrailConfig | null;
  audit: AuditConfig;
}

export interface TeamConfig {
  name?: string | null;
  enabled: boolean;
  allowed_models?: string[] | null;
  blocked_models?: string[] | null;
  rate_limit?: RateLimitConfig | null;
  token_budget?: OrgTokenBudget | null;
  guardrails: OrgGuardrailConfig;
}

export interface OrgConfig {
  name?: string | null;
  enabled: boolean;
  allowed_models?: string[] | null;
  blocked_models?: string[] | null;
  rate_limit?: RateLimitConfig | null;
  token_budget?: OrgTokenBudget | null;
  guardrails: OrgGuardrailConfig;
  teams: Record<string, TeamConfig>;
}

export interface CorsConfig {
  enabled: boolean;
  allowed_origins: string[];
  allowed_methods: string[];
  allowed_headers: string[];
}

export interface AdminConfig {
  master_key?: string;
}

export interface TracingConfig {
  enabled: boolean;
  service_name: string;
  endpoint?: string;
  sample_ratio: number;
}

export interface MetricsConfig {
  enabled: boolean;
  path: string;
}

export interface GatewayConfig {
  strategy: StrategyConfig;
  targets: Target[];
  observability: {
    tracing: TracingConfig;
    metrics: MetricsConfig;
  };
  rate_limit: RateLimitConfig;
  admin: AdminConfig;
  orgs: Record<string, OrgConfig>;
  cors: CorsConfig;
  rbac: RbacConfig;
  guardrails: GuardrailsConfig;
}

export interface Target {
  provider: string;
  weight: number;
  models?: string[];
  api_key_env?: string;
  base_url?: string;
}

export interface ConfigHistoryEntry {
  version: number;
  updated_at: string;
  config: GatewayConfig;
  rolled_back_from?: number;
}

export interface RequestLogEntry {
  trace_id: string;
  stage: string;
  model: string;
  provider: string;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  error_message?: string;
  created_at: string;
}

export interface RequestLogListResult {
  data: RequestLogEntry[];
  total: number;
}

export interface RequestLogQuery {
  model?: string;
  provider?: string;
  since?: string;
  before?: string;
}

export const api = {
  // Keys
  listKeys: () => request<ApiKey[]>("/admin/keys"),
  createKey: (data: CreateApiKeyRequest) =>
    request<ApiKey>("/admin/keys", { method: "POST", body: JSON.stringify(data) }),
  getKey: (id: string) => request<ApiKey>(`/admin/keys/${id}`),
  updateKey: (id: string, data: UpdateApiKeyRequest) =>
    request<ApiKey>(`/admin/keys/${id}`, { method: "PUT", body: JSON.stringify(data) }),
  deleteKey: (id: string) =>
    request<void>(`/admin/keys/${id}`, { method: "DELETE" }),
  revokeKey: (id: string) =>
    request<ApiKey>(`/admin/keys/${id}/revoke`, { method: "POST" }),
  rotateKey: (id: string) =>
    request<ApiKey>(`/admin/keys/${id}/rotate`, { method: "POST" }),

  // Models (first-party)
  listModels: () => request<Model[]>("/admin/models"),
  createModel: (data: CreateModelRequest) =>
    request<Model>("/admin/models", { method: "POST", body: JSON.stringify(data) }),
  getModel: (id: string) => request<Model>(`/admin/models/${id}`),
  updateModel: (id: string, data: UpdateModelRequest) =>
    request<Model>(`/admin/models/${id}`, { method: "PUT", body: JSON.stringify(data) }),
  deleteModel: (id: string) =>
    request<void>(`/admin/models/${id}`, { method: "DELETE" }),
  toggleModel: (id: string, enabled: boolean) =>
    request<Model>(`/admin/models/${id}/toggle`, { method: "POST", body: JSON.stringify({ enabled }) }),

  // Model endpoints (a model's provider routes)
  listAllEndpoints: () => request<ModelEndpoint[]>("/admin/endpoints"),
  listEndpoints: (modelId: string) =>
    request<ModelEndpoint[]>(`/admin/models/${modelId}/endpoints`),
  createEndpoint: (modelId: string, data: CreateModelEndpointRequest) =>
    request<ModelEndpoint>(`/admin/models/${modelId}/endpoints`, {
      method: "POST",
      body: JSON.stringify(data),
    }),
  updateEndpoint: (id: string, data: UpdateModelEndpointRequest) =>
    request<ModelEndpoint>(`/admin/endpoints/${id}`, { method: "PUT", body: JSON.stringify(data) }),
  deleteEndpoint: (id: string) =>
    request<void>(`/admin/endpoints/${id}`, { method: "DELETE" }),
  toggleEndpoint: (id: string, enabled: boolean) =>
    request<ModelEndpoint>(`/admin/endpoints/${id}/toggle`, {
      method: "POST",
      body: JSON.stringify({ enabled }),
    }),
  knownProviders: () =>
    request<{ data: string[] }>("/admin/known-providers").then((r) => r.data),

  /**
   * Dev/break-glass admin login (enabled on the gateway via
   * DEV_ADMIN_PASSWORD). Exchanges a username+password for a short-lived
   * admin JWT; store it with setAuthToken. 404 means the mechanism is
   * disabled on this gateway.
   */
  adminLogin: (username: string, password: string) =>
    request<{ access_token: string; token_type: string; expires_in: number }>(
      "/auth/admin/login",
      { method: "POST", body: JSON.stringify({ username, password }) },
    ),

  // Dashboard & Usage
  dashboard: () => request<DashboardSummary>("/admin/dashboard"),
  usageStats: () => request<UsageStats>("/admin/usage"),
  keyUsageStats: (keyId: string) => request<UsageStats>(`/admin/usage/${keyId}`),

  // Config
  getConfig: () => request<GatewayConfig>("/admin/config"),
  updateConfig: (config: GatewayConfig) =>
    request<{ status: string }>("/admin/config", { method: "PUT", body: JSON.stringify(config) }),
  configHistory: () =>
    request<{ data: ConfigHistoryEntry[]; summary: { total_versions: number } }>("/admin/config/history"),
  rollbackConfig: (version: number) =>
    request<{ status: string }>(`/admin/config/rollback/${version}`, { method: "POST" }),

  // Logs
  listLogs: (query?: RequestLogQuery) => {
    const params = new URLSearchParams();
    if (query?.model) params.set("model", query.model);
    if (query?.provider) params.set("provider", query.provider);
    if (query?.since) params.set("since", query.since);
    if (query?.before) params.set("before", query.before);
    const qs = params.toString();
    return request<RequestLogListResult>(`/admin/logs${qs ? `?${qs}` : ""}`);
  },
  deleteLogs: (query?: RequestLogQuery) => {
    const params = new URLSearchParams();
    if (query?.since) params.set("since", query.since);
    if (query?.before) params.set("before", query.before);
    const qs = params.toString();
    return request<{ deleted: number }>(`/admin/logs${qs ? `?${qs}` : ""}`, { method: "DELETE" });
  },

  // System
  reloadConfig: () =>
    request<{ status: string }>("/admin/reload", { method: "POST" }),
};
