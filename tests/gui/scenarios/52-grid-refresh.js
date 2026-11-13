// The grid's Refresh button fetches the rows again with the query that produced them, and keeps the
// column filters, the search and the sort that were on them.
(async () => {
  const DB = 'nobs_gui_refresh';
  let t = null;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.r (id INT PRIMARY KEY, name VARCHAR(20)); INSERT INTO ${DB}.r VALUES (1,'apple'),(2,'banana'),(3,'apricot'),(4,'cherry');`);
    t = await G.openTable(DB, 'r');
    const ID = t.cols.indexOf('id'), NAME = t.cols.indexOf('name');
    G.check('the grid has a Refresh button', !!$('refbtn_' + t.id));

    setFilter(t.id, NAME, 'ap');
    sortBy(t.id, ID); sortBy(t.id, ID); // descending
    G.eq('filtered and sorted before', viewIndices(t.id).map(ri => t.rows[ri][ID]).map(String), ['3', '1']);

    await G.run(`INSERT INTO ${DB}.r VALUES (5,'grape'),(6,'apex'); UPDATE ${DB}.r SET name='avocado' WHERE id=1;`);
    await refreshGrid(t.id);

    G.eq('the new rows are fetched', t.rows.length, 6);
    G.eq('the filter is still on', t.filters[NAME], 'ap');
    G.eq('and still shows in its box', $('filterrow_' + t.id).querySelector('input[data-ci="' + NAME + '"]').value, 'ap');
    G.eq('the sort is still on', [t.sortCol, t.sortDir], [ID, -1]);
    G.eq('so the view is the fresh rows, filtered and sorted', viewIndices(t.id).map(ri => t.rows[ri][ID]).map(String), ['6', '3']);
    G.check('the header still shows the sort arrow', $('sortrow_' + t.id).textContent.includes('▼'));
  } finally {
    if (t && T(t.id)) { if (t.pending) t.pending.upd = {}; closeTab(t.id); }
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
