// What the server warns about is shown: a value cut short under a sql_mode without
// STRICT_TRANS_TABLES, a string read as a number in a SELECT, a Note from DROP TABLE IF EXISTS. And a
// grid edit the server would store otherwise than typed is not kept until the user says so.
(async () => {
  if (typeof setRunWarnings !== 'function') { G.skip('server warnings', 'this edition does not show the server\'s warnings yet'); return G.report(); }
  const DB = 'nobs_gui_warn';
  const run = async (i, sql) => { await runSql(i, sql); await G.until(() => !T(i).runningReqId, 20000); return T(i); };
  const shown = i => $('stw_' + i).innerText.trim();
  let i = null, oldMode = null;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.w (id INT PRIMARY KEY, v VARCHAR(3));
INSERT INTO ${DB}.w VALUES (1,'abc');`);
    i = openTab('warn', '', DB, false, null);

    const t = await run(i, `SET SESSION sql_mode = ''; INSERT INTO ${DB}.w VALUES (2, 'toolong')`);
    G.eq('a value cut short is counted beside the status', shown(i), '1 warning');
    G.check('with its code and statement', (t.warnings || []).some(w => w.code === 1265 && w.statement === 2), t.warnings);
    G.check('and it stands out', !!$('stw_' + i).querySelector('.warnlink'), $('stw_' + i).innerHTML);
    warnShow(i);
    G.check('the link lists it, statement and all', $('mWarn').classList.contains('show') && /1265/.test($('warnList').innerText) && /INSERT INTO/.test($('warnList').innerText), $('warnList').innerText.slice(0, 300));
    G.check('as a list, with nothing to apply', $('warnApply').style.display === 'none', $('warnApply').style.display);
    hide('mWarn');
    G.check('it is in the log too', G.logs.some(m => /1 warning[\s\S]*Warning 1265/.test(m)), G.logs.slice(-3));

    await run(i, "SELECT CAST('abc' AS SIGNED) AS n");
    G.eq('a SELECT\'s warning comes with its rows', shown(i), '1 warning');
    await run(i, `DROP TABLE IF EXISTS ${DB}.no_such_table`);
    G.eq('a Note is listed', shown(i), '1 note');
    G.check('but does not stand out', !!$('stw_' + i).querySelector('.notelink') && !$('stw_' + i).querySelector('.warnlink'), $('stw_' + i).innerHTML);
    await run(i, 'SELECT 1');
    G.eq('a run without any clears it', shown(i), '');

    // Apply. The app's own connections take the server's sql_mode, so it is made non-strict for this.
    oldMode = await G.one('SELECT @@GLOBAL.sql_mode');
    await G.q("SET GLOBAL sql_mode = ''");
    const g = await G.openTable(DB, 'w');
    const ri = g.rows.findIndex(r => String(r[0]) === '1'), vi = g.cols.indexOf('v');
    g.pending.upd[ri + ':' + vi] = 'toolong';
    const kept = await applyChanges(g.id);
    G.eq('an edit the server would cut short is not saved', [kept, await G.one(`SELECT v FROM ${DB}.w WHERE id = 1`)], [false, 'abc']);
    G.check('the warnings are shown, with Apply anyway', $('mWarn').classList.contains('show') && $('warnApply').style.display !== 'none' && /1265/.test($('warnList').innerText), $('warnList').innerText.slice(0, 300));
    G.eq('and the edit is still waiting', Object.keys(g.pending.upd).length, 1);
    await warnApplyAnyway();
    await G.until(() => !g.runningReqId && pendingCount(g) === 0, 20000);
    G.eq('Apply anyway stores it as the server takes it', await G.one(`SELECT v FROM ${DB}.w WHERE id = 1`), 'too');
    G.eq('and says so beside the status', shown(g.id), '1 warning');
    G.take();

    // An edit that fits gives no warning at all - the row guards' own "no data" is not one.
    g.pending.upd[g.rows.findIndex(r => String(r[0]) === '1') + ':' + vi] = 'ok';
    G.eq('an ordinary edit saves at once', [await applyChanges(g.id), await G.one(`SELECT v FROM ${DB}.w WHERE id = 1`)], [true, 'ok']);
    G.eq('with nothing to say', shown(g.id), '');
    closeTab(g.id);
  } finally {
    if (oldMode != null) await G.A('/api/query', { sql: `SET GLOBAL sql_mode = '${String(oldMode).replace(/'/g, "''")}'` });
    if (i != null && T(i)) closeTab(i);
    if ($('mWarn').classList.contains('show')) hide('mWarn');
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
