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

test('saves only the concurrency limit and verifies the persisted value', async () => {
  const apiSource = await readFile(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
  const pageSource = await readFile(new URL('../src/pages/ConfigPage.tsx', import.meta.url), 'utf8');

  assert.match(
    apiSource,
    /apiSaveConfig\(\{ limits: \{ max_concurrent_requests: value \} \}\)/,
  );
  assert.match(apiSource, /const fresh = await apiFetchConfig\(\)/);
  assert.match(
    apiSource,
    /fresh\.limits\.max_concurrent_requests !== value/,
  );
  assert.match(pageSource, /const \[persistedConcurrency, setPersistedConcurrency\]/);
  assert.match(pageSource, /const \[savingConcurrency, setSavingConcurrency\]/);
  assert.match(pageSource, /\{ \.\.\.prev, limits: fresh\.limits \}/);
  assert.match(
    pageSource,
    /disabled=\{!valid \|\| !concurrencyDirty \|\| savingConcurrency \|\| saving\}/,
  );
  assert.match(
    pageSource,
    /disabled=\{saving \|\| savingConcurrency \|\| !valid\}/,
  );
});
