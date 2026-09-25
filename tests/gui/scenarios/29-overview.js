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
    await G.wait(300);
    const marked = [...$('schemas').children].filter(c => c.classList.contains('sel')).map(c => c.dataset.schema);
    G.eq('a click marks that database, and only that one', marked, [A]);
    G.eq('and opens it', curSchema, A);
    const order = [...$('overview').querySelectorAll('tr[data-db]')].map(tr => tr.getAttribute('data-db'));
    const sys = /^(information_schema|performance_schema|mysql|sys)$/i, firstSys = order.findIndex(d => sys.test(d));
    G.check('your databases come before the server\'s own', firstSys < 0 || order.slice(firstSys).every(d => sys.test(d)), order);
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${A}; DROP DATABASE IF EXISTS ${B};`);
    await loadSchemas();
  }
  return G.report();
})()
