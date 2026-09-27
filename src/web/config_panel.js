// Structured view of config.json on the configuration page.
//
// The JSON in the textarea is the document. This panel is a view over it:
// an edit here changes one value at one path in the parsed document and
// writes the document back to the textarea, and an edit there that parses
// redraws the panel. Nothing here builds a source or destination from form
// fields, so keys the panel does not model survive a save because no code
// path ever rebuilds the object that holds them.
//
// There is no request in this file. Saving stays with the page's Save
// button, which posts the textarea through the server's validation.
//
// Every element is built with createElement and textContent. Source ids,
// names and values come from the document, and building markup out of them
// would make the page its own injection vector.
//
// The pure functions are exported for tests run under node; in a browser
// the same file mounts itself on the page.
(function (root) {
  'use strict';

  // Keys each section edits or shows. Anything else is listed read-only
  // under "Other settings".
  var MODEL = {
    source: ['Id', 'Name', 'Enabled', 'TocUrl', 'Auth', 'Metadata', 'Selectors', 'PostProcessors'],
    sourceNested: {
      Auth: ['Type', 'PatreonName'],
      Metadata: ['Author', 'CoverImage', 'Description'],
      Selectors: ['SelectorType', 'VolumeWrapper', 'VolumeTitle', 'ChapterEntry', 'ChapterLink', 'MainContent']
    },
    destination: ['Name', 'Email', 'SendIndividualChapters', 'SendFullVolumes', 'StripColour', 'Sources'],
    mail: ['Name', 'Address', 'Password', 'PasswordSet', 'Destinations'],
    epubGen: ['Chapters', 'Volumes', 'StripColour'],
    topLevel: ['Mail', 'EpubGen', 'Sources']
  };

  // What the scraper assumes when a key is absent (the serde defaults in
  // config.rs). A checkbox for an absent key must show what will happen,
  // not an unticked box.
  var DEFAULTS = {
    source: { Enabled: true },
    destination: { SendIndividualChapters: false, SendFullVolumes: true, StripColour: false },
    epubGen: { Chapters: true, Volumes: true, StripColour: false }
  };

  function isObject(v) {
    return v !== null && typeof v === 'object' && !Array.isArray(v);
  }

  function getPath(doc, path) {
    var o = doc;
    for (var i = 0; i < path.length; i++) {
      if (o === null || typeof o !== 'object') return undefined;
      o = o[path[i]];
    }
    return o;
  }

  // Set one value, creating any missing object on the way. Nothing else in
  // the document is touched.
  function setPath(doc, path, value) {
    var o = doc;
    for (var i = 0; i < path.length - 1; i++) {
      var next = o[path[i]];
      if (next === null || typeof next !== 'object') {
        next = {};
        o[path[i]] = next;
      }
      o = next;
    }
    o[path[path.length - 1]] = value;
  }

  // A document is usable if it parses to an object. Anything else leaves the
  // panel on the last version that was.
  function parseDocument(text) {
    try {
      var doc = JSON.parse(text);
      return isObject(doc) ? { ok: true, doc: doc } : { ok: false, reason: 'is not an object' };
    } catch (e) {
      return { ok: false, reason: 'does not parse' };
    }
  }

  function list(doc, key) {
    return Array.isArray(doc[key]) ? doc[key] : [];
  }

  function destinations(doc) {
    var mail = isObject(doc.Mail) ? doc.Mail : {};
    return Array.isArray(mail.Destinations) ? mail.Destinations : [];
  }

  // Keys in `obj` the panel does not model, including unmodelled keys inside
  // the nested objects it does model. Paths are for display.
  function otherSettings(obj, known, nestedKnown) {
    if (!isObject(obj)) return [];
    var out = [];
    Object.keys(obj).forEach(function (k) {
      if (known.indexOf(k) === -1) out.push({ path: k, value: obj[k] });
    });
    Object.keys(nestedKnown || {}).forEach(function (k) {
      if (!isObject(obj[k])) return;
      Object.keys(obj[k]).forEach(function (sub) {
        if (nestedKnown[k].indexOf(sub) === -1) out.push({ path: k + '.' + sub, value: obj[k][sub] });
      });
    });
    return out;
  }

  // One row per configured source, plus any id the destination lists that is
  // no longer a configured source, so it is never invisible.
  function destinationSourceRows(doc, destIndex) {
    var dest = destinations(doc)[destIndex] || {};
    var listed = isObject(dest.Sources) ? dest.Sources : {};
    var rows = [];
    var seen = {};
    list(doc, 'Sources').forEach(function (s) {
      if (!isObject(s) || typeof s.Id !== 'string' || !s.Id || seen[s.Id]) return;
      seen[s.Id] = true;
      rows.push({
        id: s.Id,
        label: s.Name || s.Id,
        checked: Object.prototype.hasOwnProperty.call(listed, s.Id),
        overrides: listed[s.Id],
        stale: false
      });
    });
    Object.keys(listed).forEach(function (id) {
      if (seen[id]) return;
      rows.push({ id: id, label: id, checked: true, overrides: listed[id], stale: true });
    });
    return rows;
  }

  // Tick or untick one source for one destination. An unticked source's
  // overrides are kept in `parked` for the life of the page, so ticking it
  // again restores them instead of resetting them to the defaults.
  function toggleDestinationSource(doc, destIndex, id, on, parked) {
    var P = ['Mail', 'Destinations', destIndex, 'Sources'];
    var listed = getPath(doc, P);
    if (!isObject(listed)) {
      listed = {};
      setPath(doc, P, listed);
    }
    var key = destIndex + ':' + id;
    if (on) {
      if (!Object.prototype.hasOwnProperty.call(listed, id)) {
        listed[id] = Object.prototype.hasOwnProperty.call(parked, key) ? parked[key] : {};
      }
      delete parked[key];
    } else if (Object.prototype.hasOwnProperty.call(listed, id)) {
      parked[key] = listed[id];
      delete listed[id];
    }
  }

  function boolAt(doc, path, fallback) {
    var v = getPath(doc, path);
    return typeof v === 'boolean' ? v : fallback;
  }

  // The index: one entry per source, destination and global section.
  function entries(doc) {
    var out = [];
    list(doc, 'Sources').forEach(function (s, i) {
      s = isObject(s) ? s : {};
      out.push({
        kind: 'source', index: i, group: 'Sources',
        label: s.Name || s.Id || '(unnamed source)',
        pill: boolAt(doc, ['Sources', i, 'Enabled'], DEFAULTS.source.Enabled) ? '' : 'off'
      });
    });
    destinations(doc).forEach(function (d, i) {
      d = isObject(d) ? d : {};
      var n = isObject(d.Sources) ? Object.keys(d.Sources).length : 0;
      out.push({
        kind: 'destination', index: i, group: 'Destinations',
        label: d.Name || d.Email || '(unnamed destination)',
        pill: n ? String(n) : 'none'
      });
    });
    out.push({ kind: 'epubGen', index: 0, group: 'Global', label: 'EPUB generation', pill: '' });
    out.push({ kind: 'mail', index: 0, group: 'Global', label: 'Mail account', pill: '' });
    if (otherSettings(doc, MODEL.topLevel).length) {
      out.push({ kind: 'topLevel', index: 0, group: 'Global', label: 'Other settings', pill: '' });
    }
    return out;
  }

  // Keep the selection on something that exists after the document changes.
  function clampSelection(doc, sel) {
    var all = entries(doc);
    for (var i = 0; i < all.length; i++) {
      if (all[i].kind === sel.kind && all[i].index === sel.index) return sel;
    }
    return { kind: all[0].kind, index: all[0].index };
  }

  var logic = {
    MODEL: MODEL,
    DEFAULTS: DEFAULTS,
    getPath: getPath,
    setPath: setPath,
    parseDocument: parseDocument,
    otherSettings: otherSettings,
    destinationSourceRows: destinationSourceRows,
    toggleDestinationSource: toggleDestinationSource,
    boolAt: boolAt,
    entries: entries,
    clampSelection: clampSelection
  };

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = logic;
    return;
  }
  mount(root.document, logic);

  // ---- Browser ------------------------------------------------------------

  function mount(document, L) {
    var area = document.getElementById('config-json');
    var saveButton = document.getElementById('save');
    var panel = document.getElementById('config-panel');
    if (!area || !panel) return;

    var fields = document.getElementById('panel-fields');
    var indexBox = document.getElementById('panel-index');
    var detailBox = document.getElementById('panel-detail');
    var accordion = document.getElementById('panel-accordion');
    var badge = document.getElementById('panel-badge');
    var hint = document.getElementById('panel-hint');
    var wide = root.matchMedia('(min-width: 760px)');

    var first = L.parseDocument(area.value);
    var state = {
      doc: first.ok ? first.doc : {},
      valid: first.ok,
      sel: { kind: 'source', index: 0 },
      parked: {},
      // The index names, pills and title, relabelled in place after an edit
      // so that editing a name never redraws the field being typed in.
      labelNodes: []
    };
    state.sel = L.clampSelection(state.doc, state.sel);

    function el(tag, attrs, text) {
      var node = document.createElement(tag);
      Object.keys(attrs || {}).forEach(function (k) {
        if (k === 'className') node.className = attrs[k];
        else node.setAttribute(k, attrs[k]);
      });
      if (text !== undefined) node.textContent = text;
      return node;
    }

    // ---- Writing back ------------------------------------------------------

    function announce() {
      area.dispatchEvent(new CustomEvent('config-json-changed', { detail: { origin: 'panel' } }));
    }

    // A form edit: change one value, write the document back, relabel.
    // Never redraws the field being typed in.
    function edit(path, value) {
      if (!state.valid) return;
      L.setPath(state.doc, path, value);
      area.value = JSON.stringify(state.doc, null, 2);
      announce();
      relabel();
    }

    // ---- Field builders ----------------------------------------------------

    var fieldId = 0;
    function labelled(text, control) {
      var id = 'panel-f' + (fieldId += 1);
      control.id = id;
      var wrap = el('div', { className: 'panel-field' });
      wrap.appendChild(el('label', { for: id }, text));
      wrap.appendChild(control);
      return wrap;
    }

    function text(label, path) {
      var input = el('input', { type: 'text', spellcheck: 'false' });
      var v = L.getPath(state.doc, path);
      input.value = v === undefined || v === null ? '' : String(v);
      input.addEventListener('input', function () { edit(path, input.value); });
      return labelled(label, input);
    }

    function check(label, path, fallback) {
      var wrap = el('label', { className: 'panel-check' });
      var input = el('input', { type: 'checkbox' });
      input.checked = L.boolAt(state.doc, path, fallback);
      input.addEventListener('change', function () { edit(path, input.checked); });
      wrap.appendChild(input);
      wrap.appendChild(document.createTextNode(label));
      return wrap;
    }

    // A value outside `options` is kept as an option, so opening a section
    // never changes it and choosing it again is possible.
    function select(label, path, options, fallback, onChange) {
      var input = el('select');
      var current = L.getPath(state.doc, path);
      if (current === undefined) current = fallback;
      var all = options.slice();
      if (all.indexOf(current) === -1) all.push(current);
      all.forEach(function (o) {
        var opt = el('option', { value: String(o) }, String(o));
        if (o === current) opt.selected = true;
        input.appendChild(opt);
      });
      input.addEventListener('change', function () {
        edit(path, input.value);
        if (onChange) onChange(input.value);
      });
      return labelled(label, input);
    }

    // Shown, not editable. See the Id field in renderSource.
    function fixed(label, path, why) {
      var input = el('input', { type: 'text' });
      var v = L.getPath(state.doc, path);
      input.value = v === undefined || v === null ? '' : String(v);
      input.readOnly = true;
      var wrap = labelled(label, input);
      wrap.appendChild(note(why));
      return wrap;
    }

    function heading(text) { return el('h3', { className: 'panel-sub' }, text); }

    function note(text) { return el('p', { className: 'summary' }, text); }

    function other(items) {
      if (!items.length) return null;
      var box = el('details', { className: 'panel-other' });
      box.appendChild(el('summary', {}, 'Other settings: ' + items.length + ' not edited here (read-only)'));
      items.forEach(function (item) {
        box.appendChild(el('div', { className: 'panel-mono' }, item.path + ': ' + JSON.stringify(item.value)));
      });
      box.appendChild(note('Kept exactly as they are when you save. Edit them in the JSON below.'));
      return box;
    }

    function append(parent, children) {
      children.forEach(function (c) { if (c) parent.appendChild(c); });
      return parent;
    }

    // ---- One renderer per section, used by both layouts ------------------

    function renderSource(i) {
      var P = ['Sources', i];
      var s = L.getPath(state.doc, P) || {};
      var patreonName = text('Patreon name', P.concat(['Auth', 'PatreonName']));
      var showPatreon = function (type) { patreonName.hidden = type !== 'Patreon'; };
      var auth = select('Type', P.concat(['Auth', 'Type']), ['None', 'Patreon'], 'None', showPatreon);
      showPatreon(L.getPath(state.doc, P.concat(['Auth', 'Type'])) || 'None');
      var processors = Array.isArray(s.PostProcessors) ? s.PostProcessors : [];
      return [
        text('Name', P.concat(['Name'])),
        // The id names the source's database, db/{id}.db. Changing it here
        // would point the source at a new, empty database: the next scrape
        // re-downloads the whole series and the old history is orphaned.
        // That is a deliberate act, so it is done in the JSON.
        fixed('Id', P.concat(['Id']),
              'Changing the id starts a new database for this source, so it is changed in the JSON below.'),
        check('Enabled', P.concat(['Enabled']), L.DEFAULTS.source.Enabled),
        text('Table-of-contents URL', P.concat(['TocUrl'])),
        heading('Auth'), auth, patreonName,
        heading('Metadata'),
        text('Author', P.concat(['Metadata', 'Author'])),
        text('Cover image', P.concat(['Metadata', 'CoverImage'])),
        text('Description', P.concat(['Metadata', 'Description'])),
        heading('Selectors'),
        select('Selector type', P.concat(['Selectors', 'SelectorType']), ['class', 'id', 'tag'], 'class'),
        text('Volume wrapper', P.concat(['Selectors', 'VolumeWrapper'])),
        text('Volume title', P.concat(['Selectors', 'VolumeTitle'])),
        text('Chapter entry', P.concat(['Selectors', 'ChapterEntry'])),
        text('Chapter link', P.concat(['Selectors', 'ChapterLink'])),
        text('Main content', P.concat(['Selectors', 'MainContent'])),
        heading('Post-processors'),
        el('p', { className: 'panel-mono' }, processors.length ? processors.join(', ') : 'none'),
        other(L.otherSettings(s, L.MODEL.source, L.MODEL.sourceNested))
      ];
    }

    function renderDestination(i) {
      var P = ['Mail', 'Destinations', i];
      var d = L.getPath(state.doc, P) || {};
      var rows = L.destinationSourceRows(state.doc, i);
      var box = el('div', { className: 'panel-picks' });
      var empty = note('Sent nothing: no sources are ticked.');
      var refreshEmpty = function () {
        empty.hidden = L.destinationSourceRows(state.doc, i).some(function (r) { return r.checked; });
      };
      rows.forEach(function (r) {
        var wrap = el('label', { className: 'panel-check' });
        var input = el('input', { type: 'checkbox' });
        input.checked = r.checked;
        input.addEventListener('change', function () {
          if (!state.valid) return;
          L.toggleDestinationSource(state.doc, i, r.id, input.checked, state.parked);
          area.value = JSON.stringify(state.doc, null, 2);
          announce();
          relabel();
          refreshEmpty();
        });
        wrap.appendChild(input);
        wrap.appendChild(document.createTextNode(r.label + (r.stale ? ' (not a configured source)' : '')));
        box.appendChild(wrap);
        if (r.overrides && typeof r.overrides === 'object' && Object.keys(r.overrides).length) {
          box.appendChild(el('div', { className: 'panel-mono' }, 'overrides: ' + JSON.stringify(r.overrides)));
        }
      });
      refreshEmpty();
      return [
        text('Name', P.concat(['Name'])),
        text('Email', P.concat(['Email'])),
        check('Send individual chapters', P.concat(['SendIndividualChapters']),
              L.DEFAULTS.destination.SendIndividualChapters),
        check('Send full volumes', P.concat(['SendFullVolumes']), L.DEFAULTS.destination.SendFullVolumes),
        check('Strip colour', P.concat(['StripColour']), L.DEFAULTS.destination.StripColour),
        heading('Sources sent here'),
        box, empty,
        other(L.otherSettings(d, L.MODEL.destination))
      ];
    }

    function renderEpubGen() {
      return [
        check('Generate chapters', ['EpubGen', 'Chapters'], L.DEFAULTS.epubGen.Chapters),
        check('Generate volumes', ['EpubGen', 'Volumes'], L.DEFAULTS.epubGen.Volumes),
        check('Strip colour', ['EpubGen', 'StripColour'], L.DEFAULTS.epubGen.StripColour),
        note('Each is also switched on for anything a destination is sent.'),
        other(L.otherSettings(state.doc.EpubGen, L.MODEL.epubGen))
      ];
    }

    function renderMail() {
      var set = L.getPath(state.doc, ['Mail', 'PasswordSet']) === true;
      return [
        text('Display name', ['Mail', 'Name']),
        text('From address', ['Mail', 'Address']),
        note((set ? 'A password is set.' : 'No password is set.') +
             ' Change it in the New mail password field below.'),
        other(L.otherSettings(state.doc.Mail, L.MODEL.mail))
      ];
    }

    function renderTopLevel() {
      return [other(L.otherSettings(state.doc, L.MODEL.topLevel))];
    }

    function renderSelected() {
      var s = state.sel;
      if (s.kind === 'source') return renderSource(s.index);
      if (s.kind === 'destination') return renderDestination(s.index);
      if (s.kind === 'epubGen') return renderEpubGen();
      if (s.kind === 'mail') return renderMail();
      return renderTopLevel();
    }

    // ---- Layout ------------------------------------------------------------

    function isSelected(e) { return e.kind === state.sel.kind && e.index === state.sel.index; }

    function titleFor(e) { return e.label; }

    // Rebuild the index or accordion and the selected section. Only called
    // when nothing in the panel is being typed in: on load, on selection,
    // on a JSON edit that parses, and on a layout change.
    function draw() {
      state.labelNodes = [];
      indexBox.textContent = '';
      detailBox.textContent = '';
      accordion.textContent = '';
      var all = L.entries(state.doc);
      var group = null;
      all.forEach(function (e) {
        if (e.group !== group) {
          group = e.group;
          (wide.matches ? indexBox : accordion).appendChild(el('div', { className: 'panel-group' }, group));
        }
        var name = el('span', { className: 'panel-name' }, e.label);
        var pill = el('span', { className: 'panel-pill' }, e.pill);
        pill.hidden = !e.pill;
        state.labelNodes.push({ entry: e, name: name, pill: pill });
        if (wide.matches) {
          var item = el('button', { type: 'button', className: 'panel-item', 'aria-current': String(isSelected(e)) });
          item.appendChild(name);
          item.appendChild(pill);
          item.addEventListener('click', function () { state.sel = { kind: e.kind, index: e.index }; draw(); });
          indexBox.appendChild(item);
        } else {
          var acc = el('details', { className: 'panel-acc' });
          var summary = el('summary');
          summary.appendChild(name);
          summary.appendChild(pill);
          acc.appendChild(summary);
          if (isSelected(e)) {
            acc.open = true;
            append(acc.appendChild(el('div', { className: 'panel-body panel-grid' })), renderSelected());
          }
          acc.addEventListener('toggle', function () {
            if (acc.open && !isSelected(e)) { state.sel = { kind: e.kind, index: e.index }; draw(); }
          });
          accordion.appendChild(acc);
        }
      });
      if (wide.matches) {
        var current = all.filter(isSelected)[0];
        var title = el('h3', { className: 'panel-title' }, current ? titleFor(current) : '');
        state.titleNode = title;
        detailBox.appendChild(title);
        append(detailBox.appendChild(el('div', { className: 'panel-grid' })), renderSelected());
      } else {
        state.titleNode = null;
      }
    }

    // After a form edit: update names, counts and the title in place.
    function relabel() {
      var all = L.entries(state.doc);
      state.labelNodes.forEach(function (n) {
        var e = all.filter(function (x) { return x.kind === n.entry.kind && x.index === n.entry.index; })[0];
        if (!e) return;
        n.name.textContent = e.label;
        n.pill.textContent = e.pill;
        n.pill.hidden = !e.pill;
        if (state.titleNode && isSelected(e)) state.titleNode.textContent = titleFor(e);
      });
    }

    function showValidity(result) {
      badge.textContent = result.ok ? 'JSON valid' : 'JSON ' + result.reason;
      badge.className = 'panel-badge ' + (result.ok ? 'ok' : 'error');
      hint.textContent = result.ok
        ? 'Edit either side; each follows the other.'
        : 'The form is showing the last version that parsed and cannot be edited. Save is disabled until the JSON parses.';
      fields.disabled = !result.ok;
      if (saveButton) saveButton.disabled = !result.ok;
    }

    // The JSON changed: typed by the operator, or written by the add-forms.
    function fromJson() {
      var result = L.parseDocument(area.value);
      state.valid = result.ok;
      showValidity(result);
      if (!result.ok) return;
      state.doc = result.doc;
      state.parked = {};
      state.sel = L.clampSelection(state.doc, state.sel);
      draw();
    }

    area.addEventListener('input', fromJson);
    area.addEventListener('config-json-changed', function (e) {
      if (!e.detail || e.detail.origin !== 'panel') fromJson();
    });
    wide.addEventListener('change', draw);

    showValidity(first);
    draw();
  }
})(typeof window !== 'undefined' ? window : this);
