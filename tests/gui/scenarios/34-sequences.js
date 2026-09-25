// MariaDB's sequences have a group of their own in the object list, open like a table and can be
// dropped from their menu. MySQL has no sequences: there the list is empty.
(async () => {
  const DB = 'nobs_gui_seq';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB}; CREATE TABLE ${DB}.t (id INT PRIMARY KEY);`);
    const maria = /mariadb/i.test(await G.one('SELECT VERSION()'));
    if (maria) await G.run(`CREATE SEQUENCE ${DB}.s START WITH 10`);
    await loadObjects(DB);
    await G.until(() => objData && objData.db === DB, 10000);
    if (!maria) {
      G.check('MySQL: no sequences, and the list says so', Array.isArray(objData.r.sequences) && objData.r.sequences.length === 0, objData.r.sequences);
    } else {
      G.eq('the sequence is listed, apart from the tables', [objData.r.sequences, objData.r.tables], [['s'], ['t']]);
      objMenu({ clientX: 40, clientY: 40 }, DB, 'sequence', 's');
      const m = [...document.querySelectorAll('#ctx > .item')].map(d => d.textContent);
      $('ctx').style.display = 'none';
      G.check('its menu opens, shows and drops it', ['Open', 'Show CREATE', 'Drop sequence...'].every(x => m.includes(x)), m);
      objOpen(DB, 'sequence', 's');
      const tab = tabs[tabs.length - 1];
      await G.until(() => tab.rows, 10000);
      G.check('opened, it reads as its state', tab.cols.includes('next_not_cached_value'), tab.cols);
      closeTab(tab.id);
    }
  } finally {
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
