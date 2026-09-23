// A result charts as bars or a line, and Explain draws the plan as well as filling the grid.
(async () => {
  const q = 'SELECT TABLE_SCHEMA, COUNT(*) AS tables, SUM(TABLE_ROWS) AS est_rows FROM information_schema.TABLES GROUP BY TABLE_SCHEMA ORDER BY 1;';
  const i = openTab('chart', q, null, false, null);
  try {
    await runSql(i, q);
    await G.until(() => T(i).rows && T(i).rows.length > 0, 20000);
    chartOpen(i);
    await G.wait(200);
    const plot = $('chartPlot');
    G.check('the chart window opens', $('mChart').classList.contains('show'), $('mChart').className);
    G.eq('the axis is the column of names', $('chartX').selectedOptions[0].textContent, 'TABLE_SCHEMA');
    G.eq('and the columns of numbers are the series', [...$('chartSeries').querySelectorAll('input:checked')].length, 2);
    G.eq('one bar per row and series', plot.querySelectorAll('.cmark').length, T(i).rows.filter(r => r[1] != null).length + T(i).rows.filter(r => r[2] != null).length);
    G.check('with a legend for two series', $('chartLegend').querySelectorAll('.cleg').length === 2, $('chartLegend').innerHTML);
    $('chartType').value = 'line'; chartDraw();
    G.eq('as a line, one path per series', plot.querySelectorAll('.cline').length, 2);
    const hit = plot.querySelector('.chit');
    hit.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: hit.getBoundingClientRect().left + 2, clientY: hit.getBoundingClientRect().top + 10 }));
    G.check('hovering a point says what is there', $('chartTip').style.display === 'block' && $('chartTip').textContent.includes(String(T(i).rows[0][0])), $('chartTip').textContent);
    hide('mChart');

    $('ed_' + i).value = "SELECT t.TABLE_NAME FROM information_schema.TABLES t JOIN information_schema.COLUMNS c ON c.TABLE_SCHEMA=t.TABLE_SCHEMA AND c.TABLE_NAME=t.TABLE_NAME WHERE t.TABLE_SCHEMA='mysql'";
    await explainTab(i);
    await G.until(() => $('mPlan').classList.contains('show') && $('planBody').querySelector('.plan, .muted'), 10000);
    G.check('Explain draws the plan', $('planBody').querySelectorAll('.pcard').length >= 2, $('planBody').innerHTML.slice(0, 300));
    G.check('and still gives the EXPLAIN table', T(i).cols && T(i).cols.includes('select_type'), T(i).cols);
    hide('mPlan');
  } finally {
    closeTab(i);
  }
  return G.report();
})()
