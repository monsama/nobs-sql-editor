// A click on a cell just after scrolling picks that cell only. The grid draws the rows scrolled to a
// frame later, at a height it estimates, so the row under a pointer that has not moved could change
// while the button was down; the browser's mouseover on that row picked the cells from the one
// clicked down to it - five or six of them. A real drag still picks the block.
(async () => {
  const DB = 'nobs_gui_clickscroll';
  const D = '(SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9)';
  let t = null;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.r (id INT PRIMARY KEY, v VARCHAR(20)); INSERT INTO ${DB}.r SELECT n, CONCAT('v', n) FROM (SELECT a.d+b.d*10+c.d*100+1 AS n FROM ${D} a, ${D} b, ${D} c) x;`);
    t = await G.openTable(DB, 'r');
    const V = t.cols.indexOf('v'), wrap = $('res_' + t.id), rowH = t._rowH || 23;
    wrap.scrollTop = 400 * rowH; renderBody(t.id); await G.wait(100);
    const tr = [...document.querySelectorAll('#tbody_' + t.id + ' tr[data-r]')][8];
    const ri = +tr.dataset.r, cell = gridCellEl(t.id, ri, V), below = gridCellEl(t.id, ri + 5, V);
    const r = cell.getBoundingClientRect(), x = r.left + r.width / 2, y = r.top + r.height / 2;
    const ev = (el, type, at) => el.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, buttons: type === 'mouseup' ? 0 : 1, clientX: at.x, clientY: at.y }));
    t.cellSel = new Set();

    // The press, then the rows moving under the still pointer: a mouseover on the row five below,
    // at the very point pressed, and a mousemove there.
    ev(cell, 'mousedown', { x, y });
    ev(below, 'mouseover', { x, y });
    ev(document, 'mousemove', { x, y });
    G.eq('rows moving under a still pointer pick nothing', [...t.cellSel], []);
    ev(document, 'mouseup', { x, y });
    G.check('and the press is not taken for a drag', !window._gridDragJustEnded && !document.body.classList.contains('gridpicking'));

    // A real drag to the same row: the pointer goes there.
    const b = below.getBoundingClientRect(), to = { x: b.left + b.width / 2, y: b.top + b.height / 2 };
    ev(cell, 'mousedown', { x, y });
    ev(document, 'mousemove', to);
    ev(below, 'mouseover', to);
    G.eq('dragging there picks the six cells', t.cellSel.size, 6);
    ev(document, 'mouseup', to);

    // Drawn again with a row height that is off - rows differ by a pixel or two, and the estimate is
    // only an estimate - the rows stay where they are on screen: the one under the pointer is still
    // the one under it, so a click picks the cell it is on.
    // A point in the middle of the results; everything under it is looked through, as a toast may lie on top.
    const wr = wrap.getBoundingClientRect(), px = wr.left + wr.width / 2, py = wr.top + wr.height / 2;
    const rowAt = () => { for (const el of document.elementsFromPoint(px, py)) { const r = el.closest && el.closest('tr[data-r]'); if (r && wrap.contains(r)) return r.dataset.r; } return null; };
    const view = viewIndices(t.id);
    for (const off of [3, -3, 1.5]) {
      renderBody(t.id);
      const was = rowAt();
      // Scrolled by seven rows, and drawn with the estimate off: the row under the pointer is the
      // one seven further on, not one a few rows from it.
      t._rowH = rowH + off; wrap.scrollTop += 7 * rowH; renderBody(t.id);
      G.eq('with the row height estimated ' + off + 'px off, the row under the pointer is the one scrolled to', rowAt(), String(view[view.indexOf(+was) + 7]));
    }
  } finally {
    document.body.classList.remove('gridpicking'); window._gridDrag = null;
    if (t && T(t.id)) { if (t.pending) t.pending.upd = {}; closeTab(t.id); }
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
