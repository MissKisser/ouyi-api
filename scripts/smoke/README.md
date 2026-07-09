# 反代翻译冒烟测试

目标：用最小的代价验证 **web-translate → ouyi-api → Ouyi 7/8** 这条链路能否承担网页/字幕翻译。

## 前提

- 反代已经按 `AGENTS.md` 的命令在另一个终端启动，能访问 `http://127.0.0.1:3000/v1/models`
- 你确认过 `config.toml` 的 jwt_token 未过期（曾用一段时间内有效即可）
- 操作系统：Windows + Git Bash；Linux/macOS 直接执行即可

## 用法

```bash
cd scripts/smoke
bash run.sh                # 自动跑所有 5 步
# 或
bash run.sh models         # 只跑第 1 步
bash run.sh chat           # 只跑 2~5 步
```

`run.sh` 会：
1. 调 `GET /v1/models` 列出可用模型
2. 让你从列表里挑一个非 `-thinking` 的模型 ID（粘贴进入）
3. 发 1 个最小聊天完成请求，验证鉴权 + 流式/非流式
4. 发 3 段中→英批量翻译请求（**不带 tools 字段**），模拟 `web-translate` 的 `OpenAIAdapter`
5. 发同样的请求，但带一个空 `tools: []` 字段，对比是否触发了工具提示词注入

所有响应落到 `out/<step>.json` 方便你 review。

## 重要约束

- **不要把 `out/*.json` 提交到 git**（见 `.gitignore` 节）
- 反代用 `config.toml` 启动，**不要修改它**——AGENTS.md 已经说明
- 如果第 3 步失败、但第 2 步成功，说明端口/JWT/auth 有问题；如果第 2 步失败，说明反代本身没起来

## 能验证什么、不能验证什么

| 关注点 | 这个脚本能验证吗 |
|---|---|
| 反代鉴权 / 账号池是否工作 | ✅ |
| `/v1/chat/completions` 协议能否透传到上游 | ✅ |
| 不带 `tools` 时上游是否注入提示词 | ✅ 对比 4 / 5 步输出体积 |
| 上游模型译文质量 | ✅（人眼看输出） |
| `response_format` 被丢弃 | ❌（无法直接观察，要看上游是否仍输出 JSON） |
| `temperature` 被丢弃 | ❌（无法直接观察） |
| `system role` 失效 | ⚠️ 部分（看响应是否保留"稳重语气"） |
| 长文稳定性 | ⚠️ 只覆盖 3 段，长文要单独测 |
