// The focused cell's row and column name are marked, more quietly than the cell's own ring, and a row
// whose checkbox is ticked is marked like the focused row. A read-only grid (a join) is marked too.
(async () => {
  if (typeof gridMarkFocus !== 'function') { G.skip('focus marks', 'this edition does not mark the focused row yet'); return G.report(); }
  const DB = 'nobs_gui_focusmark';
  let i = null, j = null;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.m (id INT PRIMARY KEY, v VARCHAR(10), w VARCHAR(10));
INSERT INTO ${DB}.m VALUES (1,'a','x'),(2,'b','y'),(3,'c','z');`);
    const t = await G.openTable(DB, 'm');
    i = t.id;
    const row = id => t.rows.findIndex(r => String(r[0]) === id);
    const tr = ri => $('res_' + i).querySelector('tbody tr[data-r="' + ri + '"]');
    const marked = sel => [...$('res_' + i).querySelectorAll(sel)];
    const head = () => marked('thead th.kbcol').map(th => th.innerText.trim());
    const rowsMarked = cls => marked('tbody tr.' + cls).map(r => +r.dataset.r).sort();

    gridSetFocus(i, row('2'), 1);
    G.check('the focused cell has its ring', gridCellEl(i, row('2'), 1).classList.contains('kbfocus'));
    G.eq('its row is marked', rowsMarked('kbrow'), [row('2')]);
    G.eq('and its column name', head(), ['v']);

    gridSetFocus(i, row('3'), 2);
    G.eq('moving on marks the new row alone', rowsMarked('kbrow'), [row('3')]);
    G.eq('and the new column alone', head(), ['w']);
    G.eq('one cell has the ring', marked('tbody td.kbfocus').length, 1);

    renderBody(i);
    G.eq('the marks are drawn again with the rows', [rowsMarked('kbrow'), marked('tbody td.kbfocus').length], [[row('3')], 1]);
    sortBy(i, 0);
    G.eq('and with the column names after a sort', head(), ['w']);
    sortBy(i, 0); sortBy(i, 0);

    gridClearFocus(i);
    G.eq('Escape lets go of all three', [rowsMarked('kbrow'), head(), marked('tbody td.kbfocus').length], [[], [], 0]);

    const box = ri => tr(ri).querySelector('input.rowsel');
    box(row('1')).click();
    G.eq('a ticked row is marked', rowsMarked('rowpick'), [row('1')]);
    box(row('1')).click();
    G.eq('and not when unticked again', rowsMarked('rowpick'), []);

    gridSetFocus(i, row('1'), 1);
    t.rows = t.rows.map(r => r.slice()); renderGrid(i);
    G.eq('other rows (a reload) carry no old marks', [rowsMarked('kbrow'), head(), marked('tbody td.kbfocus').length], [[], [], 0]);
    closeTab(i); i = null;

    // A join has no key to save by, so the grid is read-only - and marked all the same.
    j = openTab('focus', '', DB, false, null);
    await runSql(j, `SELECT a.id, a.v, b.w FROM ${DB}.m a JOIN ${DB}.m b ON b.id = a.id ORDER BY a.id`);
    await G.until(() => !T(j).runningReqId, 20000);
    const r = T(j), res = $('res_' + j);
    G.check('the join is read-only', !r.pk, r.pk);
    const rtr = ri => res.querySelector('tbody tr[data-r="' + ri + '"]');
    const rMarked = cls => [...res.querySelectorAll('tbody tr.' + cls)].map(x => +x.dataset.r).sort();
    const rHead = () => [...res.querySelectorAll('thead th.kbcol')].map(th => th.innerText.trim());
    rtr(1).children[1 + 2].dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0 }));
    G.eq('a click there focuses the cell', [gridFocus[j] && gridFocus[j].ri, gridFocus[j] && gridFocus[j].ci], [1, 2]);
    G.eq('and marks its row and column', [rMarked('kbrow'), rHead()], [[1], ['w']]);
    const k = new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true, cancelable: true });
    res.dispatchEvent(k);
    G.eq('the arrow keys move it', [rMarked('kbrow'), rHead(), k.defaultPrevented], [[0], ['w'], true]);
    const kl = new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true, cancelable: true });
    res.dispatchEvent(kl);
    G.eq('across columns too', rHead(), ['v']);
    const ke = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    res.dispatchEvent(ke);
    G.check('Enter opens no editor on it', !res.querySelector('td .celled'), res.querySelector('td .celled'));
    rtr(2).querySelector('input.rowsel').click();
    G.eq('a ticked row is marked', rMarked('rowpick'), [2]);
  } finally {
    if (i != null && T(i)) closeTab(i);
    if (j != null && T(j)) closeTab(j);
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
