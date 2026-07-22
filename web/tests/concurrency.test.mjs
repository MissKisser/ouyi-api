import assert from 'node:assert/strict';
import test from 'node:test';

import {
  MAX_CONCURRENT_REQUESTS,
  isValidMaxConcurrentRequests,
} from '../src/lib/concurrency.ts';

test('accepts integer concurrency limits from zero through the maximum', () => {
  assert.equal(MAX_CONCURRENT_REQUESTS, 1000);

  for (const value of [0, 1, MAX_CONCURRENT_REQUESTS]) {
    assert.equal(isValidMaxConcurrentRequests(value), true);
  }
});

test('rejects concurrency limits outside the supported integer range', () => {
  for (const value of [-1, 1.5, MAX_CONCURRENT_REQUESTS + 1, Number.NaN]) {
    assert.equal(isValidMaxConcurrentRequests(value), false);
  }
});
