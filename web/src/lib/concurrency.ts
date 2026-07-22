export const MAX_CONCURRENT_REQUESTS = 1000;

export function parseMaxConcurrentRequestsInput(value: string): number {
  return value.trim() === '' ? Number.NaN : Number(value);
}

export function isValidMaxConcurrentRequests(value: number): boolean {
  return Number.isInteger(value) && value >= 0 && value <= MAX_CONCURRENT_REQUESTS;
}
