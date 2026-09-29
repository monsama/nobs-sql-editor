// A saved connection's time limit: it stops a statement run from the editor and says which limit
// did, and a tab's transaction - whose connection stays open between runs - takes the limit each run
// is sent with, none included, instead of keeping the one it was opened with.
(async () => {
  const realLimit = timeLimitSec;
  let limit = 1;
  timeLimitSec = () => limit;
  // A SELECT that runs for minutes on any server: a SLEEP is no good, as MySQL and MariaDB before 10.3
  // end one early without an error, and three collations tables crossed took under half a second on
  // MySQL 5.7. Four is two billion rows; the limit stops it, and it is cancelled if that failed.
  const maria = !!window.mariadb, v = maria ? 'max_statement_time' : 'max_execution_time';
  const slow = 'SELECT COUNT(*) FROM information_schema.COLLATIONS a, information_schema.COLLATIONS b, information_schema.COLLATIONS c, information_schema.COLLATIONS d';
  const one = async (id, sql) => { await runSql(id, sql); await G.until(() => !T(id).runningReqId, 30000); return T(id).rows && T(id).rows[0] ? String(T(id).rows[0][0]) : null; };
  let p = null, x = null;
  try {
    p = openTab('limit', slow, null, false);
    // Not awaited: should the limit not work, it is cancelled after half a minute instead of running on.
    const run = runSql(p, slow);
    await G.until(() => T(p).runningReqId, 5000); await G.until(() => !T(p).runningReqId, 30000);
    if (T(p).runningReqId) await cancelQuery(p);
    await run;
    const st = $('st_' + p).textContent;
    G.check('a statement over the limit is stopped', /\b(3024|1969)\b/.test(st) && /exceeded/i.test(st), st);
    G.check('and the message names the connection\'s limit', /time limit \(1 s\) stopped it/.test(st), st);
    closeTab(p); p = null;

    x = openTab('limit tx', 'SELECT 1', null, false);
    txToggle(x, true);
    const a = await one(x, 'SELECT @@SESSION.' + v);
    G.check('in a transaction the run carries the limit', maria ? +a === 1 : +a === 1000, a);
    limit = 0;
    const b = await one(x, 'SELECT @@SESSION.' + v);
    G.eq('and taken away, the next run in the same transaction has none', +b, 0);
    limit = 2;
    const c = await one(x, 'SELECT @@SESSION.' + v);
    G.check('and a new one applies at once', maria ? +c === 2 : +c === 2000, c);
    await txEnd(x, 'rollback');
    closeTab(x); x = null;
  } finally {
    timeLimitSec = realLimit;
    if (x) { try { await txEnd(x, 'rollback'); } catch (e) {} closeTab(x); }
    if (p) closeTab(p);
  }
  return G.report();
})()
