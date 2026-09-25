// "Go to referenced row" opens that row, and the quick filter finds the right rows - with an empty
// binary key and a text key that looks like hex.
(async () => {
  const DB = 'nobs_gui_fk';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.p (id VARBINARY(4) PRIMARY KEY, label VARCHAR(10));
INSERT INTO ${DB}.p VALUES (X'', 'empty'), (0x41, 'A'), (0x42, 'B');
CREATE TABLE ${DB}.tp (code VARCHAR(8) PRIMARY KEY, label VARCHAR(10));
INSERT INTO ${DB}.tp VALUES ('0x41', 'hex text'), ('A', 'letter A');`);
    const follow = async (table, col, val) => {
      await goToFkRow(DB, table, [[col, val]]);
      const t = tabs[tabs.length - 1];
      await G.until(() => t.rows && !t.runningReqId);
      return G.rowsOf(t);
    };
    G.eq('the referenced row, by a binary key', await follow('p', 'id', '0x41'), ['0x41|A']);
    G.eq('the referenced row, by an empty binary key', await follow('p', 'id', '0x'), ['0x|empty']);
    G.eq('a text key that looks like hex is text', await follow('tp', 'code', '0x41'), ['0x41|hex text']);

    const t = await G.openTable(DB, 'p');
    const ci = t.cols.indexOf('id');
    const pick = async (value, op) => {
      const sub = qfSub(t.id, 'id', value);
      await sub.find(x => Array.isArray(x) && x[0].includes(op))[1]();
      await G.until(() => !t.runningReqId); await G.wait(300);
      return G.rowsOf(t).sort();
    };
    G.eq('quick filter "=" on an empty binary value', await pick(t.rows[t.rows.findIndex(r => r[0] === '0x')][ci], ' = '), ['0x|empty']);
    await clearFilters(t.id); await G.until(() => !t.runningReqId);
    G.eq('quick filter "!=" on a binary value', await pick(t.rows[t.rows.findIndex(r => G.hex(r[0]) === '0x41')][ci], ' != '), ['0x42|B', '0x|empty']);

    // LIKE with a backslash, a percent sign and an underscore in the value: each is itself, not an
    // escape or a wildcard. With the backslash escaped only once, the filter matched nothing.
    await G.A('/api/script', { sql: `CREATE TABLE ${DB}.w (id INT PRIMARY KEY, s VARCHAR(20)); INSERT INTO ${DB}.w VALUES (1, 'C:\\\\temp\\\\x'), (2, 'C:tempx'), (3, '50%_off'), (4, '50xyoff');` });
    const tw = await G.openTable(DB, 'w');
    const like = async (value, which) => {
      const sub = qfSub(tw.id, 's', value);
      await sub.filter(x => Array.isArray(x) && / LIKE '/.test(x[0]))[which][1]();
      await G.until(() => !tw.runningReqId); await G.wait(300);
      const got = G.rowsOf(tw).map(r => r.split('|')[0]).sort();
      await clearFilters(tw.id); await G.until(() => !tw.runningReqId);
      return got;
    };
    G.eq('LIKE finds a value with backslashes, and only it', await like('C:\\temp\\x', 0), ['1']);
    G.eq('and % and _ in a value are not wildcards', await like('50%_off', 0), ['3']);
  } finally {
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
