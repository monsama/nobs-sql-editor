// Apply, twice, in a tab with auto-commit off whose result has more rows than one page. The rows
// not read yet keep the tab's one connection busy, and the second Apply waited for it and failed
// after 15 seconds as "still busy with the statement before".
(async () => {
  const DB = 'nobs_gui_txpage';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.d (n INT PRIMARY KEY); INSERT INTO ${DB}.d VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9);
CREATE TABLE ${DB}.big (id INT PRIMARY KEY, note VARCHAR(40));
INSERT INTO ${DB}.big SELECT a.n*1000+b.n*100+c.n*10+e.n+1, 'x' FROM ${DB}.d a, ${DB}.d b, ${DB}.d c, ${DB}.d e WHERE a.n < 3;`);
    const t = await G.openTable(DB, 'big');
    txToggle(t.id, true);
    await openRun(t.id);
    await G.until(() => t.rows && t.rows.length > 0 && !t.runningReqId, 20000);
    G.check('the result has more rows than one page', t.hasMore === true, { rows: t.rows.length, hasMore: t.hasMore });
    const apply = async (row, v) => {
      t.pending.upd[t.rows.findIndex(r => String(r[0]) === String(row)) + ':1'] = v;
      const t0 = Date.now(); const ok = await applyChanges(t.id); await G.until(() => !t.runningReqId, 20000);
      return { ok, ms: Date.now() - t0, status: $('st_' + t.id).textContent };
    };
    const a1 = await apply(2, 'first');
    G.check('the first Apply goes through', a1.ok !== false && !/busy/i.test(a1.status), a1);
    const a2 = await apply(5, 'second');
    G.check('and so does the second, without waiting for the rows not read yet', a2.ok !== false && !/busy/i.test(a2.status) && a2.ms < 10000, a2);
    // The check reads the transaction's connection itself, as the app does: after letting the grid's
    // unread rows go.
    await sessFree(t);
    const inTx = await G.A('/api/query', { sql: `SELECT note FROM ${DB}.big WHERE id IN (2,5) ORDER BY id`, session: t.txSession });
    G.eq('both are in the transaction', (inTx.rows || []).map(r => r[0]), ['first', 'second']);

    // Commit with the result's unread rows still waiting: it used to wait for them and time out.
    await openRun(t.id); await G.until(() => t.rows && !t.runningReqId && t.hasMore, 20000);
    t.pending.upd[t.rows.findIndex(r => String(r[0]) === '9') + ':1'] = 'committed';
    const c0 = Date.now(); await txEnd(t.id, 'commit'); const cms = Date.now() - c0;
    const stored = await G.one(`SELECT note FROM ${DB}.big WHERE id=9`);
    G.check('Commit goes through at once, with the edit in it', stored === 'committed' && cms < 10000, { stored, ms: cms, status: $('st_' + t.id).textContent });
    closeTab(t.id);
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
