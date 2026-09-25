// A tab with auto-commit off keeps one transaction across its runs until Commit or Rollback, and
// nobody else sees what it has not committed. The same checks run against the PowerShell edition
// in its Live.Tests.ps1.
(async () => {
  const DB = 'nobs_gui_tx';
  const sess = 'tx_gui_' + Date.now();
  const on = (sql, extra) => G.A('/api/script', { sql, db: DB, session: sess, ...(extra || {}) });
  const inTab = async sql => { const r = await G.A('/api/query', { sql, db: DB, session: sess }); return r.ok ? r.rows : r.error; };
  const outside = async sql => (await G.q(sql))[0][0];
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.t (id INT PRIMARY KEY, v VARCHAR(10));
INSERT INTO ${DB}.t VALUES (1,'a');`);
    const first = await on("UPDATE t SET v='b' WHERE id=1; INSERT INTO t VALUES (2,'c')");
    G.eq('a script runs in the tab\'s transaction, saying how many rows it changed', [first.ok, first.affected], [true, 2]);
    G.eq('the next run sees what the tab has not committed', await inTab('SELECT id, v FROM t ORDER BY id'), [['1', 'b'], ['2', 'c']]);
    G.eq('another connection does not see it', await outside(`SELECT GROUP_CONCAT(v ORDER BY id) FROM ${DB}.t`), 'a');

    const bad = await on("INSERT INTO t VALUES (3,'d'); INSERT INTO t VALUES (1,'dup'); INSERT INTO t VALUES (4,'e')");
    G.check('a script stops at its first error and says which statement', !bad.ok && /Statement 2 of 3 failed/.test(bad.error), bad.error);
    G.eq('the transaction is still open after it', await inTab('SELECT GROUP_CONCAT(id ORDER BY id) FROM t'), [['1,2,3']]);

    const grid = await on("UPDATE t SET v='g' WHERE id=2 LIMIT 1;\nINSERT INTO t VALUES (1,'dup');", { transaction: true });
    G.check('a failed grid save is undone on its own, not the whole transaction',
      !grid.ok && JSON.stringify(await inTab('SELECT v FROM t WHERE id=2')) === '[["c"]]', grid.error);

    // A paged result borrows the tab's connection and gives it back. The PowerShell edition answers
    // a transaction's query in one piece rather than in pages.
    await on('INSERT INTO t SELECT seq, NULL FROM (SELECT 11 + a.d * 10 + b.d AS seq FROM (SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2) a, (SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) b) x');
    const page = await G.A('/api/query', { sql: 'SELECT id FROM t ORDER BY id', db: DB, session: sess, pageSize: 5 });
    G.check('a large result comes from the transaction', page.ok && (page.hasMore ? page.rows.length === 5 : page.rows.length > 30), page);
    if (page.cursorId) await G.A('/api/close-cursor', { cursorId: page.cursorId });
    G.eq('and the tab can run again after it', await inTab('SELECT COUNT(*) > 30 FROM t'), [['1']]);

    const rb = await G.A('/api/session-end', { session: sess, action: 'rollback' });
    G.check('Rollback throws it all away', rb.ok && await outside(`SELECT COUNT(*) FROM ${DB}.t`) === '1', rb);
    await on("UPDATE t SET v='z' WHERE id=1");
    const cm = await G.A('/api/session-end', { session: sess, action: 'commit' });
    G.check('Commit makes it permanent', cm.ok && await outside(`SELECT v FROM ${DB}.t WHERE id=1`) === 'z', cm);
    await on("UPDATE t SET v='lost' WHERE id=1");
    await G.A('/api/session-end', { session: sess, action: 'close' });
    await G.wait(300);
    G.eq('closing the tab rolls back what it had not committed', await outside(`SELECT v FROM ${DB}.t WHERE id=1`), 'z');

    // the toolbar: Commit and Rollback are always there, and usable with auto-commit off
    const i = openTab('tx', "UPDATE t SET v='q' WHERE id=1;", DB, false, null);
    const box = $('txac_' + i);
    G.check('a tab starts with auto-commit on', box && box.classList.contains('ison'), box && box.className);
    G.check('with Commit and Rollback in place but unavailable', $('txcommit_' + i).disabled && $('txrollback_' + i).disabled, 'enabled');
    box.click();
    G.check('pressing it turns it off', !box.classList.contains('ison') && box.getAttribute('aria-pressed') === 'false', box.className);
    G.check('turning it off makes Commit and Rollback available', !$('txcommit_' + i).disabled && !$('txrollback_' + i).disabled, 'still disabled');
    await runSql(i, $('ed_' + i).value);
    await G.until(() => T(i).txDirty, 5000);
    G.check('Commit shows there is something to commit', $('txcommit_' + i).classList.contains('go'), $('txcommit_' + i).className);
    G.eq('the count beside Commit says what is waiting', $('txlog_' + i).textContent, '1');
    txShowLog(i);
    const card = $('txLogList').querySelector('.citem');
    G.check('and opens it, statement and all', !!card && /UPDATE t SET v='q' WHERE id=1/.test(card.innerText), card ? card.innerText.slice(0, 200) : 'no card');
    G.check('with what came of it, and Commit and Rollback there', /1 row changed/.test(card.innerText) && !$('txLogCommit').disabled && !$('txLogRollback').disabled, card.innerText.slice(0, 200));
    await txLogEnd('rollback');
    G.check('Rollback clears it', !$('txcommit_' + i).classList.contains('go'), $('txcommit_' + i).className);
    G.eq('and empties the log', [$('txlog_' + i).textContent, $('txlog_' + i).disabled], ['0', true]);
    G.eq('and the change is gone', await outside(`SELECT v FROM ${DB}.t WHERE id=1`), 'z');
    closeTab(i);

    // Commit saves grid edits that were not applied yet, and commits them with the rest; Rollback
    // throws them away.
    const tt = await G.openTable(DB, 't');
    txToggle(tt.id, true);
    await openRun(tt.id);
    tt.pending.upd[tt.rows.findIndex(r => r[0] === '1') + ':1'] = 'viaCommit';
    updateEditBar(tt.id);
    G.check('grid edits waiting light Commit up', $('txcommit_' + tt.id).classList.contains('go'), $('txcommit_' + tt.id).className);
    G.eq('and Apply carries their count', $('edit_' + tt.id).querySelector('.applyn').textContent.trim(), 'Apply (1)');
    await txEnd(tt.id, 'commit');
    G.eq('Commit saves pending grid edits and commits them', await outside(`SELECT v FROM ${DB}.t WHERE id=1`), 'viaCommit');
    tt.pending.upd[tt.rows.findIndex(r => r[0] === '1') + ':1'] = 'dropped';
    await txEnd(tt.id, 'rollback');
    G.eq('Rollback discards them', [pendingCount(tt), await outside(`SELECT v FROM ${DB}.t WHERE id=1`)], [0, 'viaCommit']);
    closeTab(tt.id);
  } finally {
    await G.A('/api/session-end', { session: sess, action: 'close' });
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
