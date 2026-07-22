import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import {
  MAX_CONCURRENT_REQUESTS,
  isValidMaxConcurrentRequests,
  parseMaxConcurrentRequestsInput,
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

test('keeps an empty concurrency input invalid', () => {
  const value = parseMaxConcurrentRequestsInput('');

  assert.equal(Number.isNaN(value), true);
  assert.equal(isValidMaxConcurrentRequests(value), false);
});

test('parses a numeric concurrency input', () => {
  assert.equal(parseMaxConcurrentRequestsInput('3'), 3);
});

test('shows concurrency settings in an expanded dedicated config section', async () => {
  const source = await readFile(new URL('../src/pages/ConfigPage.tsx', import.meta.url), 'utf8');

  assert.match(source, /<Section title=\{t\('config\.sections\.limits'\)\}[^>]*defaultOpen/);
  assert.match(source, /t\('config\.server\.maxConcurrentRequests'\)/);
});
