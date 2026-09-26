import assert from 'node:assert/strict';
import test from 'node:test';
import { needsConfirmation, shouldAutoEnhance } from './enhancementPolicy.js';

test('configured tags auto-confirm new cards without enabling global auto-approval', () => {
  const tagged = { source: 'pending', skip_confirmation: true };
  const ordinary = { source: 'pending', skip_confirmation: false };
  assert.equal(shouldAutoEnhance(tagged, false), true);
  assert.equal(needsConfirmation(tagged, false), false);
  assert.equal(shouldAutoEnhance(ordinary, false), false);
  assert.equal(needsConfirmation(ordinary, false), true);
  assert.equal(shouldAutoEnhance(ordinary, true), true);
});

test('a failed enhancement always requires deliberate retry', () => {
  const retry = { source: 'retry', skip_confirmation: true };
  assert.equal(shouldAutoEnhance(retry, false), false);
  assert.equal(shouldAutoEnhance(retry, true), false);
  assert.equal(needsConfirmation(retry, true), true);
});
