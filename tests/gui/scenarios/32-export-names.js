// One file per database never writes two databases into the same file: "nobs gui x" and
// "nobs_gui_x" both become nobs_gui_x in a file name, and the second export used to replace the
// first. The second now gets a file of its own.
(async () => {
  const A = 'nobs_gui_x', B = 'nobs gui x';
  const FOLDER = G.env.tmp + '/export-names';
  try {
    await G.run(`DROP DATABASE IF EXISTS \`${A}\`; DROP DATABASE IF EXISTS \`${B}\`; CREATE DATABASE \`${A}\`; CREATE DATABASE \`${B}\`;
CREATE TABLE \`${A}\`.one (id INT PRIMARY KEY); CREATE TABLE \`${B}\`.two (id INT PRIMARY KEY);`);
    await openExport(); await G.wait(800);
    for (const c of document.querySelectorAll('.expdb')) c.checked = (c.value === A || c.value === B);
    $('expFolder').value = FOLDER; $('expStamp').checked = false; $('expPer').checked = true;
    await runExport();
    await G.until(() => /OK |FAILED/.test($('expLog').textContent), 60000);
    await G.wait(1000);
    const files = await G.A('/api/browse', { path: FOLDER, filter: '*.sql', dirsOnly: false });
    const names = (files.files || []).map(f => f.name).sort();
    G.eq('two databases whose names make the same file name get a file each', names, ['nobs_gui_x.sql', 'nobs_gui_x_2.sql']);
    G.check('and both exports say OK', ($('expLog').textContent.match(/OK /g) || []).length >= 2, $('expLog').textContent.slice(-400));

    // A dump that fails leaves no file that looks complete: the database is gone by the time the
    // export runs, so the dump fails part way.
    const FAILDIR = G.env.tmp + '/export-failed';
    for (const c of document.querySelectorAll('.expdb')) c.checked = (c.value === A);
    $('expFolder').value = FAILDIR;
    await G.run(`DROP DATABASE \`${A}\``);
    await runExport();
    await G.until(() => /OK |FAILED/.test($('expLog').textContent), 60000);
    await G.wait(800);
    const left = ((await G.A('/api/browse', { path: FAILDIR, filter: '*.sql', dirsOnly: false })).files || []).map(f => f.name);
    G.check('a failed dump says so and leaves no finished-looking file', /FAILED/.test($('expLog').textContent) && !left.includes('nobs_gui_x.sql'), { log: $('expLog').textContent.slice(-300), left });
  } finally {
    hide('mExport');
    await G.run(`DROP DATABASE IF EXISTS \`${A}\`; DROP DATABASE IF EXISTS \`${B}\`;`);
  }
  return G.report();
})()
