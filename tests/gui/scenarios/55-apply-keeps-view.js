// After Apply the rows are read again with the column filters and the sort kept: a table tab reads
// its table again, a SELECT of your own runs again as written, and a filter on a column the new
// rows no longer have goes with it.
(async () => {
  const DB = 'nobs_gui_applyview';
  const ids = () => viewIndices(t.id).map(ri => String(t.rows[ri][t.cols.indexOf('id')]));
  let t = null, q = null;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.a (id INT PRIMARY KEY, name VARCHAR(20), grp INT, note VARCHAR(20)); INSERT INTO ${DB}.a VALUES (1,'apple',1,'x'),(2,'banana',1,'y'),(3,'apricot',2,'x'),(4,'avocado',1,'x');`);

    // A table tab: filtered and sorted, a cell changed and applied.
    t = await G.openTable(DB, 'a');
    const NAME = t.cols.indexOf('name'), ID = t.cols.indexOf('id');
    setFilter(t.id, NAME, 'ap');
    sortBy(t.id, ID); sortBy(t.id, ID); // descending
    G.eq('a table tab, filtered and sorted', ids(), ['3', '1']);
    setUpd(t.id, t.rows.findIndex(r => String(r[ID]) === '2'), NAME, 'grape');
    G.check('the change applies', await applyChanges(t.id));
    G.eq('the change is in the rows read again', t.rows.find(r => String(r[ID]) === '2')[NAME], 'grape');
    G.eq('the filter is still on', t.filters[NAME], 'ap');
    G.eq('and still shows in its box', $('filterrow_' + t.id).querySelector('input[data-ci="' + NAME + '"]').value, 'ap');
    G.eq('the sort too', [t.sortCol, t.sortDir], [ID, -1]);
    G.eq('so the view is the new rows, filtered and sorted', ids(), ['3', '2', '1']);

    // A SELECT of your own: its columns, WHERE and ORDER BY stay, rather than the whole table.
    const SQL = `SELECT id, name FROM ${DB}.a WHERE grp = 1 ORDER BY id DESC`;
    const qi = openTab('own select', SQL, DB, false);
    q = T(qi);
    await runSql(qi, SQL);
    await G.until(() => q.pk && q.rows && q.rows.length === 3, 20000);
    G.eq('a SELECT of your own can be edited', q.cols, ['id', 'name']);
    setFilter(qi, 1, 'a');
    setUpd(qi, q.rows.findIndex(r => String(r[0]) === '4'), 1, 'avocado2');
    G.check('its change applies', await applyChanges(qi));
    G.eq('and the same SELECT ran again', [q.curRun, $('ed_' + qi).value.trim()], [SQL, SQL]);
    G.eq('with its own columns, not the whole table', q.cols, ['id', 'name']);
    G.eq('its WHERE and ORDER BY', q.rows.map(r => String(r[0])), ['4', '2', '1']);
    G.eq('the change is in it', q.rows[0][1], 'avocado2');
    G.eq('and its filter is kept', q.filters[1], 'a');

    // A filter on a column the rows no longer have goes; the others stay.
    const NOTE = t.cols.indexOf('note');
    setFilter(t.id, NOTE, 'x');
    await G.run(`ALTER TABLE ${DB}.a DROP COLUMN note`);
    setUpd(t.id, t.rows.findIndex(r => String(r[ID]) === '1'), NAME, 'apple2');
    G.check('a change applies after a column was dropped', await applyChanges(t.id));
    G.check('the dropped column is gone from the grid', !t.cols.includes('note'), t.cols);
    G.eq('and its filter with it, the other kept', Object.entries(t.filters).filter(([, v]) => v).map(([ci, v]) => t.cols[ci] + '=' + v), ['name=ap']);
  } finally {
    for (const x of [t, q]) if (x && T(x.id)) { if (x.pending) x.pending.upd = {}; closeTab(x.id); }
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
