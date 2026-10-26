// The value panel against a real server: a value read into it and saved from it is stored exactly
// as it was read - untouched, in every view, format and encoding, and edited, changed only where it
// was edited. The values are the awkward ones: CR LF and lone CR, JSON with a number past 2^53 and a
// 1.0, XML whose whitespace Auto format replaces, latin1 text, bytes that are not text, emoji.
// The unit tests (tests/ui/value-panel.test.mjs) cover the same ground with a stand-in text box; this
// is the real one, the real grid and the real Apply.
(async () => {
  if (typeof vpLoad !== 'function') { G.skip('the value panel', 'this edition has no value panel yet'); return G.report(); }
  const DB = 'nobs_gui_value';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.v (id INT PRIMARY KEY, t TEXT CHARACTER SET utf8mb4, l VARCHAR(40) CHARACTER SET latin1, b BLOB, vb VARBINARY(32));
INSERT INTO ${DB}.v VALUES
 (1, CONCAT('line1', CHAR(13,10), 'line2', CHAR(13), 'line3', CHAR(10), 'end'), CONCAT('caf', _latin1 X'E9'), X'00FF100D0A', X''),
 (2, '{"id":12345678901234567890,"v":1.0,"s":"a b","e":{},"a":[]}', 'plain', CONVERT('{ "a" : [1, 2] }' USING utf8mb4), X'48006900'),
 (3, CONCAT('<?xml version="1.0"?><r><a x="1>2">t</a>', CHAR(13,10), '<c>  sp  </c></r>'), NULL, CONVERT('<p>Hi <b>there</b></p>' USING utf8mb4), NULL),
 (4, 'Grüße 😀 你好 שלום', '', X'89504E470D0A1A0A', X'0D0A0D'),
 (5, '', 'x', NULL, X'41');`);

    const COLS = ['t', 'l', 'b', 'vb'];
    const hexOf = async (col, id) => await G.one(`SELECT HEX(${col}) FROM ${DB}.v WHERE id = ${id}`, DB);
    const base = {};
    for (const c of COLS) for (let id = 1; id <= 5; id++) base[c + id] = await hexOf(c, id);
    G.eq('the fixture keeps its CR LF and lone CR', base.t1, '6C696E65310D0A6C696E65320D6C696E65330A656E64');

    const t = await G.openTable(DB, 'v');
    if (!vpOpen) await vpToggle();
    const rowOf = id => t.rows.findIndex(r => String(r[t.cols.indexOf('id')]) === String(id));
    const box = $('vpx_' + t.id);
    const show = (id, col, view, fmt, enc) => {
      const ci = t.cols.indexOf(col), p = vpPref(t, ci);
      p.view = view; p.fmt = fmt; p.enc = enc;
      vpLoad(t.id, { ri: rowOf(id), ci });
      return vpState[t.id];
    };

    // Untouched: Save writes nothing, and the box reads back as the value, in every combination.
    const VIEWS = ['binary', 'html', 'json', 'text', 'xml'], ENCS = ['utf-8', 'latin1', 'windows-1252', 'utf-16le'];
    const wrote = [], differs = [];
    let editable = 0;
    for (const col of COLS) for (let id = 1; id <= 5; id++) for (const view of VIEWS) for (const fmt of [true, false]) for (const enc of ENCS) {
      const st = show(id, col, view, fmt, enc), what = `${col}${id} ${view} fmt=${fmt} ${enc}`;
      vpSave(t.id);
      if (pendingCount(t)) { wrote.push(what + ' ' + JSON.stringify(t.pending.upd)); t.pending.upd = {}; }
      if (box.readOnly || st.orig === null) continue;
      editable++;
      const r = vpBoxValue(t.id);
      if (r.err || !vpSame(r.v, st.orig, st.kind)) differs.push({ what, orig: st.orig, back: r.err || r.v, box: box.value });
    }
    G.eq('a value saved untouched is never written', wrote, []);
    G.eq('the text box reads back as exactly the value read', differs, []);
    G.check('most combinations can be edited', editable > 200, { editable });

    // Edited: one letter typed at the start or the end, saved, applied - and the stored bytes are
    // the old ones with that letter added, every CR, space and digit around it as it was.
    const type = (at, ins) => { box.value = box.value.slice(0, at) + ins + box.value.slice(at); box.dispatchEvent(new Event('input')); };
    const EDITS = [
      ['t', 1, 'text', true, 'utf-8', 'end'],
      ['t', 5, 'json', true, 'utf-8', 'start'],
      ['t', 3, 'xml', true, 'utf-8', 'end'],
      ['t', 4, 'text', false, 'utf-8', 'end'],
      ['l', 1, 'text', true, 'utf-8', 'end'],
      ['b', 1, 'binary', true, 'utf-8', 'end'],
      ['b', 2, 'json', true, 'utf-8', 'start'],
      ['b', 3, 'html', true, 'utf-8', 'end'],
      ['vb', 4, 'text', false, 'latin1', 'end'],
      ['vb', 1, 'binary', false, 'utf-8', 'start'],
    ];
    for (const [col, id, view, fmt, enc, where] of EDITS) {
      const what = `${col}${id} edited in ${view} (fmt=${fmt}, ${enc}) at the ${where}`;
      show(id, col, view, fmt, enc);
      if (!G.check(what + ' can be edited', !box.readOnly, { note: $('vpn_' + t.id).textContent })) continue;
      // 'Z' as text; in Binary view the same byte, 5A, typed as hex.
      type(where === 'start' ? 0 : box.value.length, view === 'binary' ? '5A' : 'Z');
      G.check(what + ' is saved into the cell', vpSave(t.id) && pendingCount(t) === 1, { pending: t.pending.upd, toasts: G.toasts.slice(-3) });
      await applyChanges(t.id);
      await G.until(() => !t.runningReqId && pendingCount(t) === 0, 20000);
      const was = base[col + id] || '';
      G.eq(what + ' is stored with only that change', await hexOf(col, id), where === 'start' ? '5A' + was : was + '5A');
    }

    // Laid-out JSON edited in the middle: the number past 2^53, the 1.0 and the space inside the
    // string are stored as they were.
    show(2, 't', 'json', true, 'utf-8');
    G.check('the JSON is laid out', box.value.startsWith('{\n  "id": 12345678901234567890,'), box.value);
    const at = box.value.indexOf('1.0');
    box.value = box.value.slice(0, at) + '2.5' + box.value.slice(at + 3); box.dispatchEvent(new Event('input'));
    vpSave(t.id);
    await applyChanges(t.id);
    await G.until(() => !t.runningReqId && pendingCount(t) === 0, 20000);
    G.eq('an edit inside laid-out JSON stores only that edit',
      await G.one(`SELECT t FROM ${DB}.v WHERE id = 2`, DB), '{"id":12345678901234567890,"v":2.5,"s":"a b","e":{},"a":[]}');

    G.eq('and the app said nothing was wrong', G.errs(), []);
  } finally {
    if (typeof vpOpen !== 'undefined' && vpOpen) await vpToggle();
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
