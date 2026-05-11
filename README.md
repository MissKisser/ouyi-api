# ouyi-api

欧亿 AI 平台反向代理服务，提供 OpenAI 和 Anthropic 兼容的 API 接口，附 Web 管理面板。

## 特性

- **双协议支持** — 同时提供 OpenAI Responses API（`/openai/v1`，面向 Codex）和 Anthropic（`/anthropic`）接口
- **扩展思考** — 支持 Claude 系列模型的 thinking 模式，流式输出推理过程
- **工具调用** — Claude Code 工具请求可路由到原生 Anthropic tools 后端，普通对话继续走欧亿
- **Usage 统计** — Anthropic 响应包含 input/output token 用量，claude-hud 可正常显示
- **Web 管理面板** — 仪表盘、模型列表、聊天测试、配置管理、日志查看
- **多账号池** — 轮询分配上游 token，支持状态追踪和故障切换
- **配置热更新** — 修改上游 API 配置无需重启

## 快速开始

### 编译

```bash
cargo build --release
```

### 配置

编辑 `config.toml`，填入欧亿平台的 JWT token：

```toml
[server]
host = "127.0.0.1"
port = 3000

[ouyi]
base_url = "https://ai8.rcouyi.com"
jwt_token = "你的欧亿JWT"
app_version = "3.0.3"

[admin]
jwt_secret = "管理面板JWT密钥"
password_hash = "bcrypt哈希，首次启动自动生成"
```

也可以通过环境变量 `OUYI_CONFIG` 指定配置文件路径。

### 工具运行时

欧亿 Claude 模型未开放原生 tools 能力。需要稳定运行 Claude Code 的多轮工具链时，应配置原生 Anthropic 工具运行时；网关会将包含 `tools` 的 Anthropic Messages 请求转发到该运行时，普通无工具请求仍使用欧亿。

```toml
[tool_runtime]
mode = "native"
provider = "anthropic"
base_url = "https://api.anthropic.com"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-7"
timeout_secs = 120
```

`api_key_env` 只填写环境变量名，真实密钥通过运行环境注入。

### 运行

```bash
./target/release/ouyi-api
```

### 前端构建

```bash
cd web && npm install && npm run build
```

## API 接口

### OpenAI 兼容（Codex）

```bash
# 模型列表
curl http://127.0.0.1:3000/openai/v1/models

# 非流式对话（Responses API）
curl http://127.0.0.1:3000/openai/v1/responses \
  -H "Content-Type: application/json" \
  -d '{"model":"claude-sonnet-4-6","input":"你好"}'

# 带系统指令
curl http://127.0.0.1:3000/openai/v1/responses \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v3.2","instructions":"你是一个有用的助手","input":"介绍一下北京"}'
```

### Anthropic 兼容（Claude Code）

支持两种路径格式：`/anthropic/v1/messages` 和 `/anthropic/messages`。

```bash
# 模型列表
curl http://127.0.0.1:3000/anthropic/v1/models

# 非流式
curl http://127.0.0.1:3000/anthropic/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: any" \
  -d '{"model":"claude-sonnet-4-6","max_tokens":100,"messages":[{"role":"user","content":"Hello"}]}'

# 流式
curl http://127.0.0.1:3000/anthropic/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: any" \
  -d '{"model":"deepseek-v3.2","max_tokens":100,"messages":[{"role":"user","content":"Hello"}],"stream":true}'

# 扩展思考（thinking）模型
curl http://127.0.0.1:3000/anthropic/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: any" \
  -d '{"model":"claude-sonnet-4-6-thinking","max_tokens":200,"messages":[{"role":"user","content":"Hello"}],"stream":true}'
```

## Claude Code 集成

### 方式一：环境变量

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:3000/anthropic
export ANTHROPIC_API_KEY=proxy-no-auth-needed
export ANTHROPIC_DEFAULT_SONNET_MODEL=claude-sonnet-4-6
```

### 方式二：cc-switch 临时配置

使用 [cc-switch](https://github.com/anthropics/cc-switch) 可在不修改全局环境的情况下指定反代：

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:3000/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "proxy-no-auth-needed",
    "ANTHROPIC_MODEL": "deepseek-v4-pro"
  }
}
```

> API key 可填任意非空字符串，反代不做验证。

## OpenAI Codex 集成

在 `~/.codex/config.toml` 中添加自定义 provider：

```toml
model_provider = "ouyi"

[model_providers.ouyi]
name = "Ouyi Proxy"
base_url = "http://127.0.0.1:3000/openai/v1"
wire_api = "responses"
env_key = "OUYI_API_KEY"
```

设置环境变量（API key 可填任意非空字符串）：

```bash
export OUYI_API_KEY=proxy-no-auth-needed
```

## 管理面板

首次访问 `http://127.0.0.1:3000/admin` 会提示设置密码。登录后可管理：

- **概览** — 请求统计、成功率、平均延迟、Token 用量
- **模型列表** — 按提供商分组展示所有模型及其能力，支持一键复制 ID
- **聊天测试** — 网页端选择模型、调参、实时 SSE 流式对话
- **配置管理** — 修改上游 API 地址、JWT、模型刷新间隔
- **日志查看** — 请求日志和运行时日志分页查看
- **账号池** — 多账号管理，轮询分配 token

## 模型名称

欧亿平台使用 `provider_key::model_id` 格式。反代内置映射器自动将短名映射到全名：

| 短名 | 完整名称 |
|------|---------|
| `claude-sonnet-4-6` | `claude_chat::claude-sonnet-4-6` |
| `claude-opus-4-7-vip` | `claude_chat::claude-opus-4-7-vip` |
| `deepseek-v3.2` | `deepseek_chat::deepseek-v3.2` |
| `gpt-5.1` | `openai_chat::gpt-5.1` |
| `gemini-3-pro` | `gemini_chat::gemini-3-pro` |

短名查找支持前缀匹配（VIP 模型优先），`-thinking` 后缀模型自动启用推理模式。

## 技术栈

- **后端**：Rust / axum / tokio / reqwest
- **前端**：React 19 / TypeScript / Vite / Tailwind CSS / shadcn/ui / SWR / i18next
