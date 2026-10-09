// Logic tests for src/web/config_panel.js, run by tests/js_logic.rs.
// Fixtures are synthetic.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const P = require('../../src/web/config_panel.js');

function sample() {
  return {
    RequestDelay: 1000,
    EpubGen: { Chapters: true, Volumes: true, StripColour: false },
    Mail: {
      Name: 'Example Sender',
      Address: 'sender@example.com',
      PasswordSet: true,
      SmtpHostname: 'smtp.example.com',
      SmtpPort: 587,
      Destinations: [
        {
          Name: 'Test Reader',
          Email: 'reader@example.com',
          Sources: { 'example-source': { StripColour: true } }
        },
        { Name: 'Parked Reader', Email: 'parked@example.com', Sources: {} }
      ]
    },
    Sources: [
      {
        Id: 'example-source',
        Name: 'Example Serial',
        Enabled: true,
        TocUrl: 'https://example.com/toc/',
        Auth: { Type: 'None' },
        Metadata: { Author: 'A. Writer', Description: 'Test' },
        Selectors: { SelectorType: 'class', MainContent: 'content', IgnoredVolumes: ['Volume 1'] },
        PostProcessors: ['strip-links'],
        LegacyRetryCount: 3
      },
      // As minimal as a valid source gets: everything else has a default.
      { Id: 'minimal-source', Name: 'Minimal Serial', Enabled: true }
    ]
  };
}

test('an edit changes one value and nothing else', () => {
  const doc = sample();
  const expected = sample();
  expected.Sources[0].Metadata.Author = 'B. Writer';

  P.setPath(doc, ['Sources', 0, 'Metadata', 'Author'], 'B. Writer');

  // Includes the unmodelled Selectors.IgnoredVolumes, LegacyRetryCount and
  // RequestDelay: surviving is a property of patching, not of care.
  assert.deepEqual(doc, expected);
});

test('an edit to an absent section creates it and touches no sibling', () => {
  const doc = sample();
  P.setPath(doc, ['Sources', 1, 'Metadata', 'Author'], 'C. Writer');
  assert.deepEqual(doc.Sources[1], {
    Id: 'minimal-source', Name: 'Minimal Serial', Enabled: true,
    Metadata: { Author: 'C. Writer' }
  });
  assert.deepEqual(doc.Sources[0], sample().Sources[0]);
});

test('reading an absent path is undefined, not an error', () => {
  const doc = sample();
  assert.equal(P.getPath(doc, ['Sources', 1, 'Auth', 'Type']), undefined);
  assert.equal(P.getPath(doc, ['Sources', 9, 'Name']), undefined);
  assert.equal(P.getPath({}, ['Mail', 'Destinations', 0]), undefined);
});

test('half-typed JSON is not a document; the last one that parsed stays in use', () => {
  const good = JSON.stringify(sample(), null, 2);
  for (const partial of [good.slice(0, good.length / 2), good + ',', '{"Sources": [', '']) {
    assert.equal(P.parseDocument(partial).ok, false, partial.slice(-20));
  }
  assert.equal(P.parseDocument('[1, 2]').ok, false, 'an array is not a config');
  assert.equal(P.parseDocument('null').ok, false, 'null is not a config');
  assert.deepEqual(P.parseDocument(good).doc, sample());
});

test('other settings lists unmodelled keys, nested ones included, and no modelled key', () => {
  const s = sample().Sources[0];
  const other = P.otherSettings(s, P.MODEL.source, P.MODEL.sourceNested);
  assert.deepEqual(other, [
    { path: 'LegacyRetryCount', value: 3 },
    { path: 'Selectors.IgnoredVolumes', value: ['Volume 1'] }
  ]);
  assert.deepEqual(
    P.otherSettings(sample().Mail, P.MODEL.mail).map((o) => o.path),
    ['SmtpHostname', 'SmtpPort'],
    'PasswordSet and Destinations are handled by the panel, not listed'
  );
  assert.deepEqual(P.otherSettings(sample(), P.MODEL.topLevel), [{ path: 'RequestDelay', value: 1000 }]);
});

