// The table designer's Indexes, Foreign keys and Options tabs, and its column moves, UNSIGNED and
// collation, against a real server: one Apply that changes all of them at once lands as asked, the
// rows are untouched, and the designer then reads the table again and finds nothing left to do.
(async () => {
  if (typeof dAddIdx !== 'function') { G.skip('the designer tabs', 'this edition has no Indexes tab in the designer yet'); return G.report(); }
  const DB = 'nobs_gui_design';
  const one = sql => G.one(sql, DB);
  const rows = id => [...$(id).children];
  const rowOf = n => rows('dCols').find(tr => tr.querySelector('.dn').value === n);
  const set = (el, v) => { if (el.type === 'checkbox') el.checked = v; else el.value = v; el.dispatchEvent(new Event('change')); };
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB} CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci;
CREATE TABLE ${DB}.p (id INT PRIMARY KEY) ENGINE=InnoDB;
INSERT INTO ${DB}.p VALUES (1),(2);
CREATE TABLE ${DB}.t (id INT NOT NULL AUTO_INCREMENT PRIMARY KEY, a INT, b VARCHAR(20), c INT, KEY ix_a (a), CONSTRAINT fk_c FOREIGN KEY (c) REFERENCES ${DB}.p (id)) ENGINE=InnoDB;
INSERT INTO ${DB}.t (a,b,c) VALUES (5,'x',1),(7,'y',2);`);

    await designTable('t', DB);
    G.eq('an untouched table has nothing to do', $('dSql').value, '-- no changes detected');
    G.eq('its indexes are listed, the one the foreign key made too, the primary key not', dReadIdx().map(i => [i.name, i.kind, i.cols]), [['fk_c', 'INDEX', 'c'], ['ix_a', 'INDEX', 'a']]);
    G.eq('its foreign key is listed', dReadFks().map(f => [f.name, f.cols, f.ref, f.refCols]), [['fk_c', 'c', 'p', 'id']]);
    G.eq('and its options', [$('dEngine').value, $('dColl').value], ['InnoDB', 'utf8mb4_general_ci']);

    // c to the front, a renamed and made unsigned, b given its own collation
    const c = rowOf('c'); dMove(c.querySelector('button'), -1); dMove(c.querySelector('button'), -1); dMove(c.querySelector('button'), -1);
    const a = rowOf('a'); set(a.querySelector('.dn'), 'a2'); set(a.querySelector('.dun'), true);
    set(rowOf('b').querySelector('.dco'), 'latin1_swedish_ci');
    G.eq('the rename reaches the index list', dReadIdx().find(i => i.name === 'ix_a').cols, 'a2');
    dAddIdx({ name: 'uq_b', kind: 'UNIQUE', cols: 'b(5)', comment: 'first five' });
    set(rows('dFks')[0].querySelector('.dfd'), 'CASCADE');
    set($('dTblComment'), 'designed'); set($('dName'), 't2');
    dGen();
    const sql = $('dSql').value;
    G.check('the foreign key is dropped in an ALTER of its own', /^ALTER TABLE \S+\n\s+DROP FOREIGN KEY fk_c;\nALTER TABLE/.test(sql), sql);
    await dApply();
    G.eq('it applies', $('dLog').textContent, 'Applied OK.');

    G.eq('the columns are in the new order', (await G.q(`SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='${DB}' AND TABLE_NAME='t2' ORDER BY ORDINAL_POSITION`)).map(r => r[0]), ['c', 'id', 'a2', 'b']);
    G.check('a2 is unsigned', /unsigned/i.test(await one(`SELECT COLUMN_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='${DB}' AND TABLE_NAME='t2' AND COLUMN_NAME='a2'`)));
    G.eq('b has its collation', await one(`SELECT COLLATION_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='${DB}' AND TABLE_NAME='t2' AND COLUMN_NAME='b'`), 'latin1_swedish_ci');
    G.eq('the new index is there, the old one follows the rename', (await G.q(`SELECT INDEX_NAME, GROUP_CONCAT(COLUMN_NAME ORDER BY SEQ_IN_INDEX), MAX(NON_UNIQUE) FROM information_schema.STATISTICS WHERE TABLE_SCHEMA='${DB}' AND TABLE_NAME='t2' AND INDEX_NAME IN ('ix_a','uq_b') GROUP BY INDEX_NAME ORDER BY INDEX_NAME`)).map(r => r.map(String)), [['ix_a', 'a2', '1'], ['uq_b', 'b', '0']]);
    G.eq('the key cascades on delete now', await one(`SELECT DELETE_RULE FROM information_schema.REFERENTIAL_CONSTRAINTS WHERE CONSTRAINT_SCHEMA='${DB}' AND CONSTRAINT_NAME='fk_c'`), 'CASCADE');
    G.eq('the table is renamed and has its comment', await one(`SELECT TABLE_COMMENT FROM information_schema.TABLES WHERE TABLE_SCHEMA='${DB}' AND TABLE_NAME='t2'`), 'designed');
    G.eq('the rows are as they were', (await G.q(`SELECT c, id, a2, b FROM ${DB}.t2 ORDER BY id`)).map(r => r.map(String)), [['1', '1', '5', 'x'], ['2', '2', '7', 'y']]);
    G.eq('the designer read it again and has nothing left to do', $('dSql').value, '-- no changes detected');

    // A new table with all of it in its CREATE.
    await designTable(null, DB);
    set($('dName'), 'n');
    dAddIdx({ name: 'ix_n', kind: 'INDEX', cols: 'id', comment: '' });
    dAddFk({ name: 'fk_n', cols: 'id', ref: 'p', refCols: 'id', onDel: 'CASCADE', onUpd: '' });
    set($('dColl'), 'utf8mb4_bin');
    set(rows('dCols')[1].querySelector('.dn'), 'v');
    dGen();
    await dApply();
    G.eq('a new table applies', $('dLog').textContent, 'Applied OK.');
    G.eq('with its collation', await one(`SELECT TABLE_COLLATION FROM information_schema.TABLES WHERE TABLE_SCHEMA='${DB}' AND TABLE_NAME='n'`), 'utf8mb4_bin');
    G.eq('and its key', await one(`SELECT DELETE_RULE FROM information_schema.REFERENTIAL_CONSTRAINTS WHERE CONSTRAINT_SCHEMA='${DB}' AND CONSTRAINT_NAME='fk_n'`), 'CASCADE');
    G.eq('and nothing left to do', $('dSql').value, '-- no changes detected');
    G.eq('and the app said nothing was wrong', G.errs(), []);
  } finally {
    if ($('mDesign').classList.contains('show')) hide('mDesign');
    await G.A('/api/script', { sql: `DROP DATABASE IF EXISTS ${DB}` });
  }
  return G.report();
})()
