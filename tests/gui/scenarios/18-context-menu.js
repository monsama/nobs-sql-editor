// The cell menu offers what can be done to this cell, now: no paste when nothing has been copied,
// no "(selected)" commands when nothing is ticked, and nothing that edits a result that cannot be
// edited. Each of those used to be offered and then answer with a toast saying why not.
(async () => {
  const items = () => [...document.querySelectorAll('#ctx > .item')].map(d => d.textContent.replace(/\s+▸$/, ''));
  const parts = () => [...document.querySelectorAll('#ctx > *')].map(d => d.className);
  const open = (id, ri, ci) => { cellMenu({ preventDefault() {}, clientX: 40, clientY: 40 }, id, ri, ci); return items(); };

  // whatever schema this run has to hand - the scenario before this one may have left none selected
  const sys = ['information_schema', 'performance_schema', 'mysql', 'sys'];
  const db = (await G.q('SELECT DATABASE()'))[0][0] || (await G.q('SHOW DATABASES')).map(r => r[0]).filter(d => !sys.includes(d))[0];
  const tbl = (await G.q('SHOW TABLES FROM ' + '`' + db + '`'))[0][0];
  const i = openTab(tbl, 'SELECT * FROM `' + db + '`.`' + tbl + '`;', db, false, tbl);
  await openRun(i);
  await G.until(() => T(i).rows && T(i).rows.length > 1, 20000);
  // the clipboards are set directly: what is being tested is what the menu makes of them, and a
  // real copy needs a permission the browser does not grant a test.
  window._rowClipboard = null; window._rowsClipboard = null;
  T(i).selected = new Set();

  let m = open(i, 0, 0);
  G.check('nothing copied, so nothing to paste', !m.some(x => /^Paste/.test(x)), m);
  G.check('nothing ticked, so no command about a selection', !m.some(x => /\(selected/.test(x)), m);
  G.check('a table with a key can still be edited from here', m.includes('Set NULL') && m.includes('Set empty'), m);
  G.check('and no separator is left doubled or dangling', !parts().join(',').includes('sep,sep') && parts()[parts().length - 1] !== 'sep', parts());

  window._rowClipboard = T(i).cols.map((c, ci) => T(i).rows[0][ci]);
  m = open(i, 1, 0);
  G.check('a copied row brings both pastes back', m.includes('Paste row here (overwrite)') && m.includes('Paste rows as new'), m);
  G.check('and pasting it leaves it on the clipboard, as a copy should', (() => {
    const before = window._rowClipboard.slice();
    pasteRowInto(i, 1);
    return !!window._rowClipboard && String(window._rowClipboard) === String(before) && open(i, 1, 0).includes('Paste row here (overwrite)');
  })(), 'the clipboard was consumed by the paste');
  T(i).pending.upd = {};
  // a row from somewhere else, with the wrong number of columns, cannot go in here
  window._rowClipboard = ['only', 'two'];
  m = open(i, 1, 0);
  G.check('a row of the wrong width is not offered', T(i).cols.length === 2 || !m.includes('Paste row here (overwrite)'), m);
  window._rowClipboard = T(i).cols.map((c, ci) => T(i).rows[0][ci]);

  T(i).selected = new Set([0, 1]);
  m = open(i, 0, 0);
  G.eq('ticked rows bring back all three selection commands', m.filter(x => /\(selected/.test(x)).length, 3);

  const j = openTab('scalar', 'SELECT 1 AS one;', db, false, null);
  await runSql(j, 'SELECT 1 AS one;');
  await G.until(() => T(j).rows && T(j).rows.length, 20000);
  m = open(j, 0, 0);
  G.check('a result with no key is not offered edits or pastes', !m.includes('Set NULL') && !m.includes('Set empty') && !m.some(x => /^Paste/.test(x)), m);
  G.check('but is still copied and exported', m.includes('Copy value') && m.some(x => /Export to CSV \(all/.test(x)), m);
  $('ctx').style.display = 'none';
  return G.report();
})()
