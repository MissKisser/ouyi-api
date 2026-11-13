/**
 * Localize provider (vendor) names for the UI. We only translate the group
 * header / provider chip — individual model labels stay in their original
 * form (which is the canonical id-style name users expect to search/copy).
 *
 * - Lookup is case-insensitive and strips a trailing `_chat` suffix, so
 *   `Baidu_chat`, `baidu`, and `BAIDU` resolve to the same alias.
 * - In zh mode, any provider not in the alias table is returned with the
 *   trailing `_chat` stripped (`Mistralai_chat` -> `Mistralai`).
 * - In en (or non-zh) mode, the original name is returned unchanged.
 *
 * Sort order is exposed as `PROVIDER_RANK` so callers can pin a small set of
 * well-known providers in front while leaving the rest in their natural
 * (alphabetical / upstream) order.
 */
const PROVIDER_ALIASES: Readonly<Record<string, string>> = {
  baichuan: '百川智能',
  baidu: '文心一言',
  doubao: '豆包',
  jiejue: '阶跃星辰',
  lingyi: '零一万物',
  moonshot: 'Kimi',
  ouyi: '欧亿',
  glm: '智谱清言',
  zhipu: '智谱清言',
  xiaomi: 'Mimo',
  tongyi: '通义千问',
  nvidia: '英伟达',
};

/**
 * Pinned display order for known providers. Keys are the upstream names as
 * they arrive from the API. Anything not in this map falls to the end in
 * alphabetical order. The order is:
 *   Claude, OpenAI, Gemini, DeepSeek, 智谱清言(Zhipu), Kimi(Moonshot),
 *   欧亿(Ouyi), Mimo(Xiaomi), X(xAI), 通义千问(Tongyi), MiniMax
 */
const PROVIDER_RANK: Readonly<Record<string, number>> = {
  Claude: 0,
  OpenAI: 1,
  Gemini: 2,
  DeepSeek: 3,
  Zhipu: 4,
  Moonshot: 5,
  Ouyi_chat: 6,
  Xiaomi_chat: 7,
  X_chat: 8,
  Tongyi_chat: 9,
  MiniMax: 10,
};

/** 规整供应商标识：转小写并去掉结尾的 _chat 后缀，作为别名表查找键。 */
function providerKey(providerName: string): string {
  return providerName.toLowerCase().replace(/_chat$/, '');
}

/**
 * 把上游供应商名称本地化为界面展示名（仅本地化分组标题/供应商 chip，
 * 单个模型名保持原始 id 形式以便搜索复制）。
 * 查找不区分大小写并忽略结尾 _chat 后缀；中文环境下不在别名表中的供应商
 * 返回去掉 _chat 后缀的原始名；非中文环境原样返回。
 * @param providerName 上游供应商名称（如 Baidu_chat）
 * @param language 当前界面语言代码
 * @returns 用于展示的供应商名称
 */
export function localizeProviderName(providerName: string, language: string): string {
  if (!providerName) return providerName;
  if (!isZhLanguage(language)) return providerName;
  const key = providerKey(providerName);
  return PROVIDER_ALIASES[key] ?? providerName.replace(/_chat$/, '');
}

/**
 * 供应商排序比较器：知名供应商按 PROVIDER_RANK 固定顺序排在前面，
 * 未收录的供应商按名称字典序排在末尾。
 * @param a 参与比较的供应商标识
 * @param b 参与比较的供应商标识
 * @returns 负数表示 a 排在 b 前，正数反之，0 表示相等
 */
export function compareProviders(a: string, b: string): number {
  const rankA = PROVIDER_RANK[a];
  const rankB = PROVIDER_RANK[b];
  if (rankA !== undefined && rankB !== undefined) return rankA - rankB;
  if (rankA !== undefined) return -1;
  if (rankB !== undefined) return 1;
  return a.localeCompare(b);
}

function isZhLanguage(language: string): boolean {
  if (!language) return false;
  const lower = language.toLowerCase();
  return lower === 'zh' || lower.startsWith('zh-');
}