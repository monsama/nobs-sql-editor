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

    // a drag past the bottom of the results scrolls them, and the block grows with it
    await G.run(`CREATE TABLE ${DB}.tall (id INT PRIMARY KEY, v VARCHAR(10)); INSERT INTO ${DB}.tall SELECT i, CONCAT('v',i) FROM (SELECT a.d*100+b.d*10+c.d+1 AS i FROM (SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) a, (SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) b, (SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9) c) n WHERE i<=300;`);
    const tl = await G.openTable(DB, 'tall'), V = tl.cols.indexOf('v'), wrap = $('res_' + tl.id);
    G.check('the results are taller than their window', wrap.scrollHeight > wrap.clientHeight + 200, [wrap.scrollHeight, wrap.clientHeight]);
    tl.cellSel = new Set();
    gridDragStart({ button: 0, target: gridCellEl(tl.id, 0, V), ctrlKey: false, shiftKey: false, metaKey: false, preventDefault() {} }, tl.id, 0, V);
    gridDragOver({ buttons: 1 }, tl.id, 1, V);
    const wb = wrap.getBoundingClientRect(), mm = y => document.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: wb.left + 60, clientY: y, buttons: 1 }));
    mm(wb.bottom + 40); await G.wait(600);
    const top1 = wrap.scrollTop, picked1 = tl.cellSel.size;
    G.check('dragging past the bottom scrolls the results', top1 > 100, top1);
    // the pointer is over the rows' delete column here, not a value: the block still grows, in its own column
    G.check('and picks the cells it scrolls to, in the column it started in', picked1 > 10 && [...tl.cellSel].every(k => +k.split(':')[1] === V), picked1);
    mm(wb.top + wb.height / 2); await G.wait(200);
    const top2 = wrap.scrollTop; await G.wait(300);
    G.eq('back inside, it stops', wrap.scrollTop, top2);
    document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    mm(wb.bottom + 40); await G.wait(300);
    G.eq('and once let go, the edge does nothing', wrap.scrollTop, top2);

    // the same with something laid over the bottom of the results - a toast, say: the rows under
    // it still join the block. It used to take only the topmost element there, found no row, and
    // the block stayed as it was while the results scrolled on.
    wrap.scrollTop = 0; await G.wait(200);
    const cover = document.createElement('div');
    cover.style.cssText = `position:fixed;left:${wb.left}px;top:${wb.bottom - 60}px;width:${wb.width}px;height:60px;z-index:9999;background:rgba(0,0,0,.2)`;
    document.body.appendChild(cover);
    try {
      tl.cellSel = new Set();
      gridDragStart({ button: 0, target: gridCellEl(tl.id, 0, V), ctrlKey: false, shiftKey: false, metaKey: false, preventDefault() {} }, tl.id, 0, V);
      gridDragOver({ buttons: 1 }, tl.id, 1, V);
      mm(wb.bottom + 40); await G.wait(600);
      G.check('with the edge covered, it still picks the cells it scrolls to', wrap.scrollTop > 100 && tl.cellSel.size > 10, [wrap.scrollTop, tl.cellSel.size]);
      document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    } finally { cover.remove(); }

    // from the last row straight down, out of the results, without passing another cell
    wrap.scrollTop = wrap.scrollHeight; await G.wait(300);
    const lastRi = +[...wrap.querySelectorAll('tr[data-r]')].pop().dataset.r, lastTd = gridCellEl(tl.id, lastRi, V);
    tl.cellSel = new Set(); getSelection().removeAllRanges();
    gridDragStart({ button: 0, target: lastTd, ctrlKey: false, shiftKey: false, metaKey: false, preventDefault() {} }, tl.id, lastRi, V);
    // what the browser does on its own when a press in text is dragged: a selection from there
    const rg = document.createRange(); rg.setStart(lastTd, 0); rg.setEnd(document.body, document.body.childNodes.length); getSelection().addRange(rg);
    const lr = lastTd.getBoundingClientRect();
    document.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: lr.left + 5, clientY: wb.bottom + 60, buttons: 1 }));
    G.eq('leaving the last row downwards picks that cell', [...tl.cellSel], [lastRi + ':' + V]);
    G.check('and selects no text on the page', getSelection().isCollapsed && document.body.classList.contains('gridpicking'), getSelection().toString().slice(0, 80));
    document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    G.check('letting go gives text selection back to the page', !document.body.classList.contains('gridpicking'));
    // the browser is not let start a text selection in a result cell at all - only in text boxes
    const sel0 = (n) => { const ev = new Event('selectstart', { bubbles: true, cancelable: true }); n.dispatchEvent(ev); return ev.defaultPrevented; };
    G.check('no text selection starts in a result cell', sel0(lastTd.firstChild || lastTd) && sel0(lastTd));
    const edTa = document.querySelector('textarea.editor') || document.querySelector('input');
    G.check('but it does in a text box', edTa && !sel0(edTa));
    closeTab(tl.id);

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
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'F1', bubbles: true }));
    G.check('and F1 again closes them', !$('mShortcuts').classList.contains('show'));
    hide('mShortcuts');

    // F6 between the editor and the results; Ctrl+Shift+F formats; F9 runs as F5 does
    const k = openTab('keys', 'select id,a from ' + DB + '.t where id=1', DB, false);
    const ked = $('ed_' + k); ked.focus();
    const key = (el, o) => el.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...o }));
    key(ked, { key: 'F6' });
    G.check('F6 goes from the editor to the results', document.activeElement === $('res_' + k), document.activeElement && document.activeElement.id);
    key(document.activeElement, { key: 'F6' });
    G.check('and back', document.activeElement === ked);
    const typed = ked.value; key(ked, { key: 'F', ctrlKey: true, shiftKey: true });
    G.check('Ctrl+Shift+F formats the query (not the find box)', ked.value !== typed && ked.value.includes('\nfrom ') && (!$('fr_' + k) || $('fr_' + k).style.display === 'none'), ked.value);
    key(ked, { key: 'F9' }); await G.until(() => T(k).rows && T(k).rows.length && !T(k).runningReqId, 15000);
    G.eq('F9 runs it', T(k).rows.map(r => String(r[0])), ['1']);
    closeTab(k);

    // Ctrl+Enter with the cursor where typing "...;" leaves it runs that statement, not the next line's
    const cs = openTab('cursor', 'select id from ' + DB + '.t where id=1;\nselect id from ' + DB + '.t where id=2;', DB, false);
    const ced = $('ed_' + cs); ced.focus(); const endOfFirst = ced.value.indexOf(';') + 1; ced.setSelectionRange(endOfFirst, endOfFirst);
    key(ced, { key: 'Enter', ctrlKey: true }); await G.until(() => T(cs).rows && T(cs).rows.length && !T(cs).runningReqId, 15000);
    G.eq('Ctrl+Enter just after a ; runs the statement it ends', T(cs).rows.map(r => String(r[0])), ['1']);
    ced.setSelectionRange(ced.value.length - 3, ced.value.length - 3); T(cs).rows = null;
    key(ced, { key: 'Enter', ctrlKey: true }); await G.until(() => T(cs).rows && T(cs).rows.length && !T(cs).runningReqId, 15000);
    G.eq('and inside the second, the second', T(cs).rows.map(r => String(r[0])), ['2']);
    closeTab(cs);

    // text sizes and the zoom (Settings -> General)
    const zq = openTab('sizes', 'SELECT 1', DB, false);
    await openSettings(); const zoom0 = $('setZoom').value;
    uiSizeSet('ed', 18);
    G.eq('the editor text size applies to the editor and its colouring alike', [getComputedStyle($('ed_' + zq)).fontSize, getComputedStyle($('hl_' + zq)).fontSize], ['18px', '18px']);
    G.eq('and is remembered', localStorage.getItem('edFontSize'), '18');
    $('ed_' + zq).dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, ctrlKey: true, deltaY: -100 }));
    G.eq('Ctrl + wheel in the editor changes that setting', uiSizeGet('ed'), 19);
    uiSizeSet('grid', 16);
    G.eq('the results text size applies to the grids', getComputedStyle(document.documentElement).getPropertyValue('--gridfs').trim(), '16px');
    G.eq('Settings shows them', [$('setEdFs').value, $('setGridFs').value], ['19', '16']);
    await runSql(zq, 'SELECT 1 AS one'); await G.until(() => $('res_' + zq).querySelector('table.grid tbody td'));
    $('res_' + zq).querySelector('table.grid tbody td').dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, ctrlKey: true, deltaY: 100 }));
    G.eq('Ctrl + wheel over the results changes theirs, a step at a time', uiSizeGet('grid'), 15);
    G.eq('and leaves the editor\'s alone', uiSizeGet('ed'), 19);
    const dpr0 = devicePixelRatio;
    await uiZoomSet(1.25);
    const cfgZ = await G.A('/api/get-config');
    G.eq('the zoom is saved', String(cfgZ.config && cfgZ.config.ui_zoom), '1.25');
    if (G.desktop) {
      await G.until(() => devicePixelRatio > dpr0 * 1.2, 3000);
      G.check('and the desktop window zooms at once', Math.abs(devicePixelRatio / dpr0 - 1.25) < 0.02, { before: dpr0, after: devicePixelRatio });
      await openSettings(); await G.wait(200);
      const bx = $('mSettings').querySelector('.box').getBoundingClientRect();
      G.check('with Settings centred again', Math.abs((bx.left + bx.width / 2) - innerWidth / 2) < 30, { left: bx.left, width: bx.width, window: innerWidth });
    }
    const bad = await G.A('/api/save-config', { config: { ui_zoom: '9' } });
    G.check('a zoom out of range is refused', bad.ok === false, bad);
    // the fonts: only ones this computer has are offered; each applies where it says
    const offered = id => [...$(id).options].map(o => o.value).filter(Boolean);
    G.check('the fonts on offer are ones this computer has', ['setEdFont', 'setGridFont', 'setUiFont'].every(id => offered(id).every(fontInstalled)) && offered('setUiFont').every(n => UI_SANS.includes(n)), [offered('setEdFont'), offered('setUiFont')]);
    const mono = offered('setEdFont').find(n => n !== 'Cascadia Code'), sans = offered('setUiFont').find(n => n !== 'Segoe UI');
    if (mono) { uiFontSet('ed', mono); G.check('the code font applies to the editor', getComputedStyle($('ed_' + zq)).fontFamily.includes(mono) && getComputedStyle($('hl_' + zq)).fontFamily === getComputedStyle($('ed_' + zq)).fontFamily, getComputedStyle($('ed_' + zq)).fontFamily); }
    if (sans) {
      uiFontSet('ui', sans); G.check('the interface font applies to the page', getComputedStyle(document.body).fontFamily.includes(sans), getComputedStyle(document.body).fontFamily);
      const gt = document.createElement('table'); gt.className = 'grid'; document.body.appendChild(gt);
      G.check('and to the results, by default', getComputedStyle(gt).fontFamily.includes(sans), getComputedStyle(gt).fontFamily);
      if (mono) { uiFontSet('grid', mono); G.check('unless they have a font of their own', getComputedStyle(gt).fontFamily.includes(mono), getComputedStyle(gt).fontFamily); }
      gt.remove();
    }
    uiFontSet('ui', 'Wingdings');
    G.eq('a font not on the list is not kept', uiFontGet('ui'), '');
    await uiSizesReset();
    G.eq('Reset puts the fonts back as well', [uiFontGet('ed'), uiFontGet('grid'), uiFontGet('ui'), document.documentElement.style.getPropertyValue('--mono'), $('setEdFont').value], ['', '', '', '', '']);
    if (G.desktop) await G.until(() => Math.abs(devicePixelRatio - dpr0) < 0.01, 3000);
    const cfg1 = await G.A('/api/get-config');
    G.check('Reset to defaults: 13 px both, and 100%', uiSizeGet('ed') === 13 && uiSizeGet('grid') === 13 && String(cfg1.config.ui_zoom) === '1', [uiSizeGet('ed'), uiSizeGet('grid'), cfg1.config.ui_zoom]);

    // Restore defaults (Default appearance): everything about how the app looks and is arranged, in one go - and nothing it
    // remembers about that survives it (a setting added to Appearance and left out of the reset fails here)
    const keys = () => { const k = []; for (let i = 0; i < localStorage.length; i++) k.push(localStorage.key(i)); return k; };
    const before0 = new Set(keys());
    const lightBefore = !document.body.classList.contains('dark');
    if (!lightBefore) toggleTheme();
    uiSizeSet('ed', 17); uiSizeSet('grid', 15);
    const f1 = offered('setEdFont')[0], f2 = offered('setUiFont')[0];
    if (f1) { uiFontSet('ed', f1); uiFontSet('grid', f1); } if (f2) uiFontSet('ui', f2);
    localStorage.setItem('sideW', '420'); $('side').style.width = '420px'; localStorage.setItem('toastMs', '2000');
    setSideFolded(true); setLogFolded(true); sideFold('objects');
    await uiZoomSet(1.1);
    await resetAppearance();
    const left = keys().filter(k => !before0.has(k) && !/^(session|history|overviewCache|tableSizes|pinned_)/.test(k));
    G.eq('nothing it remembers about the look is left over', left, []);
    G.check('the theme is dark again, the sidebar open at its width, the lists and the log unfolded',
      document.body.classList.contains('dark') && $('side').style.width === '280px' && !document.body.classList.contains('side-folded') && getComputedStyle($('objects')).display !== 'none' && getComputedStyle($('schemas')).display !== 'none' && $('log').style.display !== 'none',
      { dark: document.body.classList.contains('dark'), w: $('side').style.width, body: document.body.className });
    G.eq('the fonts and sizes are the defaults', [uiSizeGet('ed'), uiSizeGet('grid'), uiFontGet('ed'), uiFontGet('grid'), uiFontGet('ui')], [13, 13, '', '', '']);
    G.eq('and the zoom is 100%', String((await G.A('/api/get-config')).config.ui_zoom), '1');
    if (lightBefore) toggleTheme();
    // every row in Settings has its control, connected or not (the Overview cache button used to hide)
    document.body.classList.add('disconnected');
    const bare = [...$('mSettings').querySelectorAll('.setpage .setrow')].filter(r => { const c = r.querySelector('.setrc'); return c && c.children.length && ![...c.querySelectorAll('button,select,input')].some(x => getComputedStyle(x).display !== 'none'); }).map(r => r.querySelector('.setrt').textContent);
    document.body.classList.remove('disconnected');
    G.eq('no Settings row loses its control when disconnected', bare, []);

    // Clear all app data: everything the page stores goes, preferences included, with the connections,
    // the library and the zoom - the calls are caught here, so the test's own connections stay
    const saved = {}; for (const k of keys()) saved[k] = localStorage.getItem(k);
    localStorage.setItem('sideW', '400'); localStorage.setItem('edFont', 'Consolas'); localStorage.setItem('nobsExpOpts', '{}');
    const realApi = api, realTimeout = window.setTimeout, calls = [];
    api = async (path, p) => { calls.push(path + (p && p.config ? ' ' + JSON.stringify(p.config) : '')); return { ok: true }; };
    window.setTimeout = (f, ms) => ms === 500 ? 0 : realTimeout(f, ms); // the reload after it
    try { await clearAllData(); } finally { api = realApi; window.setTimeout = realTimeout; }
    G.eq('Clear all app data leaves nothing in the page\'s storage', keys(), []);
    G.eq('and clears the connections and the library, and the zoom', calls, ['/api/conn-clear', '/api/lib-clear', '/api/save-config {"ui_zoom":"1"}']);
    for (const k in saved) localStorage.setItem(k, saved[k]);
    if (G.desktop) await G.until(() => Math.abs(devicePixelRatio - dpr0) < 0.01, 3000);
    if (+zoom0 && +zoom0 !== 1) await uiZoomSet(+zoom0);
    hide('mSettings'); closeTab(zq); G.take();

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
    // dragged past the end, the list there folds away as its caret would, and back in the same drag
    const y4 = sp.getBoundingClientRect().top + 5, room = sc.parentElement.clientHeight;
    mouse(sp, 'mousedown', y4); mouse(document, 'mousemove', y4 + room);
    G.check('dragged to the bottom, the objects fold away', document.body.classList.contains('objs-folded') && getComputedStyle(ob).display === 'none');
    mouse(document, 'mousemove', y4); mouse(document, 'mouseup', y4);
    G.check('and dragged back up, they are there again', !document.body.classList.contains('objs-folded') && ob.offsetHeight > 0, ob.offsetHeight);
    sideSplitReset();

    // the same between a tab's editor and its results
    const q = openTab('q', 'SELECT 1', DB, false), es = $('es_' + q), ew = $('ew_' + q);
    edFold(q, 'editor');
    const y2 = es.getBoundingClientRect().top + 5;
    mouse(es, 'mousedown', y2); mouse(document, 'mousemove', y2 + 100); mouse(document, 'mouseup', y2 + 100);
    G.check('a folded editor opens when its divider is dragged', !$('pane_' + q).classList.contains('edfolded-editor') && ew.offsetHeight > 44, ew.offsetHeight);
    const y3 = es.getBoundingClientRect().top + 5, h3 = ew.offsetHeight;
    mouse(es, 'mousedown', y3); mouse(document, 'mousemove', y3 - h3 - 10);
    G.check('dragged to the top, the editor folds away', $('pane_' + q).classList.contains('edfolded-editor'));
    mouse(document, 'mousemove', y3 - h3 + 150); mouse(document, 'mouseup', y3 - h3 + 150);
    G.check('and dragged back down, it opens again', !$('pane_' + q).classList.contains('edfolded-editor') && ew.offsetHeight > 44, ew.offsetHeight);
    closeTab(q);
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
