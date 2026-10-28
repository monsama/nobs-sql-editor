// A tab keeps its connection between runs, so what one run leaves - @variables, temporary tables,
// SET SESSION - is there for the next, as in the mysql client. Turning Auto-commit off and on again
// keeps it too; a large result read only in part does not hold up the next run; a connection the
// server dropped is opened again, and the user is told; and Settings can switch it all off.
(async () => {
  if (typeof runSessOf !== 'function') { G.skip('the kept connection', 'this edition does not keep a tab\'s connection yet'); return G.report(); }
  const DB = 'nobs_gui_keep';
  const val = t => (t.rows && t.rows[0] ? t.rows[0][0] : undefined);
  const run = async (i, sql) => { await runSql(i, sql); await G.until(() => !T(i).runningReqId, 20000); return T(i); };
  let i = null;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.t (id INT PRIMARY KEY, v VARCHAR(10));
INSERT INTO ${DB}.t VALUES (1,'a');`);
    setKeepConn(true);

    // The backend directly: one session, switched between committing each statement and a transaction.
    const sid = 'keep_gui_' + Date.now();
    const q = async (sql, tx) => { const r = await G.A('/api/query', { sql, db: DB, session: sid, sessionTx: tx }); return r.ok ? (r.rows[0] ? r.rows[0][0] : null) : r.error; };
    await q('SET @k = 5', false);
    G.eq('a kept connection commits each statement', await q('SELECT @@autocommit', false), '1');
    G.eq('and keeps its variables', await q('SELECT @k', false), '5');
    G.eq('asked for a transaction, it switches over', [await q('SELECT @@autocommit', true), await q('SELECT @k', true)], ['0', '5']);
    G.eq('and back, with what it held', [await q('SELECT @@autocommit', false), await q('SELECT @k', false)], ['1', '5']);
    await G.A('/api/session-end', { session: sid, action: 'close' });

    // A tab
    i = openTab('keep', '', DB, false, null);
    G.eq('one run sets a variable, the next reads it', val(await run(i, 'SET @nobs_x = 41; SELECT @nobs_x + 1')), '42');
    G.eq('across separate runs too', val(await run(i, 'SELECT @nobs_x')), '41');
    await run(i, 'CREATE TEMPORARY TABLE tmp_k (a INT); INSERT INTO tmp_k VALUES (7)');
    G.eq('a temporary table outlives the run that made it', val(await run(i, 'SELECT a FROM tmp_k')), '7');
    G.eq('so does SET SESSION', val(await run(i, "SET SESSION sql_mode = 'ANSI_QUOTES'; SELECT @@SESSION.sql_mode")), 'ANSI_QUOTES');
    G.eq('and it holds for the runs after',val(await run(i, 'SELECT @@SESSION.sql_mode')), 'ANSI_QUOTES');
    const plan = await tabQuery(T(i), { sql: 'EXPLAIN SELECT * FROM tmp_k', db: DB });
    G.check('Explain finds the temporary table', plan.ok, plan.error);

    // Auto-commit off makes the same connection the tab's transaction.
    txToggle(i, true);
    G.eq('with Auto-commit off the session is still there', val(await run(i, 'SELECT @nobs_x')), '41');
    await run(i, "UPDATE t SET v='tx' WHERE id=1");
    G.eq('the transaction is the tab\'s own', await G.one(`SELECT v FROM ${DB}.t WHERE id=1`), 'a');
    await txEnd(i, 'rollback');
    txToggle(i, false);
    await G.wait(300);
    G.eq('Auto-commit on again: the session is still there', val(await run(i, 'SELECT @nobs_x, @@autocommit')), '41');
    G.eq('committing each statement again', T(i).rows[0][1], '1');
    await run(i, "UPDATE t SET v='auto' WHERE id=1");
    G.eq('and a change is seen at once from outside', await G.one(`SELECT v FROM ${DB}.t WHERE id=1`), 'auto');

    // A large result read only in part holds the connection; the next run must not wait for the rest.
    const digits = '(SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9)';
    const big = await run(i, `SELECT a.d + b.d*10 + c.d*100 + e.d*1000 + f.d*10000 + g.d*100000 AS n, REPEAT('x', 200) AS pad FROM ${digits} a, ${digits} b, ${digits} c, ${digits} e, ${digits} f, ${digits} g`);
    G.check('a million rows come a page at a time', big.hasMore && !!big.cursorId, { hasMore: big.hasMore, rows: big.rows && big.rows.length });
    const t0 = Date.now();
    const after = await run(i, 'SELECT @nobs_x');
    G.check('the next run does not wait for the rest of them', Date.now() - t0 < 8000, Date.now() - t0 + ' ms');
    G.eq('and finds the session as it was', val(after), '41');
    G.eq('the run after that is not interrupted either', val(await run(i, 'SELECT 1 + 1')), '2');

    // The server drops the connection: the next run opens a new one, and says what was lost.
    const cid = val(await run(i, 'SELECT CONNECTION_ID()'));
    await G.q('KILL ' + cid);
    G.take();
    G.eq('after the server dropped it, the next run still works', val(await run(i, 'SELECT @nobs_x IS NULL')), '1');
    // Taken here: the notice is shown as a warning, which would otherwise count as a failure.
    const said = G.take();
    G.check('and says what earlier runs left is gone', said.t.concat(said.l).some(m => /connection had been closed/.test(m)), said);

    // The tab's menu starts it afresh.
    await run(i, 'SET @nobs_y = 1');
    tabSessClose(T(i));
    G.eq('a new connection for the tab starts empty', val(await run(i, 'SELECT @nobs_y IS NULL')), '1');

    // Switched off in Settings, every run starts afresh, as before.
    await run(i, 'SET @nobs_z = 1');
    setKeepConn(false);
    G.eq('with keeping off, a variable does not reach the next run', val(await run(i, 'SELECT @nobs_z IS NULL')), '1');
    setKeepConn(true);

    // Closing the tab lets the connection go.
    await run(i, 'SET @nobs_w = 1');
    const last = val(await run(i, 'SELECT CONNECTION_ID()'));
    closeTab(i); i = null;
    await G.wait(600);
    const still = await G.one(`SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = ${last}`);
    G.eq('closing the tab closes its connection', still, '0');
  } finally {
    if (i != null && T(i)) closeTab(i);
    setKeepConn(true);
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
