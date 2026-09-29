// Smaller things in the editor and the tabs: the quick filter keeps the undo history, Close others
// asks about a transaction or a routine that was not applied, and accepting a suggestion after a
// typed backtick leaves one quoted name.
(async () => {
  const DB = 'nobs_gui_small';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.t (id INT PRIMARY KEY, s VARCHAR(10)); INSERT INTO ${DB}.t VALUES (1,'a'),(2,'b');`);
    const t = await G.openTable(DB, 't');
    const ed = $('ed_' + t.id), before = ed.value;
    await addFilterClause(t.id, '`id` = 1');
    await G.until(() => !t.runningReqId);
    G.check('the quick filter writes its SQL', /WHERE `id` = 1/.test(ed.value), ed.value);
    G.check('which is the tab\'s own, not an edit', !t.sqlEdited, t.sqlEdited);
    ed.focus(); document.execCommand('undo');
    G.eq('and Ctrl+Z brings the text before it back', ed.value, before);
    await clearFilters(t.id); await G.until(() => !t.runningReqId);

    // Another query in the tab - loaded, or typed over the generated one - takes the filters with it.
    await addFilterClause(t.id, '`id` = 1'); await G.until(() => !t.runningReqId);
    await addFilterClause(t.id, "`s` = 'a'"); await G.until(() => !t.runningReqId);
    G.eq('two quick filters are on', t.filterClauses.length, 2);
    // Each is a block above the grid, taken off on its own with its x.
    const bar = $('qf_' + t.id), chips = () => [...bar.querySelectorAll('.qfchip')];
    G.check('both show as blocks above the grid', bar.style.display !== 'none' && chips().length === 2, chips().length);
    chips()[0].querySelector('.qfx').click(); await G.until(() => !t.runningReqId && t.filterClauses.length === 1);
    G.eq('its x takes off that one filter', t.filterClauses, ["`s` = 'a'"]);
    G.check('and the query keeps the other', /WHERE `s` = 'a'/.test(ed.value) && !/`id` = 1/.test(ed.value), ed.value);
    G.eq('one block is left', chips().length, 1);
    saveSession('nobs_gui_small');
    const saved = (JSON.parse(localStorage.getItem('session:nobs_gui_small') || '[]').find(x => x.table === 't' && x.db === DB) || {}).filters;
    localStorage.removeItem('session:nobs_gui_small');
    G.eq('the tab is saved with the filter left, for the next start', saved, ["`s` = 'a'"]);
    await addFilterClause(t.id, '`id` = 1'); await G.until(() => !t.runningReqId);
    edSetAll(t.id, `SELECT id FROM ${DB}.t ORDER BY id DESC`);
    G.eq('a query loaded into the tab clears them', t.filterClauses.length, 0);
    G.check('and their blocks', bar.style.display === 'none', bar.style.display);
    await openRun(t.id); await G.until(() => !t.runningReqId);
    await addFilterClause(t.id, '`id` = 2'); await G.until(() => !t.runningReqId);
    ed.value = 'SELECT 1'; ed.dispatchEvent(new Event('input'));
    G.eq('and so does typing another', t.filterClauses.length, 0);
    await openRun(t.id); await G.until(() => !t.runningReqId);
    G.check('the next run is the whole table again', !/WHERE/.test(ed.value), ed.value);

    // accepting a suggestion after a backtick
    const q = openTab('q', 'SELECT * FROM `cus', DB, false);
    const qe = $('ed_' + q);
    qe.focus(); qe.setSelectionRange(qe.value.length, qe.value.length);
    acItems = ['customers']; acIdx = 0; acTa = null;
    acAccept(q);
    G.eq('a plain name replaces the backtick begun before it', qe.value, 'SELECT * FROM customers');
    qe.value = 'SELECT * FROM `my t'; qe.setSelectionRange(qe.value.length, qe.value.length);
    acItems = ['`my table`']; acIdx = 0; acAccept(q);
    G.eq('a quoted name is quoted once', qe.value, 'SELECT * FROM `my table`');

    // Close others asks about work that is not saved in the tabs it would close
    const other = tabs.find(x => x.id === t.id);
    other.txDirty = true;
    const realAsk = ask; let asked = '';
    ask = async m => { asked = m; return false; };
    try { await closeOthers(q); } finally { ask = realAsk; other.txDirty = false; }
    G.check('Close others asks about a transaction not committed, and keeps the tabs when told no', /transaction not committed/.test(asked) && !!T(t.id), asked);
    closeTab(q); closeTab(t.id);
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
