// Settings shows the tools for MariaDB servers and the tools for MySQL servers apart, each with its
// status and download, and marks the set the connected server uses.
(async () => {
  await openSettings();
  setPage('tools');
  const status = $('cfgStatus');
  await G.until(() => /connected server is/.test(status.textContent) || /Could not tell/.test(status.textContent), 30000);
  const maria = $('cfgCardMaria'), mysql = $('cfgCardMysql');
  G.check('two cards, one for each kind of server', maria && mysql && /For MariaDB servers/.test(maria.innerText) && /For MySQL servers/.test(mysql.innerText), [maria && maria.innerText.slice(0, 40), mysql && mysql.innerText.slice(0, 40)]);
  G.check('each card has its own paths and download', maria.contains($('cfgMysql')) && maria.contains($('cfgDump')) && /Download MariaDB client tools/.test(maria.innerText)
    && mysql.contains($('cfgMysqlMy')) && mysql.contains($('cfgDumpMy')) && /Download MySQL client tools/.test(mysql.innerText), 'fields or buttons in the wrong card');
  // each path row says what it comes to, in a chip beside the box - the paths are not listed twice
  const chip = id => $('stc_' + id);
  G.check('the MariaDB card shows its tools, beside their boxes', ['cfgMysql', 'cfgDump'].every(id => chip(id) && chip(id).textContent && /(^| )(ok|bad)( |$)/.test(chip(id).className)), ['cfgMysql', 'cfgDump'].map(id => chip(id) && chip(id).className + ' ' + chip(id).textContent));
  G.check('and there is no second list of them', !$('cfgStatusMaria') && !$('cfgStatusMysql'), 'a status panel is still there');
  const serverIsMaria = /MariaDB/i.test(await G.one('SELECT VERSION()'));
  const mysqlTools = !chip('cfgMysqlMy').classList.contains('none') && !chip('cfgDumpMy').classList.contains('none');
  const inUse = (!serverIsMaria && mysqlTools) ? mysql : maria;
  G.check('the card the connected server uses is marked, and only that one', inUse.classList.contains('inuse') && !(inUse === maria ? mysql : maria).classList.contains('inuse'),
    { status: status.textContent, maria: maria.className, mysql: mysql.className });
  G.check('and the line above says so', new RegExp('connected server is ' + (serverIsMaria ? 'MariaDB' : 'MySQL')).test(status.textContent), status.textContent);

  // One page at a time, picked on the left, in a window that keeps its size from page to page.
  const box = $('mSettings').querySelector('.box'), size = () => [Math.round(box.offsetWidth), Math.round(box.offsetHeight)];
  const shown = () => [...document.querySelectorAll('#mSettings .setpage')].filter(s => s.offsetParent).map(s => s.dataset.p);
  const save = document.querySelector('#mSettings .setfoot .go');
  const at = size();
  G.eq('Client tools shows alone', shown(), ['tools']);
  G.check('with Save', getComputedStyle(save).visibility === 'visible', save.style.visibility);
  setPage('general');
  G.eq('General shows alone', shown(), ['general']);
  G.check('holding the update check and the message timing', $('cfgUpdateCheck').offsetParent && $('cfgToastMs').offsetParent, 'not on the page');
  G.check('without Save, which is for the tool paths', getComputedStyle(save).visibility === 'hidden', save.style.visibility);
  G.eq('the window keeps its size', size(), at);
  setPage('data');
  G.eq('Local data shows alone', shown(), ['data']);
  G.eq('and the window still keeps its size', size(), at);
  // where things are kept: settings in the roaming AppData, downloaded programs in the local one
  await G.until(() => $('cfgPathConfig').textContent && $('cfgPathTools').textContent, 20000);
  G.check('Local data says where the settings and the downloaded tools are', /config\.json$/i.test($('cfgPathConfig').textContent) && /\\Local\\/i.test($('cfgPathTools').textContent) && /\\bin$/i.test($('cfgPathTools').textContent),
    [$('cfgPathConfig').textContent, $('cfgPathTools').textContent]);
  G.check('with a way to open each folder', [...document.querySelectorAll('#mSettings .setpage[data-p="data"] button')].filter(b => b.textContent === 'Open folder').length === 2, 'Open folder buttons');
  G.check('the menu marks the page shown', document.querySelector('#mSettings .setnav button.on').dataset.p === 'data', document.querySelector('#mSettings .setnav button.on').dataset.p);
  setPage('tools');

  // Save tries a path before it keeps it: one that is not there, or is the other tool, is refused
  // with the reason, and Settings stays open with the field marked.
  const was = $('cfgMysql').value, wasDump = $('cfgDump').value;
  const said = []; const realToast = toast; toast = (m, bad) => said.push([m, bad]);
  try {
    $('cfgMysql').value = 'C:\\no\\such\\folder\\mysql.exe'; await saveSettings();
    G.check('a path with no file is not saved', $('mSettings').classList.contains('show') && $('cfgMysql').classList.contains('bad') && said.some(([m, b]) => b && /Not saved.*no file/.test(m)), said);
    // any other program is refused in either box
    const np = 'C:\\Windows\\System32\\notepad.exe';
    const n1 = await G.A('/api/check-tool', { path: np, kind: 'mysql' }), n2 = await G.A('/api/check-tool', { path: np, kind: 'mysqldump' });
    G.check('another program is refused in both boxes', !!n1.error && !!n2.error && !n1.version && !n2.version, [n1, n2]);
    const st = await G.A('/api/tools-status', {});
    if (st.mysqldump && st.mysqldump !== '(not found)') {
      said.length = 0; $('cfgMysql').value = st.mysqldump; await saveSettings();
      G.check('mysqldump in the mysql box is not saved either', $('cfgMysql').classList.contains('bad') && said.some(([m]) => /not mysql\.exe/.test(m)), said);
      const ok = await G.A('/api/check-tool', { path: st.mysqldump, kind: 'mysqldump' });
      G.check('and the real one passes, with its version', ok.ok && !ok.error && /MariaDB|MySQL/.test(ok.version || ''), ok);
    }
  } finally { toast = realToast; $('cfgMysql').value = was; $('cfgDump').value = wasDump; $('cfgMysql').classList.remove('bad'); }
  hide('mSettings');
  return G.report();
})()
