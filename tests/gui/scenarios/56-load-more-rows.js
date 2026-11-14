// Beside the row count: "Load all rows" reads the rest of the result, and "Load more" the number
// typed beside it (10,000 to begin with) - exactly that many, from the same open result.
(async () => {
  const DB = 'nobs_gui_loadmore';
  // 0-9, for counting up to 3500 rows on servers without WITH RECURSIVE (MySQL 5.7).
  const D = '(SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9)';
  let t = null;
  const logs = [];
  const origLog = log;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.big (id INT PRIMARY KEY, v VARCHAR(20)); INSERT INTO ${DB}.big SELECT n, CONCAT('v',n) FROM (SELECT a.d+b.d*10+c.d*100+e.d*1000+1 AS n FROM ${D} a, ${D} b, ${D} c, ${D} e) x WHERE n<=3500;`);
    t = await G.openTable(DB, 'big');
    log = (m, ...a) => { logs.push(String(m)); return origLog(m, ...a); };
    G.eq('the first page is read', [t.rows.length, t.hasMore], [1000, true]);
    const bar = $('stla_' + t.id);
    G.check('the buttons are there', bar && /Load all rows/.test(bar.textContent) && /Load more/.test(bar.textContent), bar && bar.innerHTML);
    G.eq('the number starts at 10,000', $('stlan_' + t.id).value, '10000');
    G.eq('the box, then Load more, then Load all rows', [...bar.children].map(e => e.tagName === 'INPUT' ? 'box' : e.textContent), ['box', 'Load more', 'Load all rows']);
    G.eq('a plain text box, with no up and down arrows', $('stlan_' + t.id).type, 'text');

    $('stlan_' + t.id).value = '1500'; $('stlan_' + t.id).dispatchEvent(new Event('input'));
    await loadMoreRows(t.id);
    G.eq('Load more reads exactly that many more', [t.rows.length, t.hasMore], [2500, true]);
    G.check('without an error about the result', !logs.some(m => /ERROR|not found|closed/i.test(m)), logs);
    G.eq('and the number stays as typed', $('stlan_' + t.id).value, '1500');

    $('stlan_' + t.id).value = 'abc';
    const nt = G.toasts.length;
    await loadMoreRows(t.id);
    G.eq('a number that is not one reads nothing', t.rows.length, 2500);
    // and says so - a warning this test asked for, so not one of the errors checked at the end
    const said = G.toasts.splice(nt);
    G.check('and says what to type', said.length === 1 && /whole number/.test(said[0]), said);

    await loadAllRows(t.id);
    G.eq('Load all rows reads the rest', [t.rows.length, t.hasMore], [3500, false]);
    G.check('again without an error', !logs.some(m => /ERROR|not found|closed/i.test(m)), logs);
    G.eq('and the buttons go once every row is in', bar.innerHTML, '');
    G.eq('every row once, in order', [t.rows[0][0], t.rows[2499][0], t.rows[3499][0]].map(String), ['1', '2500', '3500']);
  } finally {
    log = origLog;
    if (t && T(t.id)) closeTab(t.id);
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
