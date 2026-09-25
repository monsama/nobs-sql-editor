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
    for (const a of ["'nobs_ue_a'@'%'", "'nobs_ue_clone'@'%'", "'nobs_ue_moved'@'localhost'"]) await G.A('/api/script', { sql: 'DROP USER IF EXISTS ' + a });
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
    await G.until(() => $('uPrivs').querySelector('.utab'), 5000);
    const pr = [...$('uPrivs').querySelectorAll('.utab tbody tr')].map(tr => tr.innerText.replace(/\s+/g, ' ').trim());
    G.eq('the account shows the privileges as a table: where, what, and whether it may pass them on', pr, ['Database nobs_test SELECT no']);

    // A click in the list - not only a selection made in code - makes every action available.
    const item = [...$('userSel').querySelectorAll('.uitem')].find(d => d.title === 'nobs_ue_a@%');
    usersSelect('nobs_ue_clone_none', '%');
    item.click(); await G.wait(300);
    const acts = ['uRenameBtn', 'uDropBtn', 'uPrivBtn', 'uLockBtn'].map(id => $(id)).filter(b => b.offsetParent);
    G.check('a clicked account can be edited', window._selAcct && window._selAcct.u === 'nobs_ue_a' && acts.length >= 3 && acts.every(b => !b.disabled), acts.map(b => b.id + ':' + b.disabled));
    G.check('and its settings show', /Sign-in method/.test($('userInfo').textContent), $('userInfo').textContent);

    if (C.roles) {
      answers.push(() => ({ n: 'nobs_ue_role' }));
      await roleCreate();
      G.check('Create role makes a role', _uaccts.some(a => a.u === 'nobs_ue_role' && a.role), _uaccts.map(a => a.u).join(','));
      usersSelect('nobs_ue_a', '%');
      answers.push(o => { window._rolesFields = o.fields; const r = {}; o.fields.forEach(f => { r[f.key] = f.type === 'checkbox' ? f.label.startsWith('nobs_ue_role') : f.value; }); r.def = o.fields.find(f => f.key === 'def').options.find(x => x.label.startsWith('nobs_ue_role')).value; return r; });
      await rolesEdit();
      G.check('each role says what ticking it does', window._rolesFields.filter(f => f.type === 'checkbox').every(f => /^Give .+ to .+SET ROLE/.test(f.title || '')) && /SET ROLE/.test(window._rolesFields.find(f => f.key === 'def').title || ''), window._rolesFields.map(f => f.title));
      G.check('Roles gives the role as the default', /nobs_ue_role(@%)?default/.test($('uRoles').textContent), $('uRoles').textContent);
      usersSelect('nobs_ue_role', maria ? '' : '%');
      G.check('and the role lists who has it', $('uRolesH').textContent === 'Given to' && /nobs_ue_a@%/.test($('uRoles').textContent), $('uRoles').textContent);
      usersSelect('nobs_ue_a', '%');
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

    answers.push(() => ({ user: 'nobs_ue_moved', host: 'localhost' }));
    usersSelect('nobs_ue_clone', '%'); await acctRename();
    const moved = await G.q("SELECT User, Host FROM mysql.user WHERE User LIKE 'nobs\\_ue\\_%' ORDER BY User");
    G.check('Rename moves the account, its grants with it', moved.some(r => r[0] === 'nobs_ue_moved' && r[1] === 'localhost') && !moved.some(r => r[0] === 'nobs_ue_clone')
      && /nobs\\_test/.test(await grants("'nobs_ue_moved'@'localhost'")), moved);
    G.check('and shows it under its new name', window._selAcct && uName(window._selAcct) === 'nobs_ue_moved@localhost', window._selAcct && uName(window._selAcct));

    await whoHasAccess('nobs_test');
    await G.until(() => $('accList').querySelector('.utab'), 10000);
    const acc = $('accList').innerText;
    G.check('Who has access lists the accounts, with where they may act', /nobs_ue_a/.test(acc) && /nobs_ue_moved/.test(acc) && /database/i.test(acc) && /SELECT/.test(acc), acc.slice(0, 300));
    $('accFilter').value = 'moved'; accRender();
    G.check('and the filter narrows them', /nobs_ue_moved/.test($('accList').innerText) && !/nobs_ue_a@/.test($('accList').innerText), $('accList').innerText.slice(0, 200));
    [...$('accList').querySelectorAll('.acclink')].find(a => a.dataset.u === 'nobs_ue_moved').click();
    await G.until(() => window._selAcct && window._selAcct.u === 'nobs_ue_moved', 10000);
    G.check('a click on an account opens it in Users', !$('mAccess').classList.contains('show') && $('mUsers').classList.contains('show') && window._selAcct.u === 'nobs_ue_moved', window._selAcct && uName(window._selAcct));
  } finally {
    inputBox = realInput; ask = realAsk;
    hide('mUsers');
    await cleanup();
  }
  return G.report();
})()
