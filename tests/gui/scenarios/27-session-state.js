// A script that ends in one SELECT ran in two steps, on two connections, and what belongs to a
// connection did not carry over: ROW_COUNT() and LAST_INSERT_ID() of the statements before, user
// variables, temporary tables. Such a script now runs on one connection.
(async () => {
  const DB = 'nobs_gui_session';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.t (id INT AUTO_INCREMENT PRIMARY KEY, v INT);
INSERT INTO ${DB}.t (v) VALUES (1),(2),(3);`);

    let t = await G.runIn('UPDATE t SET v = v + 1 WHERE v >= 2;\nSELECT ROW_COUNT() AS n;', DB);
    G.eq('ROW_COUNT() sees the UPDATE before it', G.rowsOf(t), ['2']);

    t = await G.runIn('INSERT INTO t (v) VALUES (9);\nSELECT LAST_INSERT_ID() AS id;', DB);
    G.eq('LAST_INSERT_ID() sees the INSERT before it', G.rowsOf(t), ['4']);

    t = await G.runIn('SET @x := 5;\nSELECT @x AS x;', DB);
    G.eq('a user variable set before the SELECT is there', G.rowsOf(t), ['5']);

    t = await G.runIn('CREATE TEMPORARY TABLE tmp AS SELECT 7 AS s;\nSELECT s FROM tmp;', DB);
    G.eq('so is a temporary table', G.rowsOf(t), ['7']);

    // An ordinary script still gets the paged, editable grid of its SELECT.
    t = await G.runIn('UPDATE t SET v = v;\nSELECT * FROM t;', DB);
    G.check('a script without session state still binds its SELECT to the table', t.table === 't' && !!t.pk, [t.table, t.pk]);
    G.take();
  } finally {
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
