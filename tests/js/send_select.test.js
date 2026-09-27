// Logic tests for src/web/send_select.js, run by tests/js_logic.rs.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const S = require('../../src/web/send_select.js');

test('the bar names what is selected', () => {
  assert.equal(S.selectionLabel(0, 0), '');
  assert.equal(S.selectionLabel(1, 0), '1 volume selected');
  assert.equal(S.selectionLabel(2, 3), '2 volumes, 3 chapters selected');
  assert.equal(S.selectionLabel(0, 1), '1 chapter selected');
});

test('the total counts items times destinations and stops at the cap', () => {
  assert.deepEqual(S.emailTotal(3, 0, 50), { text: 'Tick at least one destination.', over: false, disabled: true });
  assert.deepEqual(S.emailTotal(3, 1, 50), { text: '3 emails to 1 destination', over: false, disabled: false });
  assert.deepEqual(S.emailTotal(1, 1, 50), { text: '1 email to 1 destination', over: false, disabled: false });
  assert.deepEqual(S.emailTotal(25, 2, 50), { text: '50 emails to 2 destinations', over: false, disabled: false });
  assert.deepEqual(S.emailTotal(26, 2, 50), {
    text: '52 emails to 2 destinations, more than 50. Send fewer at a time.', over: true, disabled: true
  });
});
