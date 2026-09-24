// The Users dialog: an account made with its settings, privileges ticked and applied, a role given
// as the default, account settings changed, a clone, and who has access to a database - each
// checked against what the server then says. Its accounts and role are removed at the end.
(async () => {
  const maria = !!window.mariadb;
  const role = maria ? 'nobs_ue_role' : "'nobs_ue_role'@'%'";
  const realInput = inputBox, realAsk = ask;
  const answers = [];
  inputBox = async o => answers.shift()(o);
  ask = async () => true;
  const cu = async a => String((await G.q('SHOW CREATE USER ' + a))[0][0]);
  const grants = async a => (await G.q('SHOW GRANTS FOR ' + a)).map(r => r[0]).join('\n');
  const cleanup = async () => {
    for (const a of ["'nobs_ue_a'@'%'", "'nobs_ue_clone'@'%'"]) await G.A('/api/script', { sql: 'DROP USER IF EXISTS ' + a });
    await G.A('/api/script', { sql: 'DROP ROLE IF EXISTS ' + role });
  };
  // MySQL 5.7 has no roles, and MariaDB before 10.4 neither password expiry per account nor locking:
  // the dialogs leave those out there, and so does this.
  const C = await G.caps();
  try {
    await cleanup();
    answers.push(() => ({ user: 'nobs_ue_a', host: '%', pw: 'Ue-pw-1', plugin: '', more: true, ssl: 'NONE', exp: 'never', days: '90', mq: '100', mu: '0', mc: '0', muc: '5' }));
    await openUsers(); await newUser();
    const made = await cu("'nobs_ue_a'@'%'");
    G.check('Create user makes it with its limits and expiry', /MAX_QUERIES_PER_HOUR 100/.test(made) && (!C.expire || /PASSWORD EXPIRE NEVER/i.test(made)), made);
    G.check('and its details say so', /at most 100 queries an hour/.test($('userInfo').textContent) && (!C.expire || /never expires/.test($('userInfo').textContent)), $('userInfo').textContent);

    await privOpen(); $('privScope').value = 'db'; $('privDb').value = 'nobs_test'; await privScopeChanged();
    for (const p of ['SELECT', 'INSERT']) $('privList').querySelector('input[value="' + p + '"]').click();
    // "_" in a database-level grant is a wildcard, so the database is named exactly: nobs\_test.
    G.check('ticked privileges show as the GRANT they become', /^GRANT SELECT, INSERT ON `?nobs\\_test`?\.\* TO 'nobs_ue_a'@'%';$/.test($('privSql').textContent), $('privSql').textContent);
    await privApply();
    $('privList').querySelector('input[value="INSERT"]').click();
    await privApply();
    const g = await grants("'nobs_ue_a'@'%'");
    G.check('and applying them grants and revokes', /GRANT SELECT ON `nobs\\_test`\.\*/.test(g) && !/INSERT/.test(g), g);
    hide('mPriv');

    if (C.roles) {
      answers.push(() => ({ n: 'nobs_ue_role' }));
      await roleCreate();
      G.check('Create role makes a role', _uaccts.some(a => a.u === 'nobs_ue_role' && a.role), _uaccts.map(a => a.u).join(','));
      usersSelect('nobs_ue_a', '%');
      answers.push(o => { const r = {}; o.fields.forEach(f => { r[f.key] = f.type === 'checkbox' ? f.label.startsWith('nobs_ue_role') : f.value; }); r.def = o.fields.find(f => f.key === 'def').options.find(x => x.label.startsWith('nobs_ue_role')).value; return r; });
      await rolesEdit();
      G.check('Roles gives the role as the default', /nobs_ue_role(@%)? \(default\)/.test($('userInfo').textContent), $('userInfo').textContent);
    } else {
      G.check('a server without roles is not offered Create role', !_uRoleSupport, 'roles were found on ' + C.version);
    }

    answers.push(o => { const r = {}; o.fields.forEach(f => { r[f.key] = f.value; }); r.mq = '0'; r.ssl = 'ANY'; if ('locked' in r) r.locked = true; return r; });
    await acctEdit();
    const edited = await cu("'nobs_ue_a'@'%'");
    G.check('Account settings: SSL required, limit gone, locked', /REQUIRE SSL/i.test(edited) && !/MAX_QUERIES_PER_HOUR 100/.test(edited) && (!C.lock || /ACCOUNT LOCK/.test(edited)), edited);

    answers.push(() => ({ user: 'nobs_ue_clone', host: '%', pw: 'Clone-pw-2' }));
    await acctClone();
    const cg = await grants("'nobs_ue_clone'@'%'");
    G.check('Clone copies the grants and the role', /GRANT SELECT ON `nobs\\_test`\.\*/.test(cg) && (!C.roles || /nobs_ue_role/.test(cg)), cg);
    const signIn = await G.A('/api/connect', { conn: { ...getConn(), user: 'nobs_ue_clone', password: 'Clone-pw-2', ssl: 'required' } });
    G.check('and the clone signs in with its own password', signIn.ok, signIn.error);

    answers.push(() => ({ db: 'nobs_test' }));
    await whoHasAccess();
    G.check('Who has access lists the accounts', /nobs_ue_a/.test($('vText').value) && /nobs_ue_clone/.test($('vText').value), $('vText').value.slice(0, 200));
    hide('mView');
  } finally {
    inputBox = realInput; ask = realAsk;
    hide('mUsers');
    await cleanup();
  }
  return G.report();
})()
