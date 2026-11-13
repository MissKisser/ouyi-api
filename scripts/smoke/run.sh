#!/usr/bin/env bash
# web-translate → ouyi-api → Ouyi 翻译链路冒烟测试
# 跑前：确认反代已在另一个终端启动
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
OUT_DIR="$(dirname "$0")/out"
mkdir -p "$OUT_DIR"

API_KEY="${API_KEY:-sk-smoke-test-placeholder}"
# 注：API_KEY 字段反代不直接用（它走的是 jwt 池）；
# 这里只是为了模仿 OpenAI 协议的 Authorization 头，让客户端兼容层不报 401 解析错误。

# ----------- 颜色 -----------
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

section() { printf "\n${YELLOW}== %s ==${NC}\n" "$1"; }
ok()      { printf "${GREEN}✓ %s${NC}\n" "$1"; }
fail()    { printf "${RED}✗ %s${NC}\n" "$1"; exit 1; }

# 计时
timed() {
  local out="$1"; shift
  local start_ns end_ns
  start_ns=$(date +%s%N)
  "$@" > "$out" 2>&1 || true
  end_ns=$(date +%s%N)
  echo $(( (end_ns - start_ns) / 1000000 ))
}

# ----------- STEP 1: GET /v1/models -----------
step_models() {
  section "STEP 1: GET ${BASE_URL}/v1/models"
  local ms
  ms=$(timed "$OUT_DIR/01-models.json" curl -sS -w '\nHTTP_CODE:%{http_code}\n' \
        -H "Authorization: Bearer ${API_KEY}" \
        "${BASE_URL}/v1/models")
  echo "延迟: ${ms} ms"
  cat "$OUT_DIR/01-models.json"
  echo
  grep -q '"data"' "$OUT_DIR/01-models.json" \
    && ok "models 端点返回正常" \
    || fail "models 端点异常，看 ${OUT_DIR}/01-models.json"
}

# ----------- STEP 2: 用户挑模型 -----------
step_pick_model() {
  section "STEP 2: 选择模型 ID（不带 -thinking 后缀）"
  # 解析 id 列表
  local ids
  ids=$(python -c "import json,sys; d=json.load(open('$OUT_DIR/01-models.json')); print('\n'.join(m['id'] for m in d.get('data', [])))" 2>/dev/null || true)
  if [ -z "$ids" ]; then
    fail "未解析出模型 ID。请检查 $OUT_DIR/01-models.json。"
  fi
  echo "可用模型："
  echo "$ids"
  echo
  printf "挑一个模型 ID（直接粘贴进去）> "
  read -r MODEL_ID
  echo "$MODEL_ID" > "$OUT_DIR/02-model-id.txt"
  ok "已选择模型: $MODEL_ID"
}

# ----------- STEP 3: 最小聊天（鉴权 + 协议） -----------
step_chat_min() {
  section "STEP 3: 最小聊天完成请求（非流式，验证鉴权）"
  local body
  body=$(cat <<JSON
{
  "model": "$(cat "$OUT_DIR/02-model-id.txt")",
  "messages": [{"role": "user", "content": "ping"}],
  "stream": false,
  "max_tokens": 32
}
JSON
)
  local ms
  ms=$(timed "$OUT_DIR/03-chat-min.json" curl -sS -w '\nHTTP_CODE:%{http_code}\n' \
        -H "Authorization: Bearer ${API_KEY}" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "${BASE_URL}/v1/chat/completions")
  echo "延迟: ${ms} ms"
  tail -5 "$OUT_DIR/03-chat-min.json"
  echo
  grep -q '"choices"' "$OUT_DIR/03-chat-min.json" \
    && ok "chat 协议正常" \
    || fail "chat 协议异常，看 $OUT_DIR/03-chat-min.json"
}

