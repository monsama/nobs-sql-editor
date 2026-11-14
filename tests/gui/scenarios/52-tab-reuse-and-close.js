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

    // A double-click on the tab bar's empty space opens a new tab; on a tab it opens none.
    const bar = $('tabsbar'), n0 = tabs.length;
    bar.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    G.eq('a double-click beside the tabs opens a new one', tabs.length, n0 + 1);
    const nt = tabs[tabs.length - 1].id;
    G.eq('and shows it', activeTab, nt);
    $('tabbtn_' + a.id).dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    G.eq('a double-click on a tab opens none', tabs.length, n0 + 1);
    closeTab(nt);

    // More tabs than fit in a row wrap onto more rows, rather than scrolling out of sight.
    const many = [];
    for (let i = 0; i < 30; i++) many.push(openTab('wrapping tab ' + i, '', DB, false, null));
    const first = $('tabbtn_' + many[0]), last = $('tabbtn_' + many[many.length - 1]);
    G.check('they wrap onto another row', last.offsetTop > first.offsetTop, [first.offsetTop, last.offsetTop]);
    G.check('and none is out of sight sideways', bar.scrollWidth <= bar.clientWidth + 1, [bar.scrollWidth, bar.clientWidth]);
    many.forEach(id => closeTab(id));
    G.check('closed again, the bar is one row high', bar.offsetHeight < 50, bar.offsetHeight);

    closeTab(q); closeTab(a.id);
    G.eq('and the app said nothing was wrong', G.errs(), []);
  } finally {
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
