// Bytes in a column that is NOT declared binary: what the app shows for them, and whether editing
// one through the cell editor stores the bytes it showed you.
//
// The app hex-encodes any value whose bytes do not decode as UTF-8 (val_to_opt), whatever the
// column is, and the cell editor offers its Text/Hex tabs for anything that merely LOOKS like hex
// - not only for columns the server declares binary. The writer disagrees: litAs() quotes for a
// column that is not declared binary, so a hex value from such a cell is stored as the characters
// "0", "x", ... rather than the bytes they denote. Whether that is reachable at all is the first
// thing this checks: a latin1 column is converted to the connection's charset by the server before
// the app ever sees it, so the premise may simply not hold - which is a fine answer, and the
// reason the checks below are conditional rather than assumed.
//
// The property that matters either way: opening a value and saving it must not change what is
// stored, and a value edited as bytes must be stored as those bytes.
(async () => {
  const DB = 'nobs_gui_bytes';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.t (id INT PRIMARY KEY, c VARCHAR(10) CHARACTER SET latin1, b VARBINARY(10));
INSERT INTO ${DB}.t VALUES (1, 0xFF, 0xFF), (2, 'plain', 0x00FF);`);

    const stored = async (col, id) => await G.one(`SELECT HEX(${col}) FROM ${DB}.t WHERE id = ${id}`, DB);
    G.eq('the fixture holds the byte FF in both columns', [await stored('c', 1), await stored('b', 1)], ['FF', 'FF']);

    const t = await G.openTable(DB, 't');
    const ci = t.cols.indexOf('c'), bi = t.cols.indexOf('b');
    const shown = t.rows[0][ci];
    const declaredBinary = !!(t.binCols && t.binCols[ci]);
    G.check('a latin1 column is not declared binary', !declaredBinary, { binCols: t.binCols });

    // Saving a value nobody edited must be a no-op, whatever the app decided to show.
    await editCell(gridCellEl(t.id, 0, ci), t.id, 0, ci);
    const box = $('vText').value, hexTab = $('vHexTabs').style.display !== 'none';
    hide('mView');
    G.check('opening and closing a value leaves the database alone', (await stored('c', 1)) === 'FF',
      { shown, box: JSON.stringify(box), storedNow: await stored('c', 1) });

    if (!/^0x[0-9A-Fa-f]+$/.test(String(shown))) {
      // The server converted latin1 to the connection charset, so the app got text, not bytes, and
      // the editor is a plain text box. Nothing here to disagree about.
      G.skip('a text column shown as hex', `the server delivered it as text (${JSON.stringify(shown)}), so the hex editor never opens for it`);
    } else {
      G.check('a value shown as hex is edited as hex', hexTab, { shown, hexTab });
      // Exactly what the editor's Save does: getVal() for a hex cell is hexCellValueForSave(mode,
      // box), and onSave hands that to setUpd.
      setUpd(t.id, 0, ci, hexCellValueForSave('hex', '0xFE'));
      await applyChanges(t.id);
      await G.until(() => !t.runningReqId, 20000);
      G.eq('a byte edited as hex is stored as that byte, not as the characters of its hex',
        await stored('c', 1), 'FE');
    }

    // The declared-binary column is the case the writer and the editor agree on - included so a
    // failure above can be read as "this column kind", not "hex editing is broken".
    const t2 = tabs[tabs.length - 1];
    await editCell(gridCellEl(t2.id, 0, bi), t2.id, 0, bi);
    hide('mView');
    setUpd(t2.id, 0, bi, hexCellValueForSave('hex', '0xFE'));
    await applyChanges(t2.id);
    await G.until(() => !t2.runningReqId, 20000);
    G.eq('a declared binary column stores the bytes it was given', await stored('b', 1), 'FE');

    // Bytes that read as text, with CR LF in them, edited in the Text tab: the box shows LF only,
    // and each 0x0D used to be lost on save.
    await G.run(`UPDATE ${DB}.t SET b = 0x610D0A62 WHERE id = 2`);
    const t3 = await G.openTable(DB, 't');
    const r2 = t3.rows.findIndex(r => String(r[t3.cols.indexOf('id')]) === '2');
    await editCell(gridCellEl(t3.id, r2, bi), t3.id, r2, bi);
    const vt = $('vText');
    G.check('the value opens as text', _vHexState && _vHexState.mode === 'text' && /a\s*\n?b/.test(vt.value), { mode: _vHexState && _vHexState.mode, box: JSON.stringify(vt.value) });
    vt.value = vt.value + 'c';
    [...$('mView').querySelectorAll('button')].find(b => b.textContent.trim() === 'Save').click();
    await G.wait(300);
    G.eq('saved from the Text tab, it keeps its CR LF', String(t3.pending.upd[r2 + ':' + bi] || '').toUpperCase(), '0X610D0A6263');
    t3.pending.upd = {}; hide('mView');

    // Text that merely looks like hex is shown as the text it is; bytes are still decoded.
    await G.run(`CREATE TABLE ${DB}.h (id INT PRIMARY KEY, v VARCHAR(10) CHARACTER SET utf8mb4, b VARBINARY(10)); INSERT INTO ${DB}.h VALUES (1, '0x41', 0x41);`);
    const th = await G.openTable(DB, 'h');
    const vi = th.cols.indexOf('v'), hb = th.cols.indexOf('b');
    G.eq('a text column holding "0x41" shows "0x41", not "A"', gridCellEl(th.id, 0, vi).innerText.trim(), '0x41');
    G.check('and a binary column holding the byte 0x41 is still decoded', gridCellEl(th.id, 0, hb).innerText.trim() !== '0x41' || !!(th.binCols && th.binCols[hb]), gridCellEl(th.id, 0, hb).innerText);
  } finally {
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