# ----------- STEP 4: 翻译请求（不带 tools，对照 web-translate prompt） -----------
step_translate_no_tools() {
  section "STEP 4: 翻译（不带 tools，照搬 web-translate prompt.ts）"
  local model body
  model=$(cat "$OUT_DIR/02-model-id.txt")
  # 复刻 prompt.ts:14-22 + OpenAI adapter:39-46
  # 注意：故意不传 response_format，验证"反代丢弃它后，上游是否仍按 JSON 输出"
  body=$(cat <<JSON
{
  "model": "${model}",
  "stream": false,
  "messages": [
    {"role": "system", "content": "You are a professional translator. Translate from zh-CN to en-US. Rules: 1. Preserve URLs, code, numbers, and proper nouns unchanged. 2. Maintain the tone and register of the source text. 3. Return ONLY valid JSON: {\"translations\": [\"...\", \"...\"]} with array length matching input. 4. Each translation corresponds to the input text at the same 1-based index."},
    {"role": "user", "content": "[Texts to translate]\n\n[1] 这是一个测试句子，用于验证翻译质量。\n\n[2] OpenAI 推出 GPT-4o 后，API 价格大幅下降。\n\n[3] The user's quick API key was leaked — see https://example.com/keys"}
  ]
}
JSON
)
  local ms
  ms=$(timed "$OUT_DIR/04-translate-no-tools.json" curl -sS -w '\nHTTP_CODE:%{http_code}\n' \
        -H "Authorization: Bearer ${API_KEY}" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "${BASE_URL}/v1/chat/completions")
  echo "延迟: ${ms} ms"
  echo "--- 响应内容 ---"
  python -c "
import json
d = json.load(open('$OUT_DIR/04-translate-no-tools.json'))
content = d['choices'][0]['message']['content']
print(content)
print('---')
try:
    obj = json.loads(content)
    if isinstance(obj, dict) and 'translations' in obj:
        print(f'解析成功: {len(obj[\"translations\"])} 条翻译')
        for i, t in enumerate(obj['translations'], 1):
            print(f'  [{i}] {t}')
except Exception as e:
    print(f'解析失败: {e}')
" 2>&1 || cat "$OUT_DIR/04-translate-no-tools.json"
  ok "STEP 4 完成"
}

# ----------- STEP 5: 翻译请求（带空 tools，对照是否触发注入） -----------
step_translate_empty_tools() {
  section "STEP 5: 翻译（带空的 tools: []，验证反代是否注入提示词）"
  local model body
  model=$(cat "$OUT_DIR/02-model-id.txt")
  body=$(cat <<JSON
{
  "model": "${model}",
  "stream": false,
  "tools": [],
  "messages": [
    {"role": "system", "content": "You are a professional translator. Translate from zh-CN to en-US. Rules: 1. Preserve URLs, code, numbers, and proper nouns unchanged. 2. Maintain the tone and register of the source text. 3. Return ONLY valid JSON: {\"translations\": [\"...\", \"...\"]} with array length matching input. 4. Each translation corresponds to the input text at the same 1-based index."},
    {"role": "user", "content": "[Texts to translate]\n\n[1] 这是一个测试句子，用于验证翻译质量。\n\n[2] OpenAI 推出 GPT-4o 后，API 价格大幅下降。\n\n[3] The user's quick API key was leaked — see https://example.com/keys"}
  ]
}
JSON
)
  local ms
  ms=$(timed "$OUT_DIR/05-translate-empty-tools.json" curl -sS -w '\nHTTP_CODE:%{http_code}\n' \
        -H "Authorization: Bearer ${API_KEY}" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "${BASE_URL}/v1/chat/completions")
  echo "延迟: ${ms} ms"
  echo "响应字节数: $(wc -c < "$OUT_DIR/05-translate-empty-tools.json")"
  echo "对比 STEP 4（不带 tools）字节数: $(wc -c < "$OUT_DIR/04-translate-no-tools.json")"
  ok "STEP 5 完成"
}

# ----------- 主入口 -----------
case "${1:-all}" in
  models)       step_models ;;
  pick)         step_pick_model ;;
  chat)         step_chat_min ;;
  translate)    step_translate_no_tools && step_translate_empty_tools ;;
  all)
    step_models
    step_pick_model
    step_chat_min
    step_translate_no_tools
    step_translate_empty_tools
    section "全部完成"
    echo "结果在: $OUT_DIR"
    echo "对比 04 vs 05 的响应字节数，看反代是否对空 tools 字段做了处理。"
    ;;
  *) echo "用法: bash run.sh {models|pick|chat|translate|all}"; exit 2 ;;
esac

# ── STEP 鉴权：未启用时应放行（启用场景由管理端热更新后人工复验）──
echo "=== AUTH-1: 未配置 [api_keys] 时无凭据访问 /v1/models，期望 200 ==="
curl -sS -o /dev/null -w '%{http_code}\n' "$BASE_URL/v1/models"
