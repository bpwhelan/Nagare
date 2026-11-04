import test from 'node:test';
import assert from 'node:assert/strict';
import { mediaFilterMode, mediaFilterSelection, updateMediaFilter } from './mediaFilters.js';

const empty = () => ({ included_libraries: [], excluded_libraries: [], included_folders: [], excluded_folders: [] });

test('switching between whitelist and blacklist preserves checks and clears the opposite lists', () => {
  const whitelist = updateMediaFilter(empty(), 'include', { libraries: ['jp'], folders: ['/Japanese'] });
  const blacklist = updateMediaFilter(whitelist, 'exclude');
  assert.deepEqual(blacklist, { mode: 'exclude', included_libraries: [], excluded_libraries: ['jp'], included_folders: [], excluded_folders: ['/Japanese'] });
  assert.deepEqual(updateMediaFilter(blacklist, 'include'), whitelist);
});

test('an empty mode survives saving and reloading', () => {
  const whitelist = JSON.parse(JSON.stringify(updateMediaFilter(empty(), 'include')));
  assert.equal(mediaFilterMode(whitelist), 'include');
  const blacklist = JSON.parse(JSON.stringify(updateMediaFilter(whitelist, 'exclude')));
  assert.equal(mediaFilterMode(blacklist), 'exclude');
  assert.deepEqual(mediaFilterSelection(blacklist), { libraries: [], folders: [] });
});

test('existing include-only and exclude-only rules select the matching mode', () => {
  assert.equal(mediaFilterMode({ ...empty(), included_libraries: ['jp'] }), 'include');
  assert.equal(mediaFilterMode({ ...empty(), excluded_libraries: ['en'] }), 'exclude');
  assert.equal(mediaFilterMode({ ...empty(), included_folders: ['/Japanese'] }), 'include');
  assert.equal(mediaFilterMode(empty()), 'exclude');
});
