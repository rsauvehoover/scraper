// Folding volumes on the table-of-contents page.
//
// Each volume is a <details>. The server opens the newest one, which is
// also all a reader without JavaScript gets: folding itself needs no
// script. This file adds the collapse/expand-all button and remembers which
// volumes the reader left open, per series, in this browser only.
//
// The pure functions are exported for tests run under node; in a browser the
// same file attaches itself to the page.
(function (root) {
  'use strict';

  // Which volumes to open, by id. A remembered choice wins. A volume with
  // none, such as one added since the last visit, takes the page's default.
  function openStates(ids, saved, defaults) {
    var out = {};
    ids.forEach(function (id) {
      var remembered = saved && Object.prototype.hasOwnProperty.call(saved, id) &&
        typeof saved[id] === 'boolean';
      out[id] = remembered ? saved[id] : defaults[id] === true;
    });
    return out;
  }

  // What the button does next: collapse if anything is open, otherwise expand.
  function toggleAction(states) {
    return Object.keys(states).some(function (id) { return states[id]; }) ? 'collapse' : 'expand';
  }

  // A stored value is used only if it is an object of booleans; anything else
  // is treated as nothing remembered.
  function parseSaved(raw) {
    try {
      var v = JSON.parse(raw);
      if (!v || typeof v !== 'object' || Array.isArray(v)) return null;
      return Object.keys(v).every(function (k) { return typeof v[k] === 'boolean'; }) ? v : null;
    } catch (e) {
      return null;
    }
  }

  var logic = { openStates: openStates, toggleAction: toggleAction, parseSaved: parseSaved };
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = logic;
    return;
  }

  var document = root.document;
  var box = document.getElementById('volumes');
  var button = document.getElementById('toggle-volumes');
  if (!box || !button) return;

  var key = box.getAttribute('data-storage-key');
  var volumes = [].slice.call(box.querySelectorAll('details.volume'));
  var idOf = function (d) { return d.getAttribute('data-volume'); };

  // Storage can be missing or refuse (a private window, blocked site data).
  // Then nothing is remembered and the page still works.
  function read() {
    try { return parseSaved(root.localStorage.getItem(key)); } catch (e) { return null; }
  }
  function write() {
    try { root.localStorage.setItem(key, JSON.stringify(states)); } catch (e) { /* not kept */ }
  }

  function label() {
    button.textContent = toggleAction(states) === 'collapse' ? 'Collapse all' : 'Expand all';
  }

  var defaults = {};
  volumes.forEach(function (d) { defaults[idOf(d)] = d.open; });
  var states = openStates(volumes.map(idOf), read(), defaults);

  // `states` is what the page is meant to show. Setting `open` fires
  // `toggle` later, so a toggle that already matches `states` is one this
  // script caused and is not a choice to remember. Only the reader's own
  // clicks are written.
  volumes.forEach(function (d) {
    d.open = states[idOf(d)];
    d.addEventListener('toggle', function () {
      if (d.open === states[idOf(d)]) return;
      states[idOf(d)] = d.open;
      label();
      write();
    });
  });

  button.addEventListener('click', function () {
    var open = toggleAction(states) === 'expand';
    volumes.forEach(function (d) {
      states[idOf(d)] = open;
      d.open = open;
    });
    label();
    write();
  });

  label();
  button.hidden = false;
})(typeof window !== 'undefined' ? window : this);
