// Send controls: the selection bar on the table of contents and the email
// total on the send form.
//
// Neither is needed to send. Without script, the contents page's bar is a
// plain submit button and the form's Send button is always enabled; the
// server checks everything again on submit. The pure functions are
// exported for tests run under node; in a browser the same file attaches to
// whichever of the two pages it is on.
(function (root) {
  'use strict';

  function plural(n, one) { return n + ' ' + one + (n === 1 ? '' : 's'); }

  function selectionLabel(volumes, chapters) {
    var parts = [];
    if (volumes) parts.push(plural(volumes, 'volume'));
    if (chapters) parts.push(plural(chapters, 'chapter'));
    return parts.length ? parts.join(', ') + ' selected' : '';
  }

  function emailTotal(items, destinations, cap) {
    if (destinations === 0) return { text: 'Tick at least one destination.', over: false, disabled: true };
    var n = items * destinations;
    var over = n > cap;
    var text = plural(n, 'email') + ' to ' + plural(destinations, 'destination');
    if (over) text += ', more than ' + cap + '. Send fewer at a time.';
    return { text: text, over: over, disabled: over };
  }

  var logic = { selectionLabel: selectionLabel, emailTotal: emailTotal };
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = logic;
    return;
  }

  var document = root.document;

  var pick = document.getElementById('send-pick');
  if (pick) {
    var bar = document.getElementById('send-bar');
    var count = document.getElementById('send-count');
    var clear = document.getElementById('send-clear');
    var boxes = [].slice.call(pick.querySelectorAll('input.pick'));
    var update = function () {
      var v = boxes.filter(function (b) { return b.checked && b.name === 'v'; }).length;
      var c = boxes.filter(function (b) { return b.checked && b.name === 'c'; }).length;
      count.textContent = selectionLabel(v, c);
      bar.hidden = v + c === 0;
    };
    boxes.forEach(function (b) { b.addEventListener('change', update); });
    clear.addEventListener('click', function () {
      boxes.forEach(function (b) { b.checked = false; });
      update();
    });
    clear.hidden = false;
    // A reload or Back can restore ticks the browser remembered.
    root.addEventListener('pageshow', update);
    update();
  }

  var form = document.getElementById('send-form');
  if (form) {
    var total = document.getElementById('send-total');
    var submit = document.getElementById('send-submit');
    var items = Number(total.getAttribute('data-items'));
    var cap = Number(total.getAttribute('data-cap'));
    var dests = [].slice.call(form.querySelectorAll('input.dest'));
    var refresh = function () {
      var t = emailTotal(items, dests.filter(function (d) { return d.checked; }).length, cap);
      total.textContent = t.text;
      total.className = t.over ? 'error' : '';
      submit.disabled = t.disabled;
    };
    dests.forEach(function (d) { d.addEventListener('change', refresh); });
    // One click, one send: the server refuses a second while the first
    // runs, but the reader should not have to see that.
    form.addEventListener('submit', function () { submit.disabled = true; });
    // Back to a page the browser kept would otherwise leave it disabled.
    root.addEventListener('pageshow', refresh);
    refresh();
  }
})(typeof window !== 'undefined' ? window : this);
