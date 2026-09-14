import assert from 'node:assert/strict';
import { test } from 'node:test';
import { contextSentence, makeReviewDraft, validRange } from './review.js';

test('expanding context preserves highlighted Japanese and escapes new text', () => {
  const lines = [{text: '「前の文」'}, {text: '私の言葉。'}, {text: '<次の文>'}];
  assert.equal(contextSentence(lines, 0, 2, '私の<b>言葉</b>。'), '「前の文」私の<b>言葉</b>。&lt;次の文&gt;');
  assert.equal(contextSentence(lines, 0, 1, '私の<b>言葉</b>'), '「前の文」私の<b>言葉</b>。');
});

test('saved timing and inclusion survive later config changes', () => {
  const draft = makeReviewDraft({dialog: {event: {sentence: '<b>文</b>'}, matched_line_index: 1,
    included_line_first: 0, included_line_last: 2, start_ms: 3250, end_ms: 8900, generate_avif: false}}, [],
    {audio_start_offset_ms: 900, audio_end_offset_ms: 800});
  assert.deepEqual([draft.first, draft.last, draft.start, draft.end, draft.generateAvif], [0,2,3.25,8.9,false]);
});

test('invalid preview/save ranges are rejected', () => {
  assert(validRange(0, 1.25));
  for (const range of [[1,1],[2,1],[-1,2],['',3],[0,Infinity],[NaN,4]]) assert(!validRange(...range));
});
