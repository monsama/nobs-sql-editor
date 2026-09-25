// A procedure's tab: Run (F5) is Apply, so a new version that fails does not leave the procedure
// gone - on MySQL the tab's SQL drops it first. And a body that ends in a -- comment still applies:
// the delimiter used to follow it on the same line and be taken into the comment.
(async () => {
  const DB = 'nobs_gui_rt';
  const exists = async () => +(await G.one(`SELECT COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA='${DB}' AND ROUTINE_NAME='p'`));
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};`);
    await G.A('/api/script', { sql: `DELIMITER $$\nCREATE PROCEDURE ${DB}.p() BEGIN SELECT 1; END$$\nDELIMITER ;` });
    G.check('the procedure is there to start with', (await exists()) === 1, await exists());

    await openDdl(DB, 'procedure', 'p');
    const id = activeTab, ed = $('ed_' + id);
    G.check('it opens in a tab of its own', !!T(id).ddl && /SELECT 1/.test(ed.value), ed.value.slice(0, 200));
    const good = ed.value;

    ed.value = good.replace('SELECT 1;', 'SELEC broken;');
    await runTab(id);
    await G.until(() => !/Applying|Running/.test($('st_' + id).textContent), 15000);
    G.check('Run with a new version that fails leaves the procedure there', (await exists()) === 1, $('st_' + id).textContent);

    ed.value = good.replace(/END(\s*)\n\$\$/, (m, sp) => 'END -- a note after the body' + sp + '\n$$');
    G.check('the body now ends in a comment', /END -- a note after the body\s*\n\$\$/.test(ed.value), ed.value.slice(-160));
    await runTab(id);
    await G.until(() => /Applied OK|error|ERROR/i.test($('st_' + id).textContent), 15000);
    G.check('and that applies', /Applied OK/.test($('st_' + id).textContent) && (await exists()) === 1, $('st_' + id).textContent);
    closeTab(id);
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
