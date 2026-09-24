// "Set NULL" is offered only in a cell whose column can hold NULL, and pressing the button in a
// cell's box sets that cell and no other. The press used to go on to the cell as the start of a
// drag; the grid was rebuilt under the held button, the release over the cell that now sat there
// picked the two, and the next value typed - or the next press of the button - went into both.
(async () => {
  const DB = 'nobs_gui_setnull';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.p (id INT PRIMARY KEY, n VARCHAR(10) NULL, nn VARCHAR(10) NOT NULL, m VARCHAR(10) NULL);
INSERT INTO ${DB}.p VALUES (1,'a','a','a'),(2,'b','b','b');`);
    const t = await G.openTable(DB, 'p');
    const i = t.id;
    const col = c => t.cols.indexOf(c);
    const cell = (ri, c) => gridCellEl(i, ri, col(c));
    const fire = (el, type, o) => el.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, buttons: type === 'mouseup' || type === 'click' ? 0 : 1, ...o }));
    const openBox = async c => { cellClick(cell(0, c), i, 0, col(c), { shiftKey: false }); await G.until(() => cell(0, c).querySelector('.celled'), 5000); return cell(0, c).querySelector('.celled'); };
    const shut = async () => { renderGrid(i); await G.wait(100); };
    const items = () => [...document.querySelectorAll('#ctx > .item')].map(d => d.textContent);
    const menuOf = async c => { await cellMenu({ preventDefault() {}, clientX: 40, clientY: 40 }, i, 0, col(c)); const m = items(); $('ctx').style.display = 'none'; return m; };

    // --- where it is offered
    let box = await openBox('n');
    G.check('a NULL-able column has the button in its box', !!box.querySelector('button'), box.outerHTML);
    await shut();
    box = await openBox('nn');
    G.check('a NOT NULL column has none', !box.querySelector('button'), box.outerHTML);
    await shut();
    box = await openBox('id');
    G.check('nor has the key', !box.querySelector('button'), box.outerHTML);
    await shut();
    G.check('the menu offers it on a NULL-able column', (await menuOf('n')).includes('Set NULL'));
    G.check('but not on a NOT NULL one', !(await menuOf('nn')).includes('Set NULL'));
    G.check('nor on the key', !(await menuOf('id')).includes('Set NULL'));
    await editCell(cell(0, 'nn'), i, 0, col('nn'));
    G.check('the cell window has no "Set NULL" for a NOT NULL column', ![...$('vActions').querySelectorAll('button')].some(b => b.textContent === 'Set NULL'));
    hide('mView');
    await editCell(cell(0, 'n'), i, 0, col('n'));
    G.check('but does for a NULL-able one', [...$('vActions').querySelectorAll('button')].some(b => b.textContent === 'Set NULL'));
    hide('mView');
    await rowForm(i, 0);
    const rfBtn = c => $('rf_' + col(c)).parentElement.querySelector('button');
    G.check('the row form shows it beside a NULL-able field only', rfBtn('n').style.visibility !== 'hidden' && rfBtn('nn').style.visibility === 'hidden' && rfBtn('id').style.visibility === 'hidden');
    hide('mRowForm');
    setUpd(i, 0, col('nn'), null);
    G.check('and a NOT NULL cell is not given NULL another way', !(('0:' + col('nn')) in t.pending.upd), t.pending.upd);
    G.take();

    // --- the press stays with the button: the drag that would follow it over the next cell picks nothing
    t.cellSel = new Set(); t._cellAnchor = null;
    box = await openBox('n');
    const nb = box.querySelector('button');
    fire(nb, 'mousedown');
    G.check('the press does not start a drag', !window._gridDrag, window._gridDrag);
    const next = cell(0, 'nn');
    fire(next, 'mouseover');
    fire(next, 'mouseup');
    fire(nb, 'click');
    await G.wait(200);
    G.eq('the button sets its own cell to NULL', t.pending.upd['0:' + col('n')], null);
    G.eq('and only that cell', Object.keys(t.pending.upd), ['0:' + col('n')]);
    G.check('with nothing picked afterwards', !t.cellSel || t.cellSel.size === 0, [...(t.cellSel || [])]);
    G.check('and no editor opened on the neighbour', !gridCellEl(i, 0, col('nn')).querySelector('.celled'));

    // --- typed into several picked cells, the NOT NULL ones among them are left alone
    t.pending = { upd: {}, del: new Set(), ins: [] }; renderGrid(i); await G.wait(100);
    t.cellSel = new Set(['0:' + col('n'), '0:' + col('nn'), '0:' + col('m')]); t._cellAnchor = '0:' + col('n');
    gridSetFocus(i, 0, col('n'));
    typeIntoCells(i, 'x');
    await G.until(() => cell(0, 'n').querySelector('.celled button'), 3000);
    cell(0, 'n').querySelector('.celled button').click();
    await G.wait(200);
    G.eq('NULL goes into the picked cells that can hold it', Object.entries(t.pending.upd).sort(), [['0:' + col('m'), null], ['0:' + col('n'), null]].sort());
    G.take();

    // --- the menu does the same for picked cells, without typing anything first
    t.pending = { upd: {}, del: new Set(), ins: [] }; renderGrid(i); await G.wait(100);
    t.cellSel = new Set(['0:' + col('n'), '0:' + col('nn'), '1:' + col('m')]);
    await cellMenu({ preventDefault() {}, clientX: 40, clientY: 40 }, i, 0, col('n'));
    const pm = [...document.querySelectorAll('#ctx > .item')];
    const pick = label => pm.find(d => d.textContent === label);
    G.check('the menu offers NULL and empty for the picked cells', !!pick('Set 3 picked cells to NULL') && !!pick('Set 3 picked cells to empty'), pm.map(d => d.textContent));
    pick('Set 3 picked cells to NULL').click();
    await G.wait(100);
    G.eq('NULL goes into those that can hold it, staged for Apply', Object.entries(t.pending.upd).sort(), [['0:' + col('n'), null], ['1:' + col('m'), null]].sort());
    t.cellSel = new Set(['0:' + col('n'), '0:' + col('nn'), '1:' + col('m')]);
    await cellMenu({ preventDefault() {}, clientX: 40, clientY: 40 }, i, 0, col('n'));
    [...document.querySelectorAll('#ctx > .item')].find(d => d.textContent === 'Set 3 picked cells to empty').click();
    await G.wait(100);
    G.eq('and empty into all of them', Object.entries(t.pending.upd).sort(), [['0:' + col('n'), ''], ['0:' + col('nn'), ''], ['1:' + col('m'), '']].sort());
    t.cellSel = new Set(['0:' + col('nn'), '1:' + col('nn')]);
    await cellMenu({ preventDefault() {}, clientX: 40, clientY: 40 }, i, 0, col('nn'));
    G.check('NULL is not offered when none of the picked cells can hold it', ![...document.querySelectorAll('#ctx > .item')].some(d => /picked cells to NULL/.test(d.textContent)));
    $('ctx').style.display = 'none';
    G.take();
    t.pending = { upd: {}, del: new Set(), ins: [] }; t.cellSel = new Set(); renderGrid(i);
  } finally {
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