test('a destination is ticked for exactly the sources it lists', () => {
  const doc = sample();
  assert.deepEqual(
    P.destinationSourceRows(doc, 0).map((r) => [r.id, r.checked]),
    [['example-source', true], ['minimal-source', false]]
  );
  assert.deepEqual(
    P.destinationSourceRows(doc, 1).map((r) => r.checked),
    [false, false],
    'an empty map is sent nothing, so nothing is ticked'
  );
});

test('a listed id that is no longer a source still shows, so it can be unticked', () => {
  const doc = sample();
  doc.Mail.Destinations[0].Sources['removed-source'] = {};
  const stale = P.destinationSourceRows(doc, 0).filter((r) => r.stale);
  assert.deepEqual(stale.map((r) => [r.id, r.checked]), [['removed-source', true]]);
});

test('unticking and re-ticking a source keeps its overrides', () => {
  const doc = sample();
  const parked = {};
  P.toggleDestinationSource(doc, 0, 'example-source', false, parked);
  assert.deepEqual(doc.Mail.Destinations[0].Sources, {});
  P.toggleDestinationSource(doc, 0, 'example-source', true, parked);
  assert.deepEqual(doc.Mail.Destinations[0].Sources, { 'example-source': { StripColour: true } });
});

test('ticking a new source adds it with no overrides and leaves the rest alone', () => {
  const doc = sample();
  P.toggleDestinationSource(doc, 0, 'minimal-source', true, {});
  assert.deepEqual(doc.Mail.Destinations[0].Sources, {
    'example-source': { StripColour: true },
    'minimal-source': {}
  });
  const other = sample();
  other.Mail.Destinations[0].Sources['minimal-source'] = {};
  assert.deepEqual(doc, other);
});

test('ticking a source for a destination with no Sources key creates the map', () => {
  const doc = sample();
  delete doc.Mail.Destinations[1].Sources;
  P.toggleDestinationSource(doc, 1, 'example-source', true, {});
  assert.deepEqual(doc.Mail.Destinations[1].Sources, { 'example-source': {} });
});

test('an absent checkbox value shows the default the scraper will use', () => {
  const doc = sample();
  const d = ['Mail', 'Destinations', 0];
  assert.equal(P.boolAt(doc, d.concat(['SendFullVolumes']), P.DEFAULTS.destination.SendFullVolumes), true);
  assert.equal(P.boolAt(doc, d.concat(['StripColour']), P.DEFAULTS.destination.StripColour), false);
  assert.equal(P.boolAt(doc, ['Sources', 1, 'Enabled'], P.DEFAULTS.source.Enabled), true);
});

test('the index copes with minimal and malformed documents', () => {
  for (const doc of [
    {},
    { Sources: 'not a list', Mail: 'not an object' },
    { Sources: [null, 7, {}], Mail: { Destinations: [null, {}] } },
    sample()
  ]) {
    const all = P.entries(doc);
    assert.ok(all.some((e) => e.kind === 'mail'), 'global sections are always there');
    for (const e of all) assert.equal(typeof e.label, 'string');
    for (let i = 0; i < 3; i++) P.destinationSourceRows(doc, i);
  }
  const labels = P.entries(sample()).map((e) => [e.label, e.pill]);
  assert.deepEqual(labels.slice(0, 4), [
    ['Example Serial', ''],
    ['Minimal Serial', ''],
    ['Test Reader', '1'],
    ['Parked Reader', 'none']
  ]);
});

test('a selection that no longer exists falls back to the first entry', () => {
  const doc = sample();
  assert.deepEqual(P.clampSelection(doc, { kind: 'destination', index: 1 }), { kind: 'destination', index: 1 });
  assert.deepEqual(P.clampSelection(doc, { kind: 'destination', index: 5 }), { kind: 'source', index: 0 });
  assert.deepEqual(P.clampSelection({}, { kind: 'source', index: 0 }), { kind: 'epubGen', index: 0 });
});
