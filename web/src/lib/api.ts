const TOKEN_KEY = 'ds-admin-token';

/** 从 localStorage 读取登录 token，未登录时返回 null。副作用：仅读取。 */
export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

/** 把登录 token 持久化写入 localStorage（副作用：写存储）。 */
export function setToken(token: string) {
  localStorage.setItem(TOKEN_KEY, token);
}
/** 从 localStorage 移除登录 token（副作用：删存储），用于登出或收到 401 时。 */
export function clearToken() {
  localStorage.removeItem(TOKEN_KEY);
}

let onUnauthorized: (() => void) | null = null;

/** 注册 401 回调：收到 401 时自动调用（用于 AuthProvider 同步 token 状态），传 null 取消注册。 */
export function setOnUnauthorized(cb: (() => void) | null) {
  onUnauthorized = cb;
}

/**
 * 统一 API 请求函数：自动附加 Accept / Content-Type 头与 Bearer token，解析 JSON 响应。
 * @param path 请求路径（以 / 开头的相对路径，如 /admin/api/status）
 * @param init 额外的 fetch 配置（method / body / signal 等）
 * @returns 解析后的响应 JSON
 * @throws {AuthError} 响应状态为 401：先清空本地 token 并触发 onUnauthorized 回调，再抛出
 * @throws {ApiError} 其他非 2xx 状态：携带状态码与后端 error 字段信息
 */
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

/** 鉴权失败错误：apiFetch 收到 HTTP 401 时抛出，name 固定为 AuthError。 */
export class AuthError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'AuthError';
  }
}

