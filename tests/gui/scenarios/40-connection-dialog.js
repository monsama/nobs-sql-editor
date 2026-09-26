// The connection dialog (Save and Edit connection): it shows what applies - the CA only for the modes
// that check one, the SSH fields only with a tunnel - Test tries the values in the dialog, a saved
// password left untouched included, and Save and Cancel do what they say.
(async () => {
  const N = 'nobs_gui_dialog';
  const form = { host: $('host').value, port: $('port').value, user: $('user').value, pass: $('pass').value, list: $('connlist').value };
  const shown = id => { const e = $(id); return !!e && e.offsetParent !== null && getComputedStyle(e).visibility !== 'hidden'; };
  try {
    const sv = await G.A('/api/conn-save', { name: N, conn: { ...getConn(), savedName: '', password: form.pass }, savepw: true, env: 'test' });
    G.check('a connection to edit', sv.ok, sv);
    await refreshConns(); $('connlist').value = N; await pickConn();

    const done = editConn();
    await G.until(() => $('mConn').classList.contains('show'), 5000);
    G.eq('it opens with the connection in it', [$('cd_name').value, $('cd_host').value, $('cd_user').value, $('cd_env').value], [N, form.host, form.user, 'test']);
    // side by side while the window is wide enough (one column below 720 px)
    const w = document.documentElement.clientWidth, cols = getComputedStyle(document.querySelector('#mConn .cdrow')).gridTemplateColumns.split(' ').length;
    G.check('pairs side by side in a wide window', w <= 720 ? cols === 1 : cols === 2, { w, cols });
    G.check('the saved password is not in the page, and shows as saved', $('cd_pass').value === '' && $('cd_pass').classList.contains('pwsaved') && $('cd_savepw').checked);
    $('cd_ssl').value = 'default'; cdSync();
    G.check('SSL default: no CA field, and it says what default does', !shown('cd_caWrap') && /not checked/.test($('cd_sslNote').textContent), $('cd_sslNote').textContent);
    $('cd_ssl').value = 'verify-ca'; cdSync();
    G.check('SSL verify-ca: the CA field shows', shown('cd_caWrap'));
    $('cd_ssl').value = 'disabled'; cdSync();
    G.check('SSL disabled: no PAM box, the password is never sent as typed there', !shown('cd_pamWrap'));
    $('cd_ssl').value = 'default'; cdSync();
    G.check('no tunnel: no SSH fields', !shown('cd_ssh'));
    $('cd_useSsh').checked = true; cdSync();
    G.check('a tunnel: its fields show', shown('cd_ssh') && shown('cd_sshHost'));
    $('cd_useSsh').checked = false; cdSync();

    // Test, with the saved password untouched
    await cdTest(); await G.until(() => /^(Connected|.)/.test($('cdMsg').textContent) && $('cdMsg').textContent !== 'Testing...', 20000);
    G.check('Test connects with the saved password, not saving anything', /^Connected/.test($('cdMsg').textContent), $('cdMsg').textContent);
    const wrongUser = $('cd_user').value; $('cd_user').value = 'nobs_no_such_user';
    await cdTest(); await G.until(() => $('cdMsg').textContent !== 'Testing...', 20000);
    G.check('and says why when it cannot', $('cdMsg').classList.contains('err') && $('cdMsg').textContent.length > 0, $('cdMsg').textContent);
    $('cd_user').value = wrongUser;

    // Save
    $('cd_env').value = 'changed'; $('cd_ro').checked = true;
    cdOk(); await done;
    const list = await G.A('/api/conn-list');
    const it = (list.items || []).find(c => c.name === N);
    G.check('Save stores what the dialog says, and keeps the password', it && it.env === 'changed' && it.readonly === true && it.hasPassword === true, it);

    // Esc cancels
    const again = editConn();
    await G.until(() => $('mConn').classList.contains('show'), 5000);
    $('cd_env').value = 'not kept';
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await again;
    G.check('Esc closes it and saves nothing', !$('mConn').classList.contains('show') && connMeta()[N].env === 'changed', connMeta()[N]);

  } finally {
    if (_cd) cdClose(null);
    await G.A('/api/conn-delete', { name: N });
    $('host').value = form.host; $('port').value = form.port; $('user').value = form.user; $('pass').value = form.pass;
    await refreshConns(); $('connlist').value = form.list;
    window.readOnly = false; document.body.classList.remove('ro');
    G.take();
  }
  return G.report();
})()
