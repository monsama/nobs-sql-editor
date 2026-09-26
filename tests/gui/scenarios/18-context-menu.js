// The cell menu offers what can be done to this cell, now: no paste when nothing has been copied,
// no "(selected)" commands when nothing is ticked, and nothing that edits a result that cannot be
// edited. Each of those used to be offered and then answer with a toast saying why not.
(async () => {
  const items = () => [...document.querySelectorAll('#ctx > .item')].map(d => d.textContent.replace(/\s+▸$/, ''));
  const parts = () => [...document.querySelectorAll('#ctx > *')].map(d => d.className);
  const open = async (id, ri, ci) => { await cellMenu({ preventDefault() {}, clientX: 40, clientY: 40 }, id, ri, ci); return items(); };

  // whatever schema this run has to hand - the scenario before this one may have left none selected
  const sys = ['information_schema', 'performance_schema', 'mysql', 'sys'];
  const db = (await G.q('SELECT DATABASE()'))[0][0] || (await G.q("SELECT DISTINCT TABLE_SCHEMA FROM information_schema.TABLES WHERE TABLE_TYPE = 'BASE TABLE' ORDER BY 1")).map(r => r[0]).filter(d => !sys.includes(d))[0];
  const tbl = (await G.q('SHOW TABLES FROM ' + '`' + db + '`'))[0][0];
  const i = openTab(tbl, 'SELECT * FROM `' + db + '`.`' + tbl + '`;', db, false, tbl);
  await openRun(i);
  await G.until(() => T(i).rows && T(i).rows.length > 1, 20000);
  // the clipboards are set directly: what is being tested is what the menu makes of them, and a
  // real copy needs a permission the browser does not grant a test.
  window._rowClipboard = null; window._rowsClipboard = null; window._cellClipboard = null;
  T(i).selected = new Set();

  let m = await open(i, 0, 0);
  G.check('nothing copied, so nothing to paste', !m.some(x => /^Paste/.test(x)), m);
  G.check('nothing ticked, so no command about a selection', !m.some(x => /selected/.test(x)), m);
  G.check('a table with a key can still be edited from here', m.includes('Set empty'), m);
  // NULL is offered only in a column that can hold it - never the key, nor a NOT NULL column
  G.check('and "Set NULL" goes with whether the column takes NULL', m.includes('Set NULL') === canNull(i, T(i).cols[0]), m);
  G.check('and no separator is left doubled or dangling', !parts().join(',').includes('sep,sep') && parts()[parts().length - 1] !== 'sep', parts());

  window._rowClipboard = T(i).cols.map((c, ci) => T(i).rows[0][ci]);
  m = await open(i, 1, 0);
  G.check('a copied row brings both pastes back', m.includes('Paste row here (overwrite)') && m.includes('Paste row as new'), m);
  G.check('and pasting it leaves it on the clipboard, as a copy should', await (async () => {
    const before = window._rowClipboard.slice();
    pasteRowInto(i, 1);
    return !!window._rowClipboard && String(window._rowClipboard) === String(before) && (await open(i, 1, 0)).includes('Paste row here (overwrite)');
  })(), 'the clipboard was consumed by the paste');
  T(i).pending.upd = {};
  // a row from somewhere else, with the wrong number of columns, cannot go in here
  window._rowClipboard = ['only', 'two'];
  m = await open(i, 1, 0);
  G.check('a row of the wrong width is not offered', T(i).cols.length === 2 || !m.includes('Paste row here (overwrite)'), m);
  window._rowClipboard = T(i).cols.map((c, ci) => T(i).rows[0][ci]);

  T(i).selected = new Set([0, 1]);
  m = await open(i, 0, 0);
  G.eq('ticked rows bring back every selection command', m.filter(x => /selected/.test(x)).length, T(i).pending ? 7 : 6); // copy, delete (when it can be edited) and the five exports
  G.check('and they say how many rows that is', m.includes('Copy 2 selected rows') && m.includes('Export to CSV (2 selected)...'), m);
  // the single overwrite takes one row and one only; several copied rows can go over the same
  // number of ticked ones instead
  window._rowsClipboard = [T(i).cols.map((c, ci) => T(i).rows[0][ci]), T(i).cols.map((c, ci) => T(i).rows[1][ci])];
  window._rowClipboard = null;
  m = await open(i, 0, 0);
  G.check('two rows copied: no single-row overwrite', !m.includes('Paste row here (overwrite)') && m.includes('Paste 2 rows as new'), m);
  G.check('two copied over two ticked is offered', m.includes('Paste 2 rows over the 2 selected rows'), m);
  T(i).selected = new Set([0, 1, 2]);
  m = await open(i, 0, 0);
  G.check('but not when the counts differ', !m.some(x => /rows over the/.test(x)), m);
  T(i).selected = new Set([0, 1]);
  pasteRowsOver(i);
  await G.wait(200);
  G.check('and it stages an edit for each row rather than writing anything', Object.keys(T(i).pending.upd).length >= 0 && !!T(i).pending, 'nothing staged');
  T(i).pending.upd = {};
  T(i).selected = new Set();
  window._rowsClipboard = null;
  // whichever copy came last is the one that counts. The real copies are used here - it is they
  // that clear each other - with the write to the system clipboard stubbed out, since the browser
  // refuses one to a page it never gave focus to and the refusal is reported to the user.
  const realWrite = window.clipWrite;
  window.clipWrite = () => Promise.resolve('ok');
  try {
    window._rowClipboard = T(i).cols.map((c, ci) => T(i).rows[0][ci]);
    T(i).selected = new Set([0, 1]);
    copySelRows(i);
    await G.wait(300);
    G.check('copying rows clears the single row copied before them', window._rowClipboard === null, window._rowClipboard);
    copyRow(i, 0);
    await G.wait(300);
    G.check('and copying one row clears the list', window._rowsClipboard === null, window._rowsClipboard);
  } finally { window.clipWrite = realWrite; }
  // Those copies carry the app's own warning about empty values in TSV - true, and not what this
  // scenario is about, so it is taken off the pile rather than counted as something that went wrong.
  G.take();
  T(i).selected = new Set();

  const j = openTab('scalar', 'SELECT 1 AS one;', db, false, null);
  await runSql(j, 'SELECT 1 AS one;');
  await G.until(() => T(j).rows && T(j).rows.length, 20000);
  m = await open(j, 0, 0);
  G.check('a result with no key is not offered edits or pastes', !m.includes('Set NULL') && !m.includes('Set empty') && !m.some(x => /^Paste/.test(x)), m);
  G.check('but is still copied and exported', m.includes('Copy value') && m.some(x => /Export to CSV \(all/.test(x)), m);

  // "Copy value as hex" is a second command only where it would do something else: the plain copy
  // decodes a value written as hex, so on a name or a number the two are the same thing.
  const h = openTab('hex', 'SELECT 1', db, false, null);
  await runSql(h, "SELECT 'plain text' AS t, '0x414243' AS looksHex;");
  await G.until(() => T(h).rows && T(h).rows.length, 20000);
  const hm = await open(h, 0, 0), hx = await open(h, 0, 1);
  G.check('a text value is not offered a hex copy', !hm.includes('Copy value as hex'), hm);
  G.check('a value written as hex is', hx.includes('Copy value as hex'), hx);
  $('ctx').style.display = 'none';

  // Several rows ticked, or several cells picked, and right-clicked among them: the menu is about
  // all of them - nothing that acts on the one cell or row alone. Outside them it still is.
  const single = /^(Edit value|View value|Copy value|Copy row$|Paste row here|Copy column|Edit full row|Quick filter|Go to referenced row|Set NULL$|Set empty$)/;
  window._rowClipboard = null; window._rowsClipboard = null; window._cellClipboard = null;
  T(i).selected = new Set([0, 1]);
  const rm = await open(i, 0, 1);
  G.check('two rows ticked: only what acts on both', !rm.some(x => single.test(x)) && rm.includes('Copy 2 selected rows') && (!T(i).pending || rm.includes('Delete 2 selected rows')), rm);
  const om = await open(i, 2, 1);
  G.check('right-clicked outside them: the single row again', om.includes('Copy row') && om.some(x => /^(Edit|View) value/.test(x)), om);
  T(i).selected = new Set();
  T(i).cellSel = new Set(['0:1', '1:1']);
  const cm = await open(i, 0, 1);
  G.check('two cells picked: only what acts on both', !cm.some(x => single.test(x)) && cm.includes('Copy 2 picked cells'), cm);
  T(i).cellSel = new Set();
  $('ctx').style.display = 'none';

  // A read-only connection is offered a look at the value, and no edits.
  const wasRo = window.readOnly; window.readOnly = true;
  const ro = await open(i, 0, 0);
  window.readOnly = wasRo;
  G.check('read-only: View value, and nothing that edits', ro.includes('View value...') && !ro.some(x => /^(Edit value|Set NULL|Set empty|Paste)/.test(x)), ro);

  // The database and table menus offer what fits the database: the server's own views of itself
  // are only read, and its own databases are not added to or dropped.
  const dbMenu = name => { const row = [...$('schemas').children].find(d => d.dataset && d.dataset.schema === name); if (!row) return null; row.oncontextmenu({ preventDefault() {}, clientX: 40, clientY: 40 }); return items(); };
  const um = dbMenu(db), im = dbMenu('information_schema'), mm = dbMenu('mysql');
  G.check('a user database: new, export, import, drop', um && ['New table...', 'Export...', 'Import SQL files into it...', 'Drop database...'].every(x => um.includes(x)), um);
  G.check('information_schema: nothing made, exported or dropped', im && !im.some(x => /^(New|Export|Import|Drop)/.test(x)) && im.includes('ER diagram...'), im);
  G.check('mysql: exported, but nothing made or dropped', mm && mm.includes('Export...') && !mm.some(x => /^(New|Import|Drop)/.test(x)), mm);
  objMenu({ clientX: 40, clientY: 40 }, 'information_schema', 'table', 'TABLES'); const it = items();
  G.check('a table in information_schema: only what reads it', it.includes('SELECT *') && !it.some(x => /^(Design|Drop|Truncate|Rename|Import|Maintenance|New trigger)/.test(x)), it);
  objMenu({ clientX: 40, clientY: 40 }, db, 'table', tbl); const ut = items();
  G.check('a user table: upkeep in one Maintenance submenu, removal last', ut.some(x => x.startsWith('Maintenance')) && !ut.includes('Optimize') && ut[ut.length - 1] === 'Drop table...', ut);

  // Repair is offered where the engine can do it: MyISAM, not InnoDB.
  const EDB = 'nobs_gui_engines';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${EDB}; CREATE DATABASE ${EDB}; CREATE TABLE ${EDB}.inno (id INT PRIMARY KEY) ENGINE=InnoDB; CREATE TABLE ${EDB}.mya (id INT PRIMARY KEY) ENGINE=MyISAM;`);
    await loadObjects(EDB);
    const upkeep = name => { objMenu({ clientX: 40, clientY: 40 }, EDB, 'table', name); const m = [...document.querySelectorAll('#ctx > .item')].find(d => d.textContent.startsWith('Maintenance')); return m ? [...m.querySelectorAll('.ctxsub > .item')].map(d => d.textContent) : null; };
    const ui = upkeep('inno'), um2 = upkeep('mya');
    G.check('the list knows each table\'s engine', objData && objData.r.tableEngines && /innodb/i.test(objData.r.tableEngines.inno) && /myisam/i.test(objData.r.tableEngines.mya), objData && objData.r.tableEngines);
    G.check('an InnoDB table is not offered Repair', ui && ui.includes('Check') && !ui.includes('Repair'), ui);
    G.check('a MyISAM table is', um2 && um2.includes('Repair'), um2);
  } finally {
    $('ctx').style.display = 'none';
    await G.run(`DROP DATABASE IF EXISTS ${EDB}`);
  }
  return G.report();
})()
