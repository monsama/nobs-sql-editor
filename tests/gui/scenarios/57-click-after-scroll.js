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
  } finally {
    document.body.classList.remove('gridpicking'); window._gridDrag = null;
    if (t && T(t.id)) { if (t.pending) t.pending.upd = {}; closeTab(t.id); }
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
