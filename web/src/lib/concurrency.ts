/** 并发上限输入允许的最大值。 */
export const MAX_CONCURRENT_REQUESTS = 1000;

/**
 * 解析并发上限输入框的原始字符串。
 * @param value 输入框文本
 * @returns 解析出的数值；空白输入返回 NaN（表示待填写而非 0）
 */
export function parseMaxConcurrentRequestsInput(value: string): number {
  return value.trim() === '' ? Number.NaN : Number(value);
}

/**
 * 校验并发上限是否合法：必须为 0 到 MAX_CONCURRENT_REQUESTS 之间的整数（0 表示不限制）。
 * @param value 待校验的数值
 */
export function isValidMaxConcurrentRequests(value: number): boolean {
  return Number.isInteger(value) && value >= 0 && value <= MAX_CONCURRENT_REQUESTS;
}
