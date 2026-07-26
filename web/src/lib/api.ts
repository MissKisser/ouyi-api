const TOKEN_KEY = 'ds-admin-token';

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string) {
  localStorage.setItem(TOKEN_KEY, token);
}
export function clearToken() {
  localStorage.removeItem(TOKEN_KEY);
}

let onUnauthorized: (() => void) | null = null;

/** 注册 401 回调：收到 401 时自动调用（用于 AuthProvider 同步 token 状态） */
export function setOnUnauthorized(cb: (() => void) | null) {
  onUnauthorized = cb;
}

export async function apiFetch<T>(path: string, init?: RequestInit & { signal?: AbortSignal }): Promise<T> {
  const token = getToken();
  const headers: Record<string, string> = {
    'Accept': 'application/json',
    'Content-Type': 'application/json',
    ...(init?.headers as Record<string, string> ?? {}),
  };
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const res = await fetch(path, { ...init, headers });
  if (res.status === 401) {
    clearToken();
    onUnauthorized?.();
    throw new AuthError('Unauthorized');
  }
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new ApiError(res.status, body.error || `API error: ${res.status}`);
  }
  return res.json();
}

export class AuthError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'AuthError';
  }
}

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

// ── Auth API ──────────────────────────────────────────────────────────────

export interface LoginResponse {
  token: string;
}

export async function apiSetup(password: string): Promise<LoginResponse> {
  return apiFetch<LoginResponse>('/admin/api/setup', {
    method: 'POST',
    body: JSON.stringify({ password }),
  });
}

export async function apiLogin(password: string): Promise<LoginResponse> {
  return apiFetch<LoginResponse>('/admin/api/login', {
    method: 'POST',
    body: JSON.stringify({ password }),
  });
}

// ── Data Types ────────────────────────────────────────────────────────────

export interface RequestLog {
  timestamp: number;
  request_id: string;
  model: string;
  api_key: string;
  prompt_tokens: number;
  completion_tokens: number;
  latency_ms: number;
  success: boolean;
}

export interface RuntimeLogEntry {
  timestamp: string;
  level: string;
  target: string;
  message: string;
}

export interface RuntimeLogsResponse {
  total: number;
  offset: number;
  limit: number;
  logs: RuntimeLogEntry[];
}

export interface ToolRuntimeLog {
  timestamp: string;
  request_id: string;
  endpoint: string;
  mode: string;
  provider: string;
  model: string;
  base_url: string;
  decision: string;
  tool_names: string[];
  prompt_tokens: number;
  completion_tokens: number;
  latency_ms: number;
  success: boolean;
  error?: string;
}

export interface ToolRuntimeConfig {
  enabled: boolean;
  provider: 'anthropic' | 'openai-compatible';
  base_url: string;
  api_key: string;
  model: string;
  timeout_secs: number;
}

export interface ToolRuntimeTestResponse {
  success: boolean;
  message: string;
  latency_ms: number;
  mode: string;
  provider: string;
  model: string;
  base_url: string;
}

export interface AccountStatus {
  email: string;
  mobile: string;
  state: string;
  last_released_ms: number;
  error_count: number;
}

export type OuyiPlatform = 'ouyi7' | 'ouyi8';

export interface PlatformStatus {
  platform: OuyiPlatform;
  label: string;
  base_url: string;
  model_count: number;
  token_valid: boolean;
  last_selected_at?: string;
}

export interface AdminStatusResponse {
  accounts: AccountStatus[];
  total: number;
  idle: number;
  busy: number;
  error: number;
  invalid: number;
  platform: PlatformStatus;
}

export interface StatsSnapshot {
  total_requests: number;
  success_requests: number;
  failed_requests: number;
  avg_latency_ms: number;
  total_prompt_tokens: number;
  total_completion_tokens: number;
  uptime_secs: number;
  models: Record<string, { prompt_tokens: number; completion_tokens: number; requests: number }>;
  keys: Record<string, { prompt_tokens: number; completion_tokens: number; requests: number }>;
}

export interface ModelInfo {
  id: string;
  object: string;
  created: number;
  owned_by: string;
}

export interface ModelListResponse {
  object: string;
  data: ModelInfo[];
}

export interface CategorizedModelItem {
  label: string;
  value: string;
  provider_key: string;
}

export interface CategorizedModelGroup {
  provider_name: string;
  provider_key: string;
  count: number;
  models: CategorizedModelItem[];
}

export interface ModelInfoExtended extends ModelInfo {
  display?: string;
}

// ── Config Types (mirrors backend response) ───────────────────────────────

export interface ServerConfig {
  host: string;
  port: number;
  cors_origins: string[];
}

export interface LimitsConfig {
  max_concurrent_requests: number;
}

export interface ToolCallTagConfig {
  extra_starts: string[];
  extra_ends: string[];
}

export interface DeepSeekConfig {
  api_base: string;
  wasm_url: string;
  user_agent: string;
  client_version: string;
  client_platform: string;
  client_locale: string;
  /** JWT token for ouyi API auth (masked) */
  jwt_token: string;
  model_refresh_interval_secs: number;
  model_types: string[];
  max_input_tokens: number[];
  max_output_tokens: number[];
  model_aliases: string[];
  tool_call: ToolCallTagConfig;
}

