// Inspect: a table's details, its indexes with their columns together and in order, its foreign
// keys, and the tables that point at it - a click on one of those inspects it.
(async () => {
  const DB = 'nobs_gui_insp';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.parent (id INT PRIMARY KEY, a INT, b INT, KEY ab (b, a)) ENGINE=InnoDB;
CREATE TABLE ${DB}.child (id INT PRIMARY KEY, pid INT, CONSTRAINT fk_parent FOREIGN KEY (pid) REFERENCES ${DB}.parent (id)) ENGINE=InnoDB;`);
    await inspect(DB, 'parent');
    await G.until(() => $('inspRef').querySelector('.insplink') && $('inspIdx').querySelector('.utab'), 15000);
    G.check('the window names the table and its engine', $('inspTitle').textContent === 'parent' && /innodb/i.test($('inspSub').textContent), [$('inspTitle').textContent, $('inspSub').textContent]);
    const idx = [...$('inspIdx').querySelectorAll('tbody tr')].map(tr => [...tr.children].map(td => td.innerText.replace(/\s+/g, ' ').trim()));
    G.check('an index of two columns is one row, its columns in order', idx.some(r => r[0] === 'ab' && /^b\s*a$/.test(r[1])) && idx.some(r => r[0] === 'PRIMARY' && /primary key/i.test(r[2])), idx);
    G.check('the details say how it is kept', /Engine/.test($('inspInfo').innerText) && /Index size/.test($('inspInfo').innerText), $('inspInfo').innerText);
    G.check('the table that points at it is listed', /child/.test($('inspRef').innerText) && /fk_parent/.test($('inspRef').innerText), $('inspRef').innerText);
    $('inspRef').querySelector('.insplink').click();
    await G.until(() => $('inspTitle').textContent === 'child' && $('inspFk').querySelector('.utab'), 15000);
    G.check('a click inspects it, with its key pointing back', /fk_parent/.test($('inspFk').innerText) && /parent/.test($('inspFk').innerText), $('inspFk').innerText);
  } finally {
    hide('mInspect');
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
