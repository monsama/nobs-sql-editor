// Typing with cells picked writes what is typed into every one of them, and a query from the
// history can be kept in the library.
(async () => {
  const DB = 'nobs_gui_typein';
  const LIB = 'nobs gui history save ' + Date.now();
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.p (id INT PRIMARY KEY, v VARCHAR(10), w VARCHAR(10));
INSERT INTO ${DB}.p VALUES (1,'a','a'),(2,'b','b'),(3,'c','c'),(4,'d','d');`);
    const t = await G.openTable(DB, 'p');
    const i = t.id;
    const cell = (ri, ci) => gridCellEl(i, ri, ci);
    const click = (el, mods) => el.dispatchEvent(new MouseEvent('click', { bubbles: true, ...mods }));
    const key = k => { const w = $('res_' + i); w.focus(); const e = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true }); w.dispatchEvent(e); return e; };
    const row = id => t.rows.findIndex(r => String(r[0]) === id);

    t.cellSel = new Set(); t._cellAnchor = null; renderBody(i);
    click(cell(row('1'), 1), { ctrlKey: true });
    click(cell(row('3'), 1), { ctrlKey: true });
    click(cell(row('4'), 2), { ctrlKey: true });
    const typed = key('x');
    G.check('a typed key is taken by the grid', typed.defaultPrevented, 'the key went on to the browser');
    await G.until(() => $('res_' + i).querySelector('td .celled input'), 3000); const inp = $('res_' + i).querySelector('td .celled input');
    G.eq('the editor opens holding the key', inp && inp.value, 'x');
    inp.value = 'xy'; inp.dispatchEvent(new Event('input', { bubbles: true }));
    inp.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    G.eq('Enter writes it into every picked cell', Object.entries(t.pending.upd).sort(),
      [[row('1') + ':1', 'xy'], [row('3') + ':1', 'xy'], [row('4') + ':2', 'xy']].sort());
    G.check('and into nothing else', t.pending.upd[row('2') + ':1'] === undefined, t.pending.upd);

    // nothing picked: the focused cell alone
    t.pending = { upd: {}, del: new Set(), ins: [] };
    t.cellSel = new Set(); t._cellAnchor = null; renderGrid(i);
    gridSetFocus(i, row('2'), 2);
    key('z');
    await G.until(() => $('res_' + i).querySelector('td .celled input'), 3000); const one = $('res_' + i).querySelector('td .celled input');
    one.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    G.eq('with nothing picked it is the focused cell', Object.entries(t.pending.upd), [[row('2') + ':2', 'z']]);
    t.pending = { upd: {}, del: new Set(), ins: [] }; renderGrid(i);

    // --- the history, into the library
    const realInput = inputBox, realAsk = ask;
    inputBox = async o => ({ name: LIB, sql: o.fields.find(f => f.key === 'sql').value });
    ask = async () => true;
    try { await histSaveToLib('SELECT 42'); } finally { inputBox = realInput; ask = realAsk; }
    await libLoad();
    const saved = libAll().find(x => x.name === LIB);
    G.eq('a history entry is saved to the library', saved && saved.sql, 'SELECT 42');
    openHistory();
    G.check('each history entry offers it', [...$('histList').querySelectorAll('button')].some(b => b.textContent === 'Save to library'), 'no button');
    // A click on the query - which is where resizing its box ends - used to open it and close the
    // window. Only Open (or a double-click) opens it now.
    const entry = $('histList').querySelector('.item');
    if (entry) {
      entry.querySelector('code').click();
      G.check('clicking a history query, as resizing it does, leaves the window open', getComputedStyle($('mHist')).display !== 'none', '');
      const before = tabs.length;
      [...entry.querySelectorAll('button')].find(b => b.textContent === 'Open').click();
      G.check('and Open opens it in a tab', tabs.length === before + 1 && getComputedStyle($('mHist')).display === 'none', tabs.length - before);
    }
    hide('mHist');
  } finally {
    await G.A('/api/lib-delete', { name: LIB });
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
