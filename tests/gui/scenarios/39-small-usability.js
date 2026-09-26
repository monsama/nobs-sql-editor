// Three things that behave as elsewhere: a drag in a cell's editor selects its text and picks no
// cells; New connection, then New or Esc, goes back to the connection you were on; and a folded list
// or half opens again when its divider is dragged.
(async () => {
  const DB = 'nobs_gui_usab';
  const mouse = (el, type, y) => el.dispatchEvent(new MouseEvent(type, { bubbles: true, clientX: 50, clientY: y, button: 0, buttons: type === 'mouseup' ? 0 : 1 }));
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.t (id INT PRIMARY KEY, a VARCHAR(20), b VARCHAR(20)); INSERT INTO ${DB}.t VALUES (1,'one','uno'),(2,'two','dos');`);
    const t = await G.openTable(DB, 't'), A = t.cols.indexOf('a'), B = t.cols.indexOf('b');

    // a drag that starts in the cell's open editor
    const td = gridCellEl(t.id, 0, A); inlineEdit(td, t.id, 0, A);
    await G.until(() => td.querySelector('input,textarea'), 10000); // the column's type can take a round trip
    const box = td.querySelector('input,textarea');
    G.check('the cell has its editor open', !!box);
    t.cellSel = new Set();
    gridDragStart({ button: 0, target: box, ctrlKey: false, shiftKey: false, metaKey: false, preventDefault() {} }, t.id, 0, A);
    gridDragOver({ buttons: 1 }, t.id, 0, B); gridDragEnd();
    G.eq('dragging out of it picks no cells', t.cellSel.size, 0);
    if (box) box.blur(); await G.wait(50); t.pending.upd = {}; renderGrid(t.id);
    closeTab(t.id);

    // New, then New again or Esc
    const before = { list: $('connlist').value, host: $('host').value, user: $('user').value, pass: $('pass').value };
    newConn();
    G.check('New clears the connection bar', $('host').value === '127.0.0.1' && $('user').value === '' && $('connlist').value === '');
    newConn();
    G.eq('New again puts back the one you were on', { list: $('connlist').value, host: $('host').value, user: $('user').value, pass: $('pass').value }, before);
    newConn(); document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    G.eq('and so does Esc', { list: $('connlist').value, host: $('host').value, user: $('user').value, pass: $('pass').value }, before);
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    G.eq('Esc with nothing new to cancel changes nothing', $('host').value, before.host);

    // F1 opens the list of shortcuts, as help does elsewhere
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'F1', bubbles: true }));
    G.check('F1 opens the keyboard shortcuts', $('mShortcuts').classList.contains('show'));
    const scBox = $('mShortcuts').querySelector('.box');
    G.check('which fit the window without scrolling', scBox.scrollHeight <= innerHeight - 20 || innerHeight < 800, { box: scBox.scrollHeight, window: innerHeight });
    hide('mShortcuts');

    // a folded list opens when its divider is dragged
    const sp = $('sideSplit'), sc = $('schemas'), ob = $('objects');
    const y0 = sp.getBoundingClientRect().top + 5;
    sideFold('objects');
    G.check('the objects are folded away', getComputedStyle(ob).display === 'none');
    mouse(sp, 'mousedown', y0); mouse(document, 'mousemove', y0 - 120); mouse(document, 'mouseup', y0 - 120);
    G.check('dragging the divider brings them back', getComputedStyle(ob).display !== 'none' && ob.offsetHeight > 0, ob.offsetHeight);
    sideFold('schemas');
    const y1 = sp.getBoundingClientRect().top + 5;
    mouse(sp, 'mousedown', y1); mouse(document, 'mousemove', y1 + 120); mouse(document, 'mouseup', y1 + 120);
    G.check('and the databases, dragged the other way', getComputedStyle(sc).display !== 'none' && sc.offsetHeight > 40, sc.offsetHeight);
    sideSplitReset();

    // the same between a tab's editor and its results
    const q = openTab('q', 'SELECT 1', DB, false), es = $('es_' + q), ew = $('ew_' + q);
    edFold(q, 'editor');
    const y2 = es.getBoundingClientRect().top + 5;
    mouse(es, 'mousedown', y2); mouse(document, 'mousemove', y2 + 100); mouse(document, 'mouseup', y2 + 100);
    G.check('a folded editor opens when its divider is dragged', !$('pane_' + q).classList.contains('edfolded-editor') && ew.offsetHeight > 44, ew.offsetHeight);
    closeTab(q);
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
