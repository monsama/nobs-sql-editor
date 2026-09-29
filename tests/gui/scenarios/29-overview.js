// The overview: clicking a database opens it in the list on the left - that one, not every database
// whose name holds its name - and the server's own databases come after yours.
(async () => {
  const A = 'nobs_gui_ov', B = 'nobs_gui_ov2';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${A}; DROP DATABASE IF EXISTS ${B}; CREATE DATABASE ${A}; CREATE DATABASE ${B};`);
    await loadSchemas();
    await showOverview(true);
    await G.until(() => $('overview').querySelector('tr[data-db="' + A + '"]'), 30000);
    $('overview').querySelector('tr[data-db="' + A + '"]').click();
    // Marked once the list has been read again, which can take longer than a fixed pause.
    await G.until(() => curSchema === A && [...$('schemas').children].some(c => c.classList.contains('sel')), 10000);
    const marked = [...$('schemas').children].filter(c => c.classList.contains('sel')).map(c => c.dataset.schema);
    G.eq('a click marks that database, and only that one', marked, [A]);
    G.eq('and opens it', curSchema, A);
    const order = [...$('overview').querySelectorAll('tr[data-db]')].map(tr => tr.getAttribute('data-db'));
    const sys = /^(information_schema|performance_schema|mysql|sys)$/i, firstSys = order.findIndex(d => sys.test(d));
    G.check('your databases come before the server\'s own', firstSys < 0 || order.slice(firstSys).every(d => sys.test(d)), order);

    // The costliest queries: narrowed to one database on the server, opened with their plan, and
    // counted from zero again. Where performance_schema is off (MariaDB's default) none of it is offered.
    const s = window._serverInfo;
    if (s && s.ps) {
      await G.run(`CREATE TABLE ${A}.t (id INT PRIMARY KEY); INSERT INTO ${A}.t VALUES (1),(2),(3);`);
      await G.run('SELECT COUNT(*) FROM t WHERE id > 1', A);
      await topQueriesFilter(A);
      G.check('filtered to one database, only its statements are listed', s.top.db === A && s.top.rows.length > 0 && s.top.rows.every(r => r[0] === A), s.top.rows.map(r => r[0]));
      G.eq('and the filter shows which', $('overview').querySelector('.ovsec select').value, A);
      const qi = s.top.rows.findIndex(r => /FROM `?t`? WHERE/i.test(String(r[1])));
      G.check('the statement run there is among them', qi >= 0, s.top.rows.map(r => r[1]));
      if (s.top.sample && qi >= 0) {
        await openTopQuery(qi);
        const q = tabs[tabs.length - 1];
        await G.until(() => $('mPlan').classList.contains('show') && $('planBody').textContent.trim(), 20000);
        G.check('a click opens it with its real values and its plan', /id > 1/.test($('ed_' + q.id).value) && /read/i.test($('planBody').textContent), [$('ed_' + q.id).value, $('planBody').textContent.slice(0, 80)]);
        hide('mPlan'); closeTab(q.id);
      }
      const realAsk = ask; ask = async () => true;
      try { await topQueriesReset(); } finally { ask = realAsk; }
      G.eq('Reset figures counts from zero', s.top.rows.length, 0);
      await topQueriesFilter('');
      G.take();   // the reset says so in a notification
    } else {
      G.check('without performance_schema, no filter or reset is offered', !$('overview').querySelector('.ovsec select'), 'a filter is shown');
    }
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${A}; DROP DATABASE IF EXISTS ${B};`);
    // Not left selected: the next scenario's query tabs would run in a database that is gone.
    if (curSchema === A || curSchema === B) { curSchema = null; if ($('objdb') && [A, B].includes($('objdb').textContent)) clearObjectsPanel(); }
    await loadSchemas();
  }
  return G.report();
})()
