// Copied cells can be pasted, as copied rows can: Copy value -> Paste value (into one cell, or into
// every picked cell), Copy N picked cells -> Paste N cells here (laid out from that cell like a
// spreadsheet) or into as many picked cells. What is pasted is the value itself - NULL stays NULL.
(async () => {
  const DB = 'nobs_gui_cellpaste';
  const items = () => [...document.querySelectorAll('#ctx > .item')].map(d => d.textContent.replace(/\s+▸$/, ''));
  const open = async (id, ri, ci) => { await cellMenu({ preventDefault() {}, clientX: 40, clientY: 40 }, id, ri, ci); return items(); };
  const click = label => { const it = [...document.querySelectorAll('#ctx > .item')].find(d => d.textContent === label); if (it) it.click(); $('ctx').style.display = 'none'; return !!it; };
  // The text copy to the system clipboard needs a permission a test browser does not have; what is
  // tested is the values kept for Paste.
  const realCopy = copyText; copyText = () => {};
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.t (id INT PRIMARY KEY, a VARCHAR(10) NULL, b VARCHAR(10) NULL);
INSERT INTO ${DB}.t VALUES (1,'a1','b1'),(2,'a2',NULL),(3,'a3','b3');`);
    const t = await G.openTable(DB, 't');
    const id = t.id, A = t.cols.indexOf('a'), B = t.cols.indexOf('b');
    const cell = (ri, ci) => { const k = ri + ':' + ci; return (k in t.pending.upd) ? t.pending.upd[k] : t.rows[ri][ci]; };
    window._cellClipboard = null; window._rowClipboard = null; window._rowsClipboard = null; t.cellSel = new Set();

    G.check('nothing copied, nothing to paste', !(await open(id, 0, A)).some(x => /^Paste (value|\d+ )/.test(x)));
    await open(id, 0, A); G.check('Copy value', click('Copy value'));
    G.check('then Paste value is offered', (await open(id, 2, A)).includes('Paste value'));
    click('Paste value');
    G.eq('and puts the value in', cell(2, A), 'a1');

    // NULL is copied as NULL, not as an empty string
    await open(id, 1, B); click('Copy value');
    await open(id, 0, B); click('Paste value');
    G.eq('a NULL goes in as NULL', cell(0, B), null);

    // one value into every picked cell
    await open(id, 0, A); click('Copy value');
    t.cellSel = new Set(['1:' + A, '1:' + B]);
    const m = await open(id, 1, A);
    G.check('with cells picked, it fills them all', m.includes('Paste value into 2 picked cells') && !m.includes('Paste value'), m);
    click('Paste value into 2 picked cells');
    G.eq('both picked cells hold it', [cell(1, A), cell(1, B)], ['a1', 'a1']);

    // a block, laid out from where it is pasted
    t.pending.upd = {}; renderGrid(id);
    t.cellSel = new Set(['0:' + A, '0:' + B, '1:' + A, '1:' + B]);
    await open(id, 0, A); G.check('Copy 4 picked cells', click('Copy 4 picked cells'));
    t.cellSel = new Set();
    G.check('Paste 4 cells here is offered', (await open(id, 1, A)).includes('Paste 4 cells here'));
    click('Paste 4 cells here');
    G.eq('the block lands from that cell down and across', [cell(1, A), cell(1, B), cell(2, A), cell(2, B)], ['a1', 'b1', 'a2', null]);

    // as many values as picked cells: in reading order
    t.pending.upd = {}; renderGrid(id);
    t.cellSel = new Set(['2:' + B, '2:' + A, '1:' + B, '1:' + A]);
    const m2 = await open(id, 2, A);
    G.check('four values into four picked cells', m2.includes('Paste 4 values into the picked cells'), m2);
    click('Paste 4 values into the picked cells');
    G.eq('in reading order', [cell(1, A), cell(1, B), cell(2, A), cell(2, B)], ['a1', 'b1', 'a2', null]);

    // cells picked apart stay apart: copied from row 1 column a and row 3 column b, pasted from row 1
    // column a, they land on row 1 a and row 3 b - not side by side
    t.pending.upd = {}; renderGrid(id);
    t.cellSel = new Set(['0:' + A, '2:' + B]);
    await open(id, 0, A); click('Copy 2 picked cells');
    G.eq('the copy keeps the gap between them', window._cellClipboard.map(r => r.map(v => v === undefined ? '-' : v)), [['a1', '-'], ['-', '-'], ['-', 'b3']]);
    t.cellSel = new Set(); t.pending.upd['1:' + B] = 'keep'; renderGrid(id);
    await open(id, 0, A); click('Paste 2 cells here');
    G.eq('and the paste keeps it, leaving the cells in between as they were', [cell(0, A), cell(0, B), cell(1, A), cell(1, B), cell(2, B)], ['a1', 'b1', 'a2', 'keep', 'b3']);

    // picked by dragging over a column that takes NULL: NULL is offered for them
    t.pending.upd = {}; renderGrid(id);
    gridDragStart({ button: 0 }, id, 0, A); gridDragOver({ buttons: 1 }, id, 1, A); gridDragEnd();
    G.eq('a drag picks the cells', [...t.cellSel].sort(), ['0:' + A, '1:' + A].sort());
    const m3 = await open(id, 0, A);
    G.check('Set 2 picked cells to NULL is offered', m3.includes('Set 2 picked cells to NULL'), m3);
    // cells picked on two rows export those rows
    G.check('and the exports offer their 2 rows', m3.includes('Export to CSV (2 rows with picked cells)...') && m3.includes('Export to Markdown (2 rows with picked cells)...'), m3);
    const csv = await (async () => { let out = null; const realDl = dl; dl = (text) => { out = text; }; try { click('Export to CSV (2 rows with picked cells)...'); await G.wait(200); } finally { dl = realDl; } return out; })();
    G.check('which holds those two rows', !!csv && /a1/.test(csv) && /a2/.test(csv) && !/a3/.test(csv), csv);
    G.eq('and the ticks are left as they were', t.selected.size, 0);

    // saved, it is in the table
    t.cellSel = new Set();
    t.pending.upd = {}; renderGrid(id);
    t.cellSel = new Set(['2:' + B, '2:' + A, '1:' + B, '1:' + A]);
    window._cellClipboard = [['a1', 'b1'], ['a2', null]];
    await open(id, 2, A); click('Paste 4 values into the picked cells');
    t.cellSel = new Set();
    await applyChanges(id); await G.until(() => !t.runningReqId);
    G.eq('Apply writes it', await G.q(`SELECT id, a, b FROM ${DB}.t ORDER BY id`), [['1', 'a1', 'b1'], ['2', 'a1', 'b1'], ['3', 'a2', null]]);
    closeTab(id);
  } finally {
    copyText = realCopy; window._cellClipboard = null;
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
