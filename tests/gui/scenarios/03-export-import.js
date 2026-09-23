// A database exported through the dialog imports into another one unchanged - whole, and as
// structure only followed by data only - and the CSV import dialog stores a file exactly. Uses
// nobs_gui from 01 and 02, and drops what they made.
(async () => {
  const DB = 'nobs_gui', IMP = 'nobs_gui_imp', PARTS = 'nobs_gui_parts';
  const FOLDER = G.env.tmp + '/export';
  try {
    const sumGp = db => G.one(`SELECT GROUP_CONCAT(CONCAT_WS('|',id,IFNULL(HEX(t),'N'),IFNULL(HEX(b),'N'),IFNULL(HEX(n),'N'),IFNULL(bits+0,'N'),IFNULL(HEX(l1),'N')) ORDER BY id SEPARATOR ';') FROM ${db}.gp`);
    const sumCg = db => G.one(`SELECT GROUP_CONCAT(CONCAT_WS('|',HEX(id),IFNULL(HEX(t),'N'),IFNULL(HEX(b),'N'),IFNULL(HEX(x),'N')) ORDER BY id SEPARATOR ';') FROM ${db}.cg`);
    await G.run(`DROP DATABASE IF EXISTS ${IMP}`); await G.run(`DROP DATABASE IF EXISTS ${PARTS}`);
    const gp0 = await sumGp(DB), cg0 = await sumCg(DB);

    curSchema = DB;
    await openExport(); await G.wait(800);
    for (const c of document.querySelectorAll('.expdb')) c.checked = (c.value === DB);
    $('expFolder').value = FOLDER; $('expStamp').checked = false; $('expPer').checked = true;
    G.take(); await runExport(); await G.until(() => /OK /.test($('expLog').textContent) || /FAILED/.test($('expLog').textContent), 60000);
    G.check('the export writes its file', /OK /.test($('expLog').textContent) && !/FAILED/.test($('expLog').textContent), $('expLog').textContent);
    const files = await G.A('/api/browse', { path: FOLDER, filter: '*.sql', dirsOnly: false });
    const paths = (files.files || []).map(f => f.path);
    G.eq('one file for the database', (files.files || []).map(f => f.name), ['nobs_gui.sql']);
    // With no file there is nothing to import, and carrying on makes the run say that a table is
    // missing from a database two scenarios later - which is the symptom, three steps removed from
    // the cause. It has happened once in CI, and reading it took longer than it should have. Stop
    // where it went wrong instead, carrying what the app itself said about the export.
    if (!paths.length) {
      const said = G.take();
      G.check('the export left a file to import', false,
        { folder: FOLDER, expLog: $('expLog').textContent.slice(-400), toasts: said.t.slice(-6), log: said.l.slice(-6) });
      hide('mExport');
      return G.report();
    }
    hide('mExport');

    await openImport(); await G.wait(500);
    await impAppend(paths);
    G.eq('the file list shows the file', [...$('impList').querySelectorAll('.impname')].map(e => e.textContent), ['nobs_gui.sql']);
    $('impDb').value = IMP; $('impCreate').checked = true;
    await runImport(); await G.until(() => /OK /.test($('impLog').textContent) || /FAILED/.test($('impLog').textContent), 60000);
    G.check('it imports into another database', /OK /.test($('impLog').textContent) && !/FAILED/.test($('impLog').textContent), $('impLog').textContent);
    hide('mImport');
    G.eq('with every value of the first table', await sumGp(IMP), gp0);
    G.eq('and of the second', await sumCg(IMP), cg0);

    // Structure only, one file for the database; then data only, a file per table. Imported in
    // that order into a new database, they give back what the whole export did.
    const exportAs = async (what, folder, mode) => {
      await openExport(); await G.wait(800);
      for (const c of document.querySelectorAll('.expdb')) c.checked = (c.value === DB);
      $('expFolder').value = folder; $('expStamp').checked = false; $(mode).checked = true;
      $(what).checked = true; expWhatChanged();
      await runExport(); await G.until(() => /OK /.test($('expLog').textContent) || /FAILED/.test($('expLog').textContent), 60000);
      const log = $('expLog').textContent; hide('mExport');
      return { log, files: ((await G.A('/api/browse', { path: folder, filter: '*.sql', dirsOnly: false })).files || []).map(f => f.path) };
    };
    await openExport(); await G.wait(800);
    $('expWhatData').checked = true; expWhatChanged();
    G.check('data only greys what would create or drop', ['routines', 'events', 'createdb', 'adddroptb'].every(k => $('eo_' + k).disabled) && !$('eo_hexblob').disabled, '');
    hide('mExport');
    const st = await exportAs('expWhatStructure', FOLDER + '-structure', 'expPer');
    G.check('structure only writes its file', st.files.length === 1 && !/FAILED/.test(st.log), st.log);
    const da = await exportAs('expWhatData', FOLDER + '-data', 'expTable');
    G.check('data only writes a file per table', da.files.length >= 2 && !/FAILED/.test(da.log), da);
    await openImport(); await G.wait(300);
    await impAppend(st.files); $('impDb').value = PARTS; $('impCreate').checked = true;
    await runImport(); await G.until(() => /OK /.test($('impLog').textContent) || /FAILED/.test($('impLog').textContent), 60000);
    G.eq('the structure makes the tables, empty', String(await G.one(`SELECT COUNT(*) FROM ${PARTS}.gp`)), '0');
    impClear(); await impAppend(da.files); $('impCreate').checked = false;
    await runImport(); await G.until(() => /OK /.test($('impLog').textContent) || /FAILED/.test($('impLog').textContent), 60000);
    hide('mImport');
    G.eq('and the data fills them', await sumGp(PARTS), gp0);
    G.eq('both of them', await sumCg(PARTS), cg0);

    await G.run(`DROP TABLE IF EXISTS ${DB}.csv_in; CREATE TABLE ${DB}.csv_in LIKE ${DB}.gp`);
    importCsv(DB, 'csv_in'); await G.wait(300);
    $('csvFile').value = G.env.tmp + '/gp.csv'; $('csvHeader').checked = true;
    if ($('csvNullVal')) $('csvNullVal').value = '\\N';
    $('csvReplace').checked = false;
    await runCsvImport(); await G.until(() => /Imported|rror/.test($('csvLog').textContent), 30000);
    G.check('the CSV import reports its rows', /Imported 3 row/.test($('csvLog').textContent), $('csvLog').textContent);
    G.eq('and stores the file exactly', await G.one(`SELECT GROUP_CONCAT(CONCAT_WS('|',id,IFNULL(HEX(t),'N'),IFNULL(HEX(b),'N'),IFNULL(HEX(n),'N'),IFNULL(bits+0,'N'),IFNULL(HEX(l1),'N')) ORDER BY id SEPARATOR ';') FROM ${DB}.csv_in`),
      '1|6C696E65310D0A6C696E6532|DEAD|4E554C4C|1|E9;2|||N|3|N;3|78|N|30783432|N|4772FCDF65');
    hide('mCsv');
  } finally {
    curSchema = null;
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}; DROP DATABASE IF EXISTS ${IMP}; DROP DATABASE IF EXISTS ${PARTS}; DROP DATABASE IF EXISTS nobs_gui_t` });
  }
  return G.report();
})()