/** API 业务错误：非 401 的失败响应抛出，name 固定为 ApiError，status 为 HTTP 状态码。 */
export class ApiError extends Error {
  /** 本次失败的 HTTP 状态码。 */
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

// ── Auth API ──────────────────────────────────────────────────────────────

/** 登录 / 初始化成功后的响应体。 */
export interface LoginResponse {
  /** 后端签发的管理端 JWT，前端持久化到 localStorage 并在后续请求中作 Bearer 头。 */
  token: string;
}

/**
 * 首次初始化：设置管理密码并换取 token。
 * 调用端点：POST /admin/api/setup（密码未设置时使用）。
 * @throws {AuthError} 401；@throws {ApiError} 其他失败状态
 */
export async function apiSetup(password: string): Promise<LoginResponse> {
  return apiFetch<LoginResponse>('/admin/api/setup', {
    method: 'POST',
    body: JSON.stringify({ password }),
  });
}

/**
 * 管理端登录：校验密码并换取 token。
 * 调用端点：POST /admin/api/login。
 * @throws {AuthError} 401（密码错误或未初始化）；@throws {ApiError} 其他失败状态
 */
export async function apiLogin(password: string): Promise<LoginResponse> {
  return apiFetch<LoginResponse>('/admin/api/login', {
    method: 'POST',
    body: JSON.stringify({ password }),
  });
}

// ── Data Types ────────────────────────────────────────────────────────────

/** 单条上游请求日志（GET /admin/api/logs 返回的元素）。 */
export interface RequestLog {
  /** 请求发生时间，Unix 时间戳，单位秒。 */
  timestamp: number;
  /** 请求唯一 ID，用于日志追踪。 */
  request_id: string;
  /** 请求的目标模型 ID。 */
  model: string;
  /** 调用方使用的 API key（脱敏显示）。 */
  api_key: string;
  /** 输入消耗的 prompt token 数。 */
  prompt_tokens: number;
  /** 输出消耗的 completion token 数。 */
  completion_tokens: number;
  /** 端到端耗时，单位毫秒。 */
  latency_ms: number;
  /** 请求是否成功。 */
  success: boolean;
}

/** 单条服务运行日志（后端 tracing 输出）。 */
export interface RuntimeLogEntry {
  /** 日志时间，ISO 8601 字符串（含毫秒）。 */
  timestamp: string;
  /** 日志级别：ERROR / WARN / INFO / DEBUG / TRACE。 */
  level: string;
  /** 产生日志的模块路径（Rust target）。 */
  target: string;
  /** 日志正文。 */
  message: string;
}

/** 运行日志分页响应。 */
export interface RuntimeLogsResponse {
  /** 日志总条数，用于计算总页数。 */
  total: number;
  /** 本次返回的起始偏移量。 */
  offset: number;
  /** 本次返回的最大条数。 */
  limit: number;
  /** 当前页的日志条目。 */
  logs: RuntimeLogEntry[];
}

/** 单条工具模型（tool runtime）调用日志。 */
export interface ToolRuntimeLog {
  /** 调用时间，ISO 8601 字符串（含毫秒）。 */
  timestamp: string;
  /** 关联的上游请求 ID。 */
  request_id: string;
  /** 工具调用端点类型：openai 或 anthropic。 */
  endpoint: string;
  /** 工具运行模式（如 compiler）。 */
  mode: string;
  /** 工具模型供应商名称。 */
  provider: string;
  /** 工具模型 ID。 */
  model: string;
  /** 工具模型的 API 基础地址。 */
  base_url: string;
  /** 路由决策结果，如 tool_calls 或 no_tool。 */
  decision: string;
  /** 本次识别出的工具名称列表。 */
  tool_names: string[];
  /** 输入消耗的 prompt token 数。 */
  prompt_tokens: number;
  /** 输出消耗的 completion token 数。 */
  completion_tokens: number;
  /** 工具调用耗时，单位毫秒。 */
  latency_ms: number;
  /** 调用是否成功。 */
  success: boolean;
  /** 失败时的错误信息。 */
  error?: string;
}

/** 工具模型运行时配置（DeepSeek / OpenAI 两条链路共用同一结构）。 */
export interface ToolRuntimeConfig {
  /** 是否启用该工具链路。 */
  enabled: boolean;
  /** 协议类型：anthropic 或 openai-compatible。 */
  provider: 'anthropic' | 'openai-compatible';
  /** 工具模型的 API 基础地址。 */
  base_url: string;
  /** 工具模型的 API key。 */
  api_key: string;
  /** 工具模型 ID。 */
  model: string;
  /** 请求超时时间，单位秒。 */
  timeout_secs: number;
}

/** 工具链路连通性测试结果。 */
export interface ToolRuntimeTestResponse {
  /** 测试是否成功。 */
  success: boolean;
  /** 测试结果描述信息。 */
  message: string;
  /** 测试请求耗时，单位毫秒。 */
  latency_ms: number;
  /** 工具运行模式。 */
  mode: string;
  /** 实际使用的供应商名称。 */
  provider: string;
  /** 实际使用的模型 ID。 */
  model: string;
  /** 实际使用的 API 基础地址。 */
  base_url: string;
}

/** 上游账号池中单个账号的运行状态。 */
export interface AccountStatus {
  /** 账号邮箱。 */
  email: string;
  /** 账号手机号。 */
  mobile: string;
  /** 账号状态：idle / busy / error / invalid 等。 */
  state: string;
  /** 上次释放（归还可用量）距今的毫秒数。 */
  last_released_ms: number;
  /** 累计错误次数。 */
  error_count: number;
}

/** 上游平台标识：ouyi7（chatapi 形态）/ ouyi8（OpenAI 兼容形态）。 */
export type OuyiPlatform = 'ouyi7' | 'ouyi8';

/** 当前选中上游平台的运行状态。 */
export interface PlatformStatus {
  /** 平台标识。 */
  platform: OuyiPlatform;
  /** 平台展示名称。 */
  label: string;
  /** 平台 API 基础地址。 */
  base_url: string;
  /** 该平台可用的模型数量。 */
  model_count: number;
  /** 平台 JWT token 是否有效。 */
  token_valid: boolean;
  /** 最近一次选中该平台的时间，RFC 3339 字符串；未记录时缺省。 */
  last_selected_at?: string;
}

/** 管理端总览状态（GET /admin/api/status 响应）。 */
export interface AdminStatusResponse {
  /** 账号池内每个账号的状态明细。 */
  accounts: AccountStatus[];
  /** 账号总数。 */
  total: number;
  /** 空闲账号数。 */
  idle: number;
  /** 占用中账号数。 */
  busy: number;
  /** 错误状态账号数。 */
  error: number;
  /** 失效账号数。 */
  invalid: number;
  /** 当前平台状态。 */
  platform: PlatformStatus;
}

/** 服务运行统计快照（GET /admin/api/stats 响应）。 */
export interface StatsSnapshot {
  /** 总请求数。 */
  total_requests: number;
  /** 成功请求数。 */
  success_requests: number;
  /** 失败请求数。 */
  failed_requests: number;
  /** 平均端到端耗时，单位毫秒。 */
  avg_latency_ms: number;
  /** 累计 prompt token 消耗。 */
  total_prompt_tokens: number;
  /** 累计 completion token 消耗。 */
  total_completion_tokens: number;
  /** 服务已运行时长，单位秒。 */
  uptime_secs: number;
  /** 按模型聚合的用量：模型 ID → prompt/completion token 数与请求次数。 */
  models: Record<string, { prompt_tokens: number; completion_tokens: number; requests: number }>;
  /** 按 API key 聚合的用量：key → prompt/completion token 数与请求次数。 */
  keys: Record<string, { prompt_tokens: number; completion_tokens: number; requests: number }>;
}

/** 单个模型信息（OpenAI /v1/models 条目结构）。 */
export interface ModelInfo {
  /** 模型 ID，调用对话接口时使用的 model 值。 */
  id: string;
  /** 对象类型，固定为 "model"。 */
  object: string;
  /** 模型创建时间，Unix 时间戳，单位秒。 */
  created: number;
  /** 模型归属方（供应商标识）。 */
  owned_by: string;
}

/** 模型列表响应（OpenAI 风格）。 */
export interface ModelListResponse {
  /** 对象类型，固定为 "list"。 */
  object: string;
  /** 模型数组。 */
  data: ModelInfo[];
}

/** 分类模型列表中的单个模型项。 */
export interface CategorizedModelItem {
  /** 模型展示名称。 */
  label: string;
  /** 模型 ID（对话接口使用的 value）。 */
  value: string;
  /** 所属供应商的 key，用于分组筛选。 */
  provider_key: string;
}

/** 按供应商分组的模型集合。 */
export interface CategorizedModelGroup {
  /** 供应商展示名称（可经 localizeProviderName 本地化）。 */
  provider_name: string;
  /** 供应商 key，作为分组唯一标识。 */
  provider_key: string;
  /** 该供应商下的模型总数。 */
  count: number;
  /** 该供应商下的模型列表。 */
  models: CategorizedModelItem[];
}

/** 带展示名的扩展模型信息。 */
export interface ModelInfoExtended extends ModelInfo {
  /** 优先展示的名称；缺省时回退为 id。 */
  display?: string;
}

// ── Config Types (mirrors backend response) ───────────────────────────────

/** 服务监听配置。 */
export interface ServerConfig {
  /** 监听主机地址。 */
  host: string;
  /** 监听端口。 */
  port: number;
  /** 允许的 CORS 来源列表。 */
  cors_origins: string[];
}

/** 并发限制配置。 */
export interface LimitsConfig {
  /** 最大并发请求数，0 表示不限制；保存后热生效。 */
  max_concurrent_requests: number;
}

/** 工具调用标签扩展配置。 */
export interface ToolCallTagConfig {
  /** 额外识别的工具调用开始标签。 */
  extra_starts: string[];
  /** 额外识别的工具调用结束标签。 */
  extra_ends: string[];
}

/** 上游 ouyi8 平台的接入配置。 */
export interface DeepSeekConfig {
  /** 上游 API 基础地址。 */
  api_base: string;
  /** WASM 资源地址。 */
  wasm_url: string;
  /** 上游请求使用的 User-Agent。 */
  user_agent: string;
  /** 伪装的客户端版本号。 */
  client_version: string;
  /** 伪装的客户端平台标识。 */
  client_platform: string;
  /** 伪装的客户端语言区域。 */
  client_locale: string;
  /** 上游鉴权 JWT token。 */
  jwt_token: string;
  /** 模型列表自动刷新间隔，单位秒。 */
  model_refresh_interval_secs: number;
  /** 支持的模型类型列表。 */
  model_types: string[];
  /** 各模型的输入 token 上限数组。 */
  max_input_tokens: number[];
  /** 各模型的输出 token 上限数组。 */
  max_output_tokens: number[];
  /** 模型别名映射列表。 */
  model_aliases: string[];
  /** 工具调用标签配置。 */
  tool_call: ToolCallTagConfig;
}

/** 出站代理配置。 */
export interface ProxyConfig {
  /** 代理地址；未配置时为 null。 */
  url: string | null;
}

/** 管理端账号配置状态。 */
export interface AdminConfigResponse {
  /** 管理密码是否已设置。 */
  password_set: boolean;
  /** 管理 JWT 签发时间，Unix 时间戳（秒）。 */
  jwt_issued_at: number;
}

/** 上游账号池中的单个账号条目。 */
export interface AccountEntry {
  /** 账号邮箱。 */
  email: string;
  /** 账号手机号。 */
  mobile: string;
  /** 手机号区号。 */
  area_code: string;
  /** 账号密码。 */
  password: string;
  /** 上游会话 token；可选。 */
  token?: string;
  /** 是否启用该账号。 */
  enabled: boolean;
}

/** 对外开放的 API key 条目。 */
export interface ApiKeyEntry {
  /** key 值，调用方以 Bearer 方式携带。 */
  key: string;
  /** key 的用途描述。 */
  description: string;
}

/** 分类模型列表响应（GET /admin/api/models/categorized）。 */
export interface CategorizedModelsResponse {
  /** 当前生效的平台。 */
  platform: OuyiPlatform;
  /** 按供应商分组的模型集合。 */
  categories: CategorizedModelGroup[];
}

/** 上游 ouyi7 平台（chatapi 形态）的接入配置。 */
export interface Ouyi7Config {
  /** 平台 API 基础地址。 */
  base_url: string;
  /** chatapi 路径前缀。 */
  chatapi_prefix: string;
  /** 平台鉴权 JWT token。 */
  jwt_token: string;
  /** 默认使用的模型 ID。 */
  default_model: string;
  /** 模型列表自动刷新间隔，单位秒。 */
  model_refresh_interval_secs: number;
  /** 对话历史保留的最大消息条数。 */
  max_history_messages: number;
  /** 模拟流式输出时每个分片的字符数。 */
  simulated_stream_chunk_chars: number;
}

/** 平台切换配置。 */
export interface PlatformConfig {
  /** 默认使用的上游平台。 */
  default: OuyiPlatform;
  /** 服务启动时是否提示选择平台。 */
  prompt_on_start: boolean;
}

/** 完整配置响应（GET /admin/api/config），结构与后端配置文件对应。 */
export interface FullConfig {
  /** 服务监听配置。 */
  server: ServerConfig;
  /** 并发限制配置。 */
  limits: LimitsConfig;
  /** 上游 ouyi8 平台配置。 */
  deepseek: DeepSeekConfig;
  /** 出站代理配置。 */
  proxy: ProxyConfig;
  /** 管理端账号状态。 */
  admin: AdminConfigResponse;
  /** 上游账号池列表。 */
  accounts: AccountEntry[];
  /** 对外 API key 列表。 */
  api_keys: ApiKeyEntry[];
  /** DeepSeek 工具链路配置。 */
  tool_runtime: ToolRuntimeConfig;
  /** OpenAI 工具链路配置。 */
  openai_tool_runtime: ToolRuntimeConfig;
  /** 平台切换配置；后端未写入该节时为 null。 */
  platform: PlatformConfig | null;
  /** ouyi7 平台配置；未启用时为 null。 */
  ouyi_v7: Ouyi7Config | null;
}

// ── Config API ────────────────────────────────────────────────────────────

/**
 * 拉取完整配置。
 * 调用端点：GET /admin/api/config。
 * @returns 解析后的 FullConfig
 * @throws {AuthError} 401；@throws {ApiError} 其他失败状态
 */
export async function apiFetchConfig(): Promise<FullConfig> {
  return apiFetch<FullConfig>('/admin/api/config');
}

/**
 * 保存配置（部分字段即可，后端做合并）。
 * 调用端点：PUT /admin/api/config。
 * @param config 要提交的配置片段（顶层键对应 FullConfig 的节）
 * @returns 含 ok 标志的响应体
 * @throws {AuthError} 401；@throws {ApiError} 保存失败
 */
export async function apiSaveConfig(config: Record<string, unknown>): Promise<{ ok: boolean }> {
  return apiFetch<{ ok: boolean }>('/admin/api/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}

/**
 * 单独保存并发上限并校验已落盘：先 PUT limits，再 GET 最新配置，
 * 若回读值与提交值不一致则视为未持久化。
 * 调用端点：PUT /admin/api/config + GET /admin/api/config。
 * @param value 新的最大并发数（0 表示不限制）
 * @returns 回读的最新 FullConfig
 * @throws {Error} 回读值不一致（未持久化）；@throws {ApiError} / {AuthError} 请求失败
 */
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
/**
 * 切换平台（即时生效，无需重启）。把 platform 配置作为 PUT body 提交，
 * 后端会重建上游客户端、切换运行时平台并触发模型列表重载；
 * 保存后立即回读最新配置并校验 default 已落盘。
 * 调用端点：PUT /admin/api/config + GET /admin/api/config。
 * @param platform 目标平台配置（含 default 与 prompt_on_start）
 * @returns 回读的最新 FullConfig
 * @throws {Error} 回读值不一致（未持久化）；@throws {ApiError} / {AuthError} 请求失败
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

/**
 * 测试 DeepSeek 工具链路连通性（后端按给定配置发起一次真实调用）。
 * 调用端点：POST /admin/api/tool-runtime/test。
 * @param config 待测试的工具链路配置
 * @returns 测试结果（成功标志、耗时、实际使用的供应商与模型）
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiTestToolRuntime(config: ToolRuntimeConfig): Promise<ToolRuntimeTestResponse> {
  return apiFetch<ToolRuntimeTestResponse>('/admin/api/tool-runtime/test', {
    method: 'POST',
    body: JSON.stringify(config),
  });
}

/**
 * 测试 OpenAI 兼容工具链路连通性（后端按给定配置发起一次真实调用）。
 * 调用端点：POST /admin/api/openai-tool-runtime/test。
 * @param config 待测试的工具链路配置
 * @returns 测试结果（成功标志、耗时、实际使用的供应商与模型）
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiTestOpenAiToolRuntime(config: ToolRuntimeConfig): Promise<ToolRuntimeTestResponse> {
  return apiFetch<ToolRuntimeTestResponse>('/admin/api/openai-tool-runtime/test', {
    method: 'POST',
    body: JSON.stringify(config),
  });
}

// ── Logs ──────────────────────────────────────────────────────────────────

/**
 * 拉取上游请求日志。
 * 调用端点：GET /admin/api/logs（可带 limit 查询参数）。
 * @param limit 返回条数上限；缺省时由后端决定
 * @returns 按时间排列的请求日志数组
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiFetchLogs(limit?: number): Promise<RequestLog[]> {
  const path = limit ? `/admin/api/logs?limit=${limit}` : '/admin/api/logs';
  return apiFetch<RequestLog[]>(path);
}

/**
 * 分页拉取服务运行日志。
 * 调用端点：GET /admin/api/runtime-logs?offset=&limit=。
 * @param offset 起始偏移量，默认 0
 * @param limit 每页条数，默认 100
 * @returns 含总条数与当前页日志的分页响应
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiFetchRuntimeLogs(offset: number = 0, limit: number = 100): Promise<RuntimeLogsResponse> {
  return apiFetch<RuntimeLogsResponse>(`/admin/api/runtime-logs?offset=${offset}&limit=${limit}`);
}

/**
 * 拉取工具模型调用日志。
 * 调用端点：GET /admin/api/tool-runtime-logs?limit=。
 * @param limit 返回条数上限，默认 100
 * @returns 最近的工具调用日志数组
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiFetchToolRuntimeLogs(limit: number = 100): Promise<ToolRuntimeLog[]> {
  return apiFetch<ToolRuntimeLog[]>(`/admin/api/tool-runtime-logs?limit=${limit}`);
}

// ── Status & Stats ────────────────────────────────────────────────────────

/**
 * 拉取管理端总览状态（账号池计数与当前平台状态）。
 * 调用端点：GET /admin/api/status。
 * @returns 账号池与平台状态
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiFetchStatus(): Promise<AdminStatusResponse> {
  return apiFetch<AdminStatusResponse>('/admin/api/status');
}

/**
 * 拉取服务运行统计快照（请求计数、token 用量、运行时长、分模型/分 key 聚合）。
 * 调用端点：GET /admin/api/stats。
 * @returns 统计快照
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiFetchStats(): Promise<StatsSnapshot> {
  return apiFetch<StatsSnapshot>('/admin/api/stats');
}

/**
 * 拉取管理端模型列表（OpenAI 风格扁平列表）。
 * 调用端点：GET /admin/api/models。
 * @returns 模型列表响应
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiFetchModels(): Promise<ModelListResponse> {
  return apiFetch<ModelListResponse>('/admin/api/models');
}

/**
 * 拉取按供应商分组的模型列表。
 * 调用端点：GET /admin/api/models/categorized。
 * @returns 平台标识与分组模型集合
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiFetchCategorizedModels(): Promise<CategorizedModelsResponse> {
  return apiFetch<CategorizedModelsResponse>('/admin/api/models/categorized');
}

/**
 * 调用对外开放的模型列表端点（与下游客户端同一入口）。
 * 调用端点：GET /v1/models。
 * @returns OpenAI 风格模型列表响应
 * @throws {ApiError} / {AuthError} 请求失败
 */
export async function apiFetchModelsOpenAI(): Promise<ModelListResponse> {
  return apiFetch<ModelListResponse>('/v1/models');
}
