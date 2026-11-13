// Opening a table that already has a tab shows that tab instead of adding another, and closing the
// shown tab goes back to the tab shown before it rather than to the one beside it.
(async () => {
  const DB = 'nobs_gui_tabreuse';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.a (id INT PRIMARY KEY); CREATE TABLE ${DB}.b (id INT PRIMARY KEY);`);

    const a = await G.openTable(DB, 'a');
    const n = tabs.length;
    const q = openTab('Query', '', DB, false, null);
    objOpen(DB, 'table', 'a');
    G.eq('opening the table again adds no tab', tabs.length, n + 1);
    G.eq('and shows its tab', activeTab, a.id);

    // a is shown; open b at the end, then close it: back to a, not to the query tab beside it.
    const b = await G.openTable(DB, 'b');
    G.eq('b is the last tab', tabs[tabs.length - 1].id, b.id);
    closeTab(b.id);
    G.eq('closing it shows the tab shown before it', activeTab, a.id);

    closeTab(q); closeTab(a.id);
    G.eq('and the app said nothing was wrong', G.errs(), []);
  } finally {
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
