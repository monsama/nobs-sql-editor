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

    // The sidebar: every saved connection, the one shown marked; its menu (the ⋯ button) duplicates it -
    // password, environment and read-only included - and shows the copy, or deletes it; a click loads another.
    const sideMenu = name => { [...document.querySelectorAll('#cd_side .cdsitem')].find(i => i.dataset.name === name).querySelector('.cdsdup').click();
      return [...document.querySelectorAll('#ctx > .item')]; };
    const pick = (its, label) => its.find(d => d.textContent === label);
    const side = editConn();
    await G.until(() => $('mConn').classList.contains('show'), 5000);
    const items = [...document.querySelectorAll('#cd_side .cdsitem')];
    G.check('the sidebar lists the saved connections, the one shown marked', shown('cd_side') && items.length === connNames().length
      && (document.querySelector('#cd_side .cdsitem.on') || {}).dataset?.name === N, items.map(i => i.dataset.name));
    const NC = N + ' (copy)';
    const it0 = items.find(i => i.dataset.name === N);
    const m0 = sideMenu(N);
    G.eq('its menu offers Duplicate and Delete', m0.map(d => d.textContent), ['Duplicate', 'Delete...']);
    G.check('and is a menu, not a copy made at once', !it0.querySelector('svg') && connNames().indexOf(NC) < 0, connNames());
    pick(m0, 'Duplicate').click();
    await G.until(() => _cd && _cd.savedName === NC, 10000);
    const copied = ((await G.A('/api/conn-list')).items || []).find(c => c.name === NC);
    G.check('duplicate saves a copy with the password, environment and read-only', copied && copied.hasPassword === true && copied.env === 'changed' && copied.readonly === true, copied);
    G.eq('and shows it, in the fields and the sidebar', [$('cd_name').value, (document.querySelector('#cd_side .cdsitem.on') || {}).dataset?.name], [NC, NC]);
    G.eq('the connection box keeps what it had picked', $('connlist').value, N);
    await cdPick(N);
    G.eq('a click on another loads it', [_cd && _cd.savedName, $('cd_name').value], [N, N]);
    // Delete from the menu: the copy goes, from the saved ones and the sidebar, and the one shown stays.
    pick(sideMenu(NC), 'Delete...').click();
    await G.until(() => !connNames().includes(NC), 10000);
    G.check('Delete removes it, from the sidebar too', !document.querySelector('#cd_side .cdsitem[data-name="' + CSS.escape(NC) + '"]'), connNames());
    G.eq('and the connection shown stays', [_cd && _cd.savedName, $('connlist').value], [N, N]);
    cdClose(null); await side;
    await G.A('/api/conn-delete', { name: NC }); await refreshConns(); $('connlist').value = N;


    // Connected through the saved connection, then renamed in Edit: the connection you are on goes on
    // under the new name. It went on under the old one and was refused ("using password: NO").
    await refreshConns(); $('connlist').value = N; setPass(''); await connect();
    await G.until(() => !document.body.classList.contains('disconnected') && window._activeConnName === N, 15000);
    G.eq('connected through the saved connection', window._activeConnName, N);
    const N2 = N + '_renamed';
    const ren = editConn(); await G.until(() => $('mConn').classList.contains('show'), 5000);
    $('cd_name').value = N2; cdOk(); await ren;
    G.eq('renamed, the connection you are on goes by the new name', window._activeConnName, N2);
    G.eq('and still signs in', await G.one('SELECT 1+1'), '2');
    $('connlist').value = N2; await delConn();
    G.check('deleting it closes the connection, rather than leaving it to fail', document.body.classList.contains('disconnected'));
  } finally {
    if (_cd) cdClose(null);
    await G.A('/api/conn-delete', { name: N + '_renamed' });
    await G.A('/api/conn-delete', { name: N + ' (copy)' });
    await G.A('/api/conn-delete', { name: N });
    $('host').value = form.host; $('port').value = form.port; $('user').value = form.user; $('pass').value = form.pass;
    await refreshConns(); $('connlist').value = form.list;
    // Back on the connection the run started with, as it was made: typed, not a saved one.
    if (document.body.classList.contains('disconnected') || window._activeConnName !== form.list) {
      $('connlist').value = form.list; setPass(form.pass); await connect();
      await G.until(() => !document.body.classList.contains('disconnected'), 15000);
    }
    window.readOnly = false; window._activeReadOnly = false; document.body.classList.remove('ro');
    G.take();
  }
  return G.report();
})()
