export const MAX_CONCURRENT_REQUESTS = 1000;

export function isValidMaxConcurrentRequests(value: number): boolean {
  return Number.isInteger(value) && value >= 0 && value <= MAX_CONCURRENT_REQUESTS;
}