export interface ProxyConfig {
  url: string | null;
}

export interface AdminConfigResponse {
  password_set: boolean;
  jwt_issued_at: number;
}

export interface AccountEntry {
  email: string;
  mobile: string;
  area_code: string;
  password: string;
  token?: string;
  enabled: boolean;
}

export interface ApiKeyEntry {
  key: string;
  description: string;
}

export interface CategorizedModelsResponse {
  platform: OuyiPlatform;
  categories: CategorizedModelGroup[];
}

export interface Ouyi7Config {
  base_url: string;
  chatapi_prefix: string;
  jwt_token: string;
  default_model: string;
  model_refresh_interval_secs: number;
  max_history_messages: number;
  simulated_stream_chunk_chars: number;
}

export interface PlatformConfig {
  default: OuyiPlatform;
  prompt_on_start: boolean;
}

export interface FullConfig {
  server: ServerConfig;
  limits: LimitsConfig;
  deepseek: DeepSeekConfig;
  proxy: ProxyConfig;
  admin: AdminConfigResponse;
  accounts: AccountEntry[];
  api_keys: ApiKeyEntry[];
  tool_runtime: ToolRuntimeConfig;
  openai_tool_runtime: ToolRuntimeConfig;
  platform: PlatformConfig | null;
  ouyi_v7: Ouyi7Config | null;
}

// ── Config API ────────────────────────────────────────────────────────────

export async function apiFetchConfig(): Promise<FullConfig> {
  return apiFetch<FullConfig>('/admin/api/config');
}

export async function apiSaveConfig(config: Record<string, unknown>): Promise<{ ok: boolean }> {
  return apiFetch<{ ok: boolean }>('/admin/api/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}

export async function apiSaveConcurrencyLimit(value: number): Promise<FullConfig> {
  await apiSaveConfig({ limits: { max_concurrent_requests: value } });
  const fresh = await apiFetchConfig();
  if (fresh.limits.max_concurrent_requests !== value) {
    throw new Error('Concurrency limit was not persisted');
  }
  return fresh;
}

/**
 * 切换平台（即时生效，无需重启）。把 platform 配置作为 PUT body 提交，
 * 后端 prepare_and_commit_config_update 会重建 ouyi_client、切换 state.platform、
 * 触发模型列表重载。保存后立即拉取最新配置并校验 default 已落盘。
 */
export async function apiSavePlatform(platform: PlatformConfig): Promise<FullConfig> {
  await apiSaveConfig({ platform });
  const fresh = await apiFetchConfig();
  const persisted = fresh.platform?.default;
  if (persisted !== platform.default) {
    throw new Error('Platform switch was not persisted');
  }
  return fresh;
}

export async function apiTestToolRuntime(config: ToolRuntimeConfig): Promise<ToolRuntimeTestResponse> {
  return apiFetch<ToolRuntimeTestResponse>('/admin/api/tool-runtime/test', {
    method: 'POST',
    body: JSON.stringify(config),
  });
}

export async function apiTestOpenAiToolRuntime(config: ToolRuntimeConfig): Promise<ToolRuntimeTestResponse> {
  return apiFetch<ToolRuntimeTestResponse>('/admin/api/openai-tool-runtime/test', {
    method: 'POST',
    body: JSON.stringify(config),
  });
}

// ── Logs ──────────────────────────────────────────────────────────────────

export async function apiFetchLogs(limit?: number): Promise<RequestLog[]> {
  const path = limit ? `/admin/api/logs?limit=${limit}` : '/admin/api/logs';
  return apiFetch<RequestLog[]>(path);
}

export async function apiFetchRuntimeLogs(offset: number = 0, limit: number = 100): Promise<RuntimeLogsResponse> {
  return apiFetch<RuntimeLogsResponse>(`/admin/api/runtime-logs?offset=${offset}&limit=${limit}`);
}

export async function apiFetchToolRuntimeLogs(limit: number = 100): Promise<ToolRuntimeLog[]> {
  return apiFetch<ToolRuntimeLog[]>(`/admin/api/tool-runtime-logs?limit=${limit}`);
}

// ── Status & Stats ────────────────────────────────────────────────────────

export async function apiFetchStatus(): Promise<AdminStatusResponse> {
  return apiFetch<AdminStatusResponse>('/admin/api/status');
}

export async function apiFetchStats(): Promise<StatsSnapshot> {
  return apiFetch<StatsSnapshot>('/admin/api/stats');
}

export async function apiFetchModels(): Promise<ModelListResponse> {
  return apiFetch<ModelListResponse>('/admin/api/models');
}

export async function apiFetchCategorizedModels(): Promise<CategorizedModelsResponse> {
  return apiFetch<CategorizedModelsResponse>('/admin/api/models/categorized');
}

export async function apiFetchModelsOpenAI(): Promise<ModelListResponse> {
  return apiFetch<ModelListResponse>('/v1/models');
}
