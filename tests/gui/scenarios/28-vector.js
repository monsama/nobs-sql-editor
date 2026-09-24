// MySQL 9's VECTOR through the grid: shown as its bytes in hex, and a cell edited to other bytes
// is saved as them. MySQL's own client does not print a VECTOR as hex, so the PowerShell edition
// turns what it reads back into the bytes; the desktop edition is told the column is binary by
// the server. Only on a server that has the type (compat.yml's MySQL 9.4).
(async () => {
  const DB = 'nobs_gui_vector';
  const C = await G.caps();
  if (!C.vector) { G.skip('a VECTOR column shows and saves as its bytes', 'no MySQL VECTOR on ' + C.version); return G.report(); }
  try {
    // [0,0,0] is twelve zero bytes - text, as far as a reader that only looks at the bytes can tell.
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.v (id INT PRIMARY KEY, e VECTOR(3));
INSERT INTO ${DB}.v VALUES (1, STRING_TO_VECTOR('[1,2,3]')), (2, STRING_TO_VECTOR('[0,0,0]'));`);
    const t = await G.openTable(DB, 'v');
    G.eq('a VECTOR shows as its bytes', G.rowsOf(t), ['1|0x0000803F0000004000004040', '2|0x000000000000000000000000']);
    G.check('and counts as binary', !!(t.binCols && t.binCols[1]), t.binCols);

    // 1, 2 and 4 as little-endian floats.
    t.pending.upd[t.rows.findIndex(r => r[0] === '1') + ':1'] = '0x0000803F0000004000008040';
    G.take(); await applyChanges(t.id); await G.wait(1500);
    G.eq('an edited VECTOR is saved as the bytes typed', [await G.one(`SELECT VECTOR_TO_STRING(e) FROM ${DB}.v WHERE id = 1`), G.take().t],
      ['[1.00000e+00,2.00000e+00,4.00000e+00]', ['Applied 1 change(s).']]);
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
