// The context menus are laid out the same way everywhere: the main action first, then things grouped
// by what they act on, and what empties or removes something last, on its own.
(async () => {
  const DB = 'nobs_gui_menus';
  const own = d => [...d.childNodes].filter(n => n.nodeType === 3).map(n => n.textContent).join('').replace(/\s+▸$/, '').trim();
  // The menu as shown, separators as '-', a submenu as "Label ▸ [entries]"
  const shown = () => [...document.querySelectorAll('#ctx > *')].map(d => d.className === 'sep' ? '-' : own(d));
  const sub = label => { const d = [...document.querySelectorAll('#ctx > .item')].find(x => own(x) === label); return d ? [...d.querySelectorAll(':scope > .ctxsub > *')].map(s => s.className === 'sep' ? '-' : own(s)) : null; };
  const ev = { preventDefault() {}, stopPropagation() {}, clientX: 40, clientY: 40 };
  const before = (m, a, b) => m.indexOf(a) >= 0 && m.indexOf(b) >= 0 && m.indexOf(a) < m.indexOf(b);
  const groupOf = (m, x) => m.slice(0, m.indexOf(x)).filter(y => y === '-').length;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.t (id INT PRIMARY KEY, v VARCHAR(10) NULL); INSERT INTO ${DB}.t VALUES (1,'a'),(2,'b');
CREATE VIEW ${DB}.vw AS SELECT id FROM ${DB}.t;`);
    await G.run(`CREATE PROCEDURE ${DB}.p(IN a INT, OUT b VARCHAR(20)) SET b = CONCAT('x', a)`);
    await G.run(`CREATE FUNCTION ${DB}.f(x INT) RETURNS INT DETERMINISTIC RETURN x * 2`);

    // the cell menu
    const t = await G.openTable(DB, 't'); window._cellClipboard = [['z']]; window._rowClipboard = null; window._rowsClipboard = null;
    await cellMenu(ev, t.id, 0, 1); const c = shown();
    G.check('cell: this cell first - edit, NULL, empty', c[0] === 'Edit value...' && groupOf(c, 'Set NULL') === 0 && groupOf(c, 'Set empty') === 0, c);
    G.check('then the clipboard, copies before pastes', groupOf(c, 'Copy value') === 1 && groupOf(c, 'Paste value') === 1 && before(c, 'Copy row', 'Paste value'), c);
    G.check('then the row', groupOf(c, 'Edit full row (form)...') === 2 && groupOf(c, 'Delete row') === 2, c);
    G.check('then finding rows, then Export', before(c, 'Quick filter', 'Export') && c.lastIndexOf('Export') > c.indexOf('Delete row'), c);
    G.check('Export parts its formats only when each has two forms', !sub('Export').includes('-'), sub('Export'));
    $('ctx').style.display = 'none';
    // "Delete row" marks the row, as the grid's x does
    await cellMenu(ev, t.id, 1, 1); [...document.querySelectorAll('#ctx > .item')].find(x => own(x) === 'Delete row').click();
    G.check('Delete row marks it for deletion', t.pending.del.has(1));
    revertChanges(t.id);
    closeTab(t.id);

    // the object tree
    objMenu(ev, DB, 'table', 't'); const tm = shown();
    G.check('table: opening first', tm[0] === 'SELECT *', tm);
    G.eq('table: truncate and drop last, on their own', tm.slice(-3), ['-', 'Truncate...', 'Drop table...']);
    G.check('table: rename and duplicate before them', before(tm, 'Rename...', 'Truncate...') && groupOf(tm, 'Rename...') < groupOf(tm, 'Truncate...'), tm);
    G.eq('table: the exports in a submenu', sub('Export'), ['SQL dump...', 'CSV (all rows)...', 'INSERTs (all rows)...']);
    G.check('table: pinning with upkeep, not first', groupOf(tm, 'Maintenance') === groupOf(tm, tm.find(x => /Pin to top|Unpin/.test(x))) && tm[0] !== '☆ Pin to top', tm);
    objMenu(ev, DB, 'view', 'vw'); const vm = shown();
    G.check('view: open first, exports offered, drop last', vm[0] === 'Open' && !!sub('Export') && vm[vm.length - 1] === 'Drop view...', vm);
    $('ctx').style.display = 'none';

    // Call...
    objMenu(ev, DB, 'procedure', 'p'); const pm = shown();
    G.check('procedure: Call first, drop last', pm[0] === 'Call...' && pm[pm.length - 1] === 'Drop procedure...', pm);
    const n0 = tabs.length;
    [...document.querySelectorAll('#ctx > .item')].find(x => own(x) === 'Call...').click();
    await G.until(() => tabs.length > n0, 5000);
    const pt = tabs[tabs.length - 1], psql = $('ed_' + pt.id).value;
    G.check('it writes the call, IN as NULL and OUT as a variable read back after it',
      /CALL `?nobs_gui_menus`?\.`?p`?\(/.test(psql) && /NULL,\s+-- a int/i.test(psql) && /@b\s+-- b varchar\(20\) \(OUT\)/i.test(psql) && /SELECT @b;/.test(psql), psql);
    const run = await G.A('/api/script', { sql: psql.replace('NULL,', '21,'), db: DB });
    G.check('and it runs once a value is given', run.ok, run);
    closeTab(pt.id);
    objMenu(ev, DB, 'function', 'f');
    [...document.querySelectorAll('#ctx > .item')].find(x => own(x) === 'Call...').click();
    await G.until(() => tabs.length > n0, 5000);
    const ft = tabs[tabs.length - 1], fsql = $('ed_' + ft.id).value;
    G.check('a function is called in a SELECT', /^SELECT `?nobs_gui_menus`?\.`?f`?\(\s+NULL\s+-- x int\s+\);$/i.test(fsql), fsql);
    closeTab(ft.id);

    // the database list
    await loadSchemas(); const row = [...$('schemas').children].find(d => d.dataset && d.dataset.schema === DB);
    row.oncontextmenu({ ...ev }); const dm = shown();
    G.eq('database: drop last, on its own', dm.slice(-2), ['-', 'Drop database...']);
    $('ctx').style.display = 'none';
  } finally {
    window._cellClipboard = null;
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
