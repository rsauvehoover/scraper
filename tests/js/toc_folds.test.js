// Logic tests for src/web/toc_folds.js, run by tests/js_logic.rs.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const F = require('../../src/web/toc_folds.js');

// The page's default: only the newest volume open.
const ids = ['1', '2', '3'];
const defaults = { 1: false, 2: false, 3: true };

test('with nothing remembered, the page default applies', () => {
  assert.deepEqual(F.openStates(ids, null, defaults), { 1: false, 2: false, 3: true });
});

test('a remembered choice wins over the default', () => {
  const saved = { 1: true, 2: false, 3: false };
  assert.deepEqual(F.openStates(ids, saved, defaults), saved);
});

test('a volume added since the last visit takes the default', () => {
  // Volumes 1-3 were remembered; volume 4 is new and is now the newest.
  const saved = { 1: false, 2: false, 3: false };
  const now = { 1: false, 2: false, 3: false, 4: true };
  assert.deepEqual(F.openStates(['1', '2', '3', '4'], saved, now), { 1: false, 2: false, 3: false, 4: true });
});

test('a remembered volume that no longer exists is ignored', () => {
  assert.deepEqual(F.openStates(['1'], { 1: true, 9: true }, { 1: false }), { 1: true });
});

test('stored values that are not an object of booleans count as nothing remembered', () => {
  for (const raw of [null, '', 'not json', '[true, false]', '"open"', '{"1": "yes"}', '{"1": 1}']) {
    assert.equal(F.parseSaved(raw), null, String(raw));
  }
  assert.deepEqual(F.parseSaved('{"1": true, "2": false}'), { 1: true, 2: false });
});

test('the button collapses while anything is open, and expands once nothing is', () => {
  assert.equal(F.toggleAction({ 1: false, 2: true }), 'collapse');
  assert.equal(F.toggleAction({ 1: true, 2: true }), 'collapse');
  assert.equal(F.toggleAction({ 1: false, 2: false }), 'expand');
  assert.equal(F.toggleAction({}), 'expand');
});
