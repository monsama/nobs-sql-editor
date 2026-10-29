// The query history keeps each run the user starts, with what came of it: the time, connection and
// database, the rows a query returned or a script changed, the error, how long it took. The app's own
// runs are not kept. The History window finds them again by text, outcome and connection, shows the
// same SQL once with how often it ran, and takes runs out again.
(async () => {
  if (typeof histStart !== 'function') { G.skip('query history', 'this edition keeps the plain history'); return G.report(); }
  const DB = 'nobs_gui_hist', MARK = 'guihist' + Date.now();
  const wasOn = histOn();
  let i = null;
  const mine = async () => { _histItems = null; const r = await api('/api/hist-list'); return (r.items || []).filter(e => String(e.sql).includes(MARK)); };
  const run = async sql => { $('ed_' + i).value = sql; await runTab(i, true); await G.until(() => !T(i).runningReqId, 20000); await G.wait(300); };
  try {
    setHistOn(true);
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.h (id INT PRIMARY KEY);`);
    i = openTab('hist', '', DB, false, null);

    await run(`SELECT 1 AS a /* ${MARK} q */`);
    let e = (await mine()).find(x => x.sql.includes(MARK + ' q'));
    G.check('a query the user runs is kept', !!e, await mine());
    G.eq('with what came of it', e && [e.o, e.rows, e.db, e.conn, e.tab, typeof e.ms, typeof e.ts], ['ok', 1, DB, histConn(), T(i).title, 'number', 'number']);

    await run(`INSERT INTO ${DB}.h VALUES (1),(2),(3) /* ${MARK} w */`);
    e = (await mine()).find(x => x.sql.includes(MARK + ' w'));
    G.eq('a write, with the rows it changed', e && [e.o, e.affected], ['ok', 3]);

    await run(`SELECT * FROM ${DB}.no_such_table /* ${MARK} e */`);
    e = (await mine()).find(x => x.sql.includes(MARK + ' e'));
    G.check('an error, with its message', e && e.o === 'err' && /no_such_table/.test(e.err), e);

    await runSql(i, `SELECT 2 AS app_run /* ${MARK} app */`);
    await G.until(() => !T(i).runningReqId, 20000); await G.wait(300);
    G.check('a run the app starts itself is not kept', !(await mine()).some(x => x.sql.includes(MARK + ' app')), await mine());

    setHistOn(false);
    await run(`SELECT 3 /* ${MARK} off */`);
    G.check('nor anything while the history is off', !(await mine()).some(x => x.sql.includes(MARK + ' off')), '');
    setHistOn(true);

    await run(`SELECT 1 AS a /* ${MARK} q */`);
    await openHistory();
    $('histScope').value = 'conn'; $('histOut').value = ''; $('histWhen').value = '1'; $('histQ').value = MARK; histRender();
    const rows = () => [...$('histList').querySelectorAll('.citem')];
    G.eq('the window shows each text once', rows().length, 3);
    G.check('the same query twice is one row, run twice', rows().some(r => /×2/.test(r.innerText) && r.innerText.includes(MARK + ' q')), rows().map(r => r.innerText));
    $('histOut').value = 'err'; histRender();
    G.check('the errors alone', rows().length === 1 && rows()[0].innerText.includes(MARK + ' e'), rows().map(r => r.innerText));
    $('histOut').value = ''; $('histQ').value = MARK + ' w'; histRender();
    G.eq('search narrows it to the text', rows().length, 1);
    $('histQ').value = 'no_such_table'; histRender();
    G.check('and finds an error by its message', rows().some(r => r.innerText.includes(MARK + ' e')), rows().map(r => r.innerText));

    $('histQ').value = MARK + ' q'; histRender();
    rows()[0].querySelector('.citem-h').click();
    const before = tabs.length;
    [...rows()[0].querySelectorAll('button')].find(b => b.textContent === 'Open').click();
    G.check('Open opens it in a tab, on its database', tabs.length === before + 1 && T(activeTab).db === DB && $('ed_' + activeTab).value.includes(MARK), T(activeTab) && T(activeTab).db);
    closeTab(activeTab);

    await openHistory(); $('histQ').value = MARK + ' q'; histRender();
    rows()[0].querySelector('.citem-h').click();
    [...rows()[0].querySelectorAll('button')].find(b => /^Delete/.test(b.textContent)).click();
    for (let k = 0; k < 50 && (await mine()).some(x => x.sql.includes(MARK + ' q')); k++) await G.wait(100);
    G.check('Delete takes both runs of it out', !(await mine()).some(x => x.sql.includes(MARK + ' q')), '');
  } finally {
    setHistOn(wasOn);
    const left = await mine(); if (left.length) await api('/api/hist-delete', { ids: left.map(x => x.id) });
    _histItems = null;
    if ($('mHist').classList.contains('show')) hide('mHist');
    $('histQ').value = ''; $('histOut').value = ''; $('histWhen').value = '0';
    if (i != null && T(i)) closeTab(i);
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
