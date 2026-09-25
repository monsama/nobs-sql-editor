// Picking things out of a result: whole rows in the checkbox column, single cells in the grid
// itself, and Ctrl+C takes whichever of the two is holding something.
(async () => {
  // Its own table - three columns, eight rows - rather than whichever table another scenario left:
  // run on its own against a server where that was a one-column table, it stopped at the first click.
  const db = 'nobs_gui_pick', tbl = 'p';
  await G.run(`DROP DATABASE IF EXISTS ${db}; CREATE DATABASE ${db};
CREATE TABLE ${db}.p (id INT PRIMARY KEY, a VARCHAR(10), b VARCHAR(10));
INSERT INTO ${db}.p VALUES (1,'a1','b1'),(2,'a2','b2'),(3,'a3','b3'),(4,'a4','b4'),(5,'a5','b5'),(6,'a6','b6'),(7,'a7','b7'),(8,'a8','b8');`);
  const i = openTab(tbl, 'SELECT * FROM `' + db + '`.`' + tbl + '` LIMIT 8;', db, false, tbl);
  await openRun(i);
  await G.until(() => T(i).rows && T(i).rows.length >= 4, 20000);
  const t = T(i);
  const rowsPicked = () => [...(t.selected || [])].sort((a, b) => a - b);
  const cellsPicked = () => [...(t.cellSel || [])].sort();
  const cell = (ri, ci) => gridCellEl(i, ri, ci);
  const box = ri => $('res_' + i).querySelector('tr[data-r="' + ri + '"] input.rowsel');
  const click = (el, mods) => el.dispatchEvent(new MouseEvent('click', { bubbles: true, ...mods }));
  const key = (k, mods) => { const w = $('res_' + i); w.focus(); w.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, ...mods })); };

  t.selected = new Set(); t.cellSel = new Set(); t._selAnchor = null; t._cellAnchor = null; renderBody(i);

  // --- cells, in the grid itself
  click(cell(1, 0), { ctrlKey: true });
  click(cell(2, 1), { ctrlKey: true });
  G.eq('ctrl-click picks single cells', cellsPicked(), ['1:0', '2:1']);
  G.check('and leaves the rows alone', rowsPicked().length === 0, rowsPicked());
  G.check('a picked cell is marked as picked', cell(1, 0).classList.contains('cellpick'), cell(1, 0).className);

  // --- every shown cell of a row picked: the row counts as picked
  t.selected = new Set(); t.cellSel = new Set(); t._pickSel = null; t._cellAnchor = null; renderBody(i);
  const last = t.cols.length - 1;
  click(cell(0, 0), { ctrlKey: true });
  click(cell(1, last), { ctrlKey: true, shiftKey: true });
  G.eq('picking all the cells of two rows picks the two rows', rowsPicked(), [0, 1]);
  G.check('and ticks their checkboxes', box(0).checked && box(1).checked, [box(0).checked, box(1).checked]);
  click(box(2), {});
  click(cell(1, 0), { ctrlKey: true });
  G.eq('dropping one of its cells lets the row go, and a row ticked by hand stays', rowsPicked(), [0, 2]);
  key('Escape', {});
  G.check('Esc lets the picked cells go, and the row they made', cellsPicked().length === 0 && String(rowsPicked()) === '2', { cells: cellsPicked(), rows: rowsPicked() });
  key('Escape', {});
  G.eq('Esc again unticks the rows', rowsPicked(), []);
  // back to the cells picked above, for what follows
  t.selected = new Set(); t.cellSel = new Set(['1:0', '2:1']); t._pickSel = null; t._cellAnchor = '2:1'; renderBody(i);
  click(cell(2, 1), { ctrlKey: true });
  G.eq('ctrl-clicking a cell again drops it', cellsPicked(), ['1:0']);

  // from one corner to the other: start clean so the anchor is unambiguous
  t.cellSel = new Set(); t._cellAnchor = null; renderBody(i);
  click(cell(1, 0), { ctrlKey: true });
  // the browser would run its own text selection under a shift-click, which is the grey streak of
  // text that used to come with the block
  const range = document.createRange();
  range.selectNodeContents(cell(1, 0));
  window.getSelection().removeAllRanges(); window.getSelection().addRange(range);
  const down = new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0, shiftKey: true });
  cell(3, 1).dispatchEvent(down);
  click(cell(3, 1), { shiftKey: true });
  G.eq('shift-click takes the block between the corners', cellsPicked(), ['1:0', '1:1', '2:0', '2:1', '3:0', '3:1']);
  G.check('and takes no text with it', String(window.getSelection()) === '', String(window.getSelection()).slice(0, 40));
  G.check('the press is taken from the browser, so it cannot select any', down.defaultPrevented, 'mousedown went through to the browser');

  // --- dragging a block out, the way a grid is meant to work
  t.cellSel = new Set(); t._cellAnchor = null; renderBody(i);
  const press = (el, mods) => el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, ...mods }));
  const over = el => el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true, buttons: 1 }));
  const release = () => document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  press(cell(1, 0), {}); over(cell(2, 0)); over(cell(2, 1)); release();
  G.eq('a drag paints the block it covers', cellsPicked(), ['1:0', '1:1', '2:0', '2:1']);
  const drawnOnce = $('res_' + i).querySelectorAll('td.cellpick').length;
  G.eq('and the cells themselves carry it', drawnOnce, 4);
  // dragging again starts a new block, unless ctrl says otherwise
  press(cell(4, 0), {}); over(cell(4, 1)); release();
  G.eq('a second drag starts again', cellsPicked(), ['4:0', '4:1']);
  press(cell(6, 0), { ctrlKey: true }); over(cell(6, 1)); release();
  G.eq('with ctrl it adds to what is there', cellsPicked(), ['4:0', '4:1', '6:0', '6:1']);
  // a press that never moves is still a click, and opens nothing by itself
  t.cellSel = new Set(); renderBody(i);
  press(cell(3, 0), {}); release();
  G.eq('a press without a move picks nothing', cellsPicked(), []);

  // --- a copy the way the browser reports one: no focusing first, which is how it failed before
  t.cellSel = new Set(['1:0', '1:1']); renderBody(i);
  document.body.focus();
  const ev = new Event('copy', { bubbles: true, cancelable: true });
  let carried = null;
  ev.clipboardData = { setData: (_m, v) => { carried = v; } };
  document.dispatchEvent(ev);
  G.check('a plain Ctrl+C carries the picked cells, wherever the focus is', carried === [0, 1].map(ci => cellCopyValue(T(i).rows[1][ci])).join('\t'), carried);
  G.check('and the copy is taken over from the browser', ev.defaultPrevented, 'the browser copied its own idea of the selection');

  // --- the menu offers them too
  await cellMenu({ preventDefault() {}, clientX: 40, clientY: 40 }, i, 1, 0);
  const menuItems = [...document.querySelectorAll('#ctx > .item')].map(d => d.textContent);
  G.check('the cell menu offers the picked cells', menuItems.includes('Copy 2 picked cells'), menuItems);
  $('ctx').style.display = 'none';

  // --- what Ctrl+C makes of them
  const realWrite = window.clipWrite; let wrote = null;
  window.clipWrite = txt => { wrote = txt; return Promise.resolve('ok'); };
  try {
    t.cellSel = new Set(['1:0', '1:1']); renderBody(i);
    key('c', { ctrlKey: true });
    await G.wait(200);
    const want = [0, 1].map(ci => cellCopyValue(T(i).rows[1][ci])).join('\t');
    G.eq('Ctrl+C copies the picked cells, tab between columns', wrote, want);

    t.cellSel = new Set(['1:0', '2:0']); renderBody(i);
    wrote = null; key('c', { ctrlKey: true });
    await G.wait(200);
    G.check('cells from two rows come out on two lines', wrote === [1, 2].map(ri => cellCopyValue(T(i).rows[ri][0])).join('\n'), wrote);

    // with no cell picked it falls back to the picked rows, so Ctrl+A then Ctrl+C is the lot
    t.cellSel = new Set(); renderBody(i);
    key('a', { ctrlKey: true });
    await G.wait(200);
    G.eq('Ctrl+A picks every row shown', rowsPicked().length, viewIndices(i).length);
    wrote = null; key('c', { ctrlKey: true });
    await G.wait(300);
    G.check('and Ctrl+C then copies those rows', !!wrote && wrote.split('\n').length === viewIndices(i).length, (wrote || '').slice(0, 40));
  } finally { window.clipWrite = realWrite; }
  G.take();

  // --- rows, in the checkbox column
  t.selected = new Set(); t.cellSel = new Set(); t._selAnchor = null; renderBody(i);
  click(box(1), {});
  G.eq('a checkbox picks its row', rowsPicked(), [1]);
  click(box(4), { shiftKey: true });
  G.eq('shift on a checkbox takes the run of rows', rowsPicked(), [1, 2, 3, 4]);

  // --- a plain click is still the cell's, and lets the picked cells go
  t.cellSel = new Set(['0:0']); renderBody(i);
  click(cell(2, 0), {});
  G.eq('a plain click clears the picked cells', cellsPicked(), []);
  key('Escape', {});
  closeTab(i);
  await G.run(`DROP DATABASE IF EXISTS ${db}`);
  return G.report();
})()
