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
