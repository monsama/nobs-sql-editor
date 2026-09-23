// The query bar's buttons keep their size while you work: which ones show a label and which only
// an icon depends on the window's width, not on the pending count, the row total, Wrap: On/Off,
// a result coming or going, or whether there is something to commit.
(async () => {
  const sys = ['information_schema', 'performance_schema', 'mysql', 'sys'];
  const db = (await G.q('SELECT DATABASE()'))[0][0] || (await G.q('SHOW DATABASES')).map(r => r[0]).filter(d => !sys.includes(d))[0];
  const tbl = (await G.q('SHOW TABLES FROM `' + db + '`'))[0][0];
  const i = openTab(tbl, 'SELECT * FROM `' + db + '`.`' + tbl + '` LIMIT 20;', db, false, tbl);
  const bar = $('pane_' + i).querySelector('.toolbar');
  // Narrow enough that some labels have to go, so there is something that could move.
  const pane = $('pane_' + i);
  const was = pane.style.maxWidth;
  pane.style.maxWidth = '900px';
  await G.wait(200);
  const shape = () => [...bar.querySelectorAll('[data-ic]')].map(b => (b.id || b.textContent.trim()) + ':' + b.classList.contains('icoonly')).join('|');
  const before = shape();
  try {
    await openRun(i);
    await G.until(() => T(i).rows && T(i).rows.length > 0, 20000);
    await G.wait(50);
    G.eq('a result arriving moves nothing', shape(), before);
    const t = T(i);
    if (t.pk && t.pending) {
      t.pending.upd['0:0'] = 'x'; updateEditBar(i); await G.wait(50);
      G.eq('a pending count moves nothing', shape(), before);
      for (let k = 1; k < 12; k++) t.pending.upd[k + ':0'] = 'x';
      updateEditBar(i); await G.wait(50);
      G.eq('nor a two-digit one', shape(), before);
      t.pending.upd = {}; updateEditBar(i);
    }
    toggleWrap(i); await G.wait(50);
    G.eq('Wrap: On moves nothing', shape(), before);
    toggleWrap(i);
    t._total = 1234567; t.hasMore = true; updatePager(i); await G.wait(50);
    G.eq('a long row total moves nothing', shape(), before);
    // Turning auto-commit off is a choice that adds two buttons; after that, whether there is
    // something to commit changes only how Commit looks.
    txToggle(i, true); await G.wait(50);
    const manual = shape();
    t.txDirty = true; txPaint(i); await G.wait(50);
    G.eq('something to commit moves nothing', shape(), manual);
    G.check('and Commit shows it', $('txcommit_' + i).classList.contains('go'), $('txcommit_' + i).className);
    t.txDirty = false; txToggle(i, false);
  } finally {
    pane.style.maxWidth = was;
    closeTab(i);
  }
  return G.report();
})()
