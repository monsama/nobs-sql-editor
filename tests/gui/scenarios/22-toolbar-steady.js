// The query bar's buttons keep their size while you work: which ones show a label and which only
// an icon depends on the window's width, not on the pending count, the row total, Wrap: On/Off,
// a result coming or going, or whether there is something to commit.
(async () => {
  const sys = ['information_schema', 'performance_schema', 'mysql', 'sys'];
  const db = (await G.q('SELECT DATABASE()'))[0][0] || (await G.q("SELECT DISTINCT TABLE_SCHEMA FROM information_schema.TABLES WHERE TABLE_TYPE = 'BASE TABLE' ORDER BY 1")).map(r => r[0]).filter(d => !sys.includes(d))[0];
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
    // Commit and Rollback are always there, so turning auto-commit off moves nothing either, and
    // whether there is something to commit changes only how Commit looks.
    const edge = el => bar.getBoundingClientRect().right - el.getBoundingClientRect().right;
    const at = edge($('txac_' + i));
    txToggle(i, true); await G.wait(50);
    G.eq('turning auto-commit off moves nothing', shape(), before);
    G.eq('and Auto-commit stays where it was', edge($('txac_' + i)), at);
    t.txDirty = true; txPaint(i); await G.wait(50);
    G.eq('something to commit moves nothing', shape(), before);
    G.check('and Commit shows it', $('txcommit_' + i).classList.contains('go'), $('txcommit_' + i).className);
    t.txDirty = false; txToggle(i, false);
  } finally {
    pane.style.maxWidth = was;
    closeTab(i);
  }

  // The connection box keeps its width from one connection to the next, whatever tags each shows,
  // so switching moves nothing beside it; and the tags stay inside the box.
  const meta = window._connMeta, prim = window._primaryConn;
  const s = $('connlist'), pw = $('pwChip'), pwWas = pw.style.display;
  const opts = ['nobs gui plain', 'nobs gui tagged'].map(n => { const o = new Option(n, n); s.add(o); return o; });
  try {
    window._connMeta = { ...(meta || {}), 'nobs gui plain': { env: '', readonly: false, accent: '' }, 'nobs gui tagged': { env: 'production', readonly: true, accent: '' } };
    const show = async (n, env, ro, lock) => {
      s.value = n; window._primaryConn = lock ? n : prim; renderEnvChip(env, ro, ''); pw.style.display = lock ? 'inline' : 'none';
      connTitle(); syncConnTags(); await G.wait(80);
      return { w: s.getBoundingClientRect().width, x: $('connStatus').getBoundingClientRect().left };
    };
    const a = await show('nobs gui plain', '', false, false);
    const b = await show('nobs gui tagged', 'production', true, true);
    G.eq('switching to a connection with tags keeps the box its width', b.w, a.w);
    G.eq('and moves nothing beside it', b.x, a.x);
    const box = s.getBoundingClientRect(), tags = $('connTags').getBoundingClientRect();
    G.check('the tags sit inside the box', tags.left >= box.left && tags.right <= box.right, { box: [box.left, box.right], tags: [tags.left, tags.right] });
    const c = await show('nobs gui plain', '', false, false);
    G.eq('and back again', [c.w, c.x], [a.w, a.x]);
    // Beside the box: Connect for another connection, Reconnect for the open one, in one place.
    const go = $('connGo'), goX = () => go.getBoundingClientRect().left;
    G.eq('another connection picked: the button connects to it', go.title, 'Connect to the connection picked in the list');
    const x1 = goX();
    s.value = window._activeConnName || ''; connTitle(); await G.wait(30);
    G.eq('the open one picked: it reconnects', go.title, 'Reconnect');
    G.check('and it is shown', go.offsetWidth > 0 && getComputedStyle(go).visibility === 'visible', go.className);
    G.eq('in the same place', goX(), x1);
    // The connected pill is the Disconnect button; there is no separate x beside it.
    const cs = $('connStatus');
    G.check('the connected pill disconnects', cs.getAttribute('role') === 'button' && /Click to disconnect/.test(cs.title) && getComputedStyle(cs).cursor === 'pointer', { role: cs.getAttribute('role'), title: cs.title });
    G.check('and has no x beside it', !$('connX'), 'connX is still there');
  } finally {
    opts.forEach(o => o.remove());
    window._connMeta = meta; window._primaryConn = prim; pw.style.display = pwWas;
    s.value = window._activeConnName || ''; applyEnv(s.value); connTitle(); syncConnTags();
  }
  return G.report();
})()
