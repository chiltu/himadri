const API_BASE = process.env.NEXT_PUBLIC_API_URL || "http://localhost:8080";

function getAuthToken(): string | null {
  if (typeof window === "undefined") return null;
  return localStorage.getItem("himadri_master_key");
}

export function setAuthToken(key: string) {
  localStorage.setItem("himadri_master_key", key);
}

export function clearAuthToken() {
  localStorage.removeItem("himadri_master_key");
}

export function isAuthenticated(): boolean {
  return getAuthToken() !== null;
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
  return res.json();
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

export interface Provider {
  id: string;
  name: string;
  enabled: boolean;
  api_key?: string;
  base_url?: string;
  weight: number;
  created_at: string;
  updated_at: string;
}

export interface CreateProviderRequest {
  name: string;
  enabled?: boolean;
  api_key?: string;
  base_url?: string;
  weight?: number;
}

export interface UpdateProviderRequest {
  name?: string;
  enabled?: boolean;
  api_key?: string;
  base_url?: string;
  weight?: number;
}

export interface Model {
  id: string;
  name: string;
  provider_id: string;
  display_name?: string;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface CreateModelRequest {
  name: string;
  provider_id: string;
  display_name?: string;
  enabled?: boolean;
}

export interface UpdateModelRequest {
  name?: string;
  provider_id?: string;
  display_name?: string;
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
  block_pii: boolean;
  block_toxicity: boolean;
  custom_patterns: string[];
}

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
  enabled: boolean;
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
  plugins: PluginConfig[];
  observability: {
    tracing: TracingConfig;
    metrics: MetricsConfig;
  };
  rate_limit: RateLimitConfig;
  admin: AdminConfig;
  orgs: Record<string, OrgConfig>;
  cors: CorsConfig;
  rbac: RbacConfig;
}

export interface Target {
  provider: string;
  weight: number;
  models?: string[];
  api_key_env?: string;
  base_url?: string;
}

export interface PluginConfig {
  name: string;
  enabled: boolean;
  config?: Record<string, unknown>;
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
  // Auth
  testAuth: (key: string) =>
    fetch(`${API_BASE}/admin/dashboard`, {
      headers: { Authorization: `Bearer ${key}` },
    }).then((r) => r.ok),

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

  // Providers
  listProviders: () => request<Provider[]>("/admin/providers"),
  createProvider: (data: CreateProviderRequest) =>
    request<Provider>("/admin/providers", { method: "POST", body: JSON.stringify(data) }),
  getProvider: (id: string) => request<Provider>(`/admin/providers/${id}`),
  updateProvider: (id: string, data: UpdateProviderRequest) =>
    request<Provider>(`/admin/providers/${id}`, { method: "PUT", body: JSON.stringify(data) }),
  deleteProvider: (id: string) =>
    request<void>(`/admin/providers/${id}`, { method: "DELETE" }),
  toggleProvider: (id: string, enabled: boolean) =>
    request<Provider>(`/admin/providers/${id}/toggle`, { method: "POST", body: JSON.stringify({ enabled }) }),

  // Models
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
