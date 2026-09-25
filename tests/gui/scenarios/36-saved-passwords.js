// A saved connection's password never comes to the page. The page names the connection and the app
// fills the password in - for the address it was saved for, and nowhere else.
(async () => {
  const N = 'nobs_gui_pw';
  const form = { host: $('host').value, port: $('port').value, user: $('user').value, pass: $('pass').value, list: $('connlist').value };
  try {
    const conn = { ...getConn(), savedName: '', password: form.pass };
    G.check('saved', (await G.A('/api/conn-save', { name: N, conn, savepw: true })).ok);
    const g = await G.A('/api/conn-get', { name: N });
    G.check('reading it back gives whether there is a password, not the password',
      g.ok && g.conn.hasPassword === true && g.conn.password === undefined && !JSON.stringify(g).includes(form.pass), g.conn);

    await refreshConns(); $('connlist').value = N; await pickConn();
    G.eq('picked, the password box stays empty', $('pass').value, '');
    G.eq('and a query still signs in, by the name', await G.one('SELECT 1+1'), '2');

    // The same saved name sent with another address gets no password.
    const other = form.host === '127.0.0.1' ? 'localhost' : '127.0.0.1';
    // Sent straight to the backend, as a script in the page could: the app's own api() always puts
    // in the connection you are connected to.
    const raw = async (path, body) => G.desktop
      ? window.__TAURI__.core.invoke(path.replace('/api/', '').replace(/-/g, '_'), { req: body }).catch(e => ({ ok: false, error: String(e) }))
      : (await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ ...body, token: TOKEN }) })).json();
    const same = await raw('/api/query', { sql: 'SELECT 1', conn: getConn() });
    G.check('sent straight, the name signs in at its own address', same.ok, same);
    const r = await raw('/api/query', { sql: 'SELECT 1', conn: { ...getConn(), host: other } });
    G.check('the name with another host does not bring the password along', !r.ok, r);

    // Saved again pointing somewhere else, with the password box left empty: it is not carried over.
    await G.A('/api/conn-save', { name: N, conn: { ...conn, host: other, password: '' }, savepw: true, keepFrom: N });
    G.eq('a connection moved to another address has its password typed again', (await G.A('/api/conn-get', { name: N })).conn.hasPassword, false);
    // Moved back and saved with the password typed, then saved once more with the box empty: kept.
    await G.A('/api/conn-save', { name: N, conn, savepw: true });
    await G.A('/api/conn-save', { name: N, conn: { ...conn, password: '' }, savepw: true, keepFrom: N });
    G.eq('the same address keeps it when the box is left empty', (await G.A('/api/conn-get', { name: N })).conn.hasPassword, true);
  } finally {
    await G.A('/api/conn-delete', { name: N });
    $('host').value = form.host; $('port').value = form.port; $('user').value = form.user; $('pass').value = form.pass;
    await refreshConns(); $('connlist').value = form.list;
    G.take();
  }
  return G.report();
})()
