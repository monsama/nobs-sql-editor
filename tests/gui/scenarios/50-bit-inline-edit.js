// A BIT value comes from the server as bytes, and the grid shows it as a number. A click on it
// opens the editor with that same number, not the bytes, and a number typed there is saved as the
// bit pattern it stands for.
(async () => {
  const DB = 'nobs_gui_bit';
  let t = null;
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.b (id INT PRIMARY KEY, f BIT(1), w BIT(10)); INSERT INTO ${DB}.b VALUES (1, b'1', b'1000000001');`);
    t = await G.openTable(DB, 'b');
    const F = t.cols.indexOf('f'), W = t.cols.indexOf('w');
    const open = async ci => { const td = gridCellEl(t.id, 0, ci); inlineEdit(td, t.id, 0, ci); await G.until(() => td.querySelector('input,textarea'), 10000); return td.querySelector('input,textarea'); };

    G.eq('the grid shows BIT(1) as a number', gridCellEl(t.id, 0, F).textContent, '1');
    let box = await open(F);
    G.eq('and so does its editor', box && box.value, '1');
    box.value = '0'; box.dispatchEvent(new Event('input')); box.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    G.eq('a number typed in waits to be saved', t.pending.upd['0:' + F], '0');

    box = await open(W);
    G.eq('a wider BIT shows its number too', box && box.value, '513');
    box.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    G.eq('and left alone, it is not changed', '0:' + W in t.pending.upd, false);

    G.check('Apply saves it', await applyChanges(t.id), '');
    G.eq('as the bit it stands for', String(await G.one(`SELECT CAST(f AS UNSIGNED) FROM ${DB}.b WHERE id = 1`)), '0');
  } finally {
    if (t && T(t.id)) { t.pending.upd = {}; closeTab(t.id); }
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
