// The ER diagram: each table to the right of the ones it refers to, its columns with their types and
// PK / FK / UQ marks, right-angled lines between the rows a key joins with the ends saying how many,
// the key named when a line is pointed at, Fit, Find, and an SVG export with no handlers in it.
(async () => {
  const DB = 'nobs_gui_erd';
  try {
    await G.run(`DROP DATABASE IF EXISTS ${DB}; CREATE DATABASE ${DB};
CREATE TABLE ${DB}.dept (id INT PRIMARY KEY, name VARCHAR(30) NOT NULL UNIQUE);
CREATE TABLE ${DB}.emp (id INT PRIMARY KEY, dept_id INT NOT NULL, boss_id INT NULL, badge CHAR(8) UNIQUE,
  CONSTRAINT fk_emp_dept FOREIGN KEY (dept_id) REFERENCES ${DB}.dept(id), CONSTRAINT fk_emp_boss FOREIGN KEY (boss_id) REFERENCES ${DB}.emp(id));
CREATE TABLE ${DB}.task (id INT PRIMARY KEY, emp_id INT NULL, CONSTRAINT fk_task_emp FOREIGN KEY (emp_id) REFERENCES ${DB}.emp(id));
CREATE TABLE ${DB}.loner (id INT PRIMARY KEY);`);
    await openErd(DB);
    const names = () => window._erdTableNames.slice().sort();
    G.eq('only the tables with a relationship, at first', names(), ['dept', 'emp', 'task']);
    $('erdOnlyRelated').checked = false; erdRender();
    G.eq('all of them without the tick', names(), ['dept', 'emp', 'loner', 'task']);
    const P = window._erdCurPos;
    G.check('each table right of the ones it refers to, the lone one last', P.dept.x < P.emp.x && P.emp.x < P.task.x && P.loner.x > P.task.x, P);

    const g = n => $('erd_tbl_' + window._erdTableNames.indexOf(n));
    const texts = n => [...g(n).querySelectorAll('text')].map(t => t.firstChild ? t.firstChild.textContent : '');
    const d = texts('dept'), e = texts('emp');
    G.check('the types are shown', d.includes('varchar(30)') && e.some(x => /^char\(8\)$/.test(x)), d);
    G.check('with PK, FK and UQ marks', d.includes('PK') && d.includes('UQ') && e.filter(x => x === 'FK').length === 2 && e.includes('UQ'), e);
    const typeOf = (n, col) => { const ts = [...g(n).querySelectorAll('text')], i = ts.findIndex(t => t.firstChild && t.firstChild.textContent === col); return ts[i + 1]; };
    G.check('a column that takes NULL has its type in italics', typeOf('task', 'emp_id').getAttribute('font-style') === 'italic' && !typeOf('emp', 'dept_id').getAttribute('font-style'));

    const rels = [...document.querySelectorAll('#erdBox g.erdrel')];
    G.eq('one line per foreign key, the one to itself too', rels.length, 3);
    const rel = k => rels.find(r => r.querySelector('title').textContent.startsWith(k));
    const task = rel('fk_task_emp'), dept = rel('fk_emp_dept');
    G.check('pointing at a line names its key', !!task && task.querySelector('title').textContent.includes('task.emp_id') && task.querySelector('title').textContent.includes('may be NULL'), rels.map(r => r.querySelector('title').textContent));
    G.check('none-or-one where the key takes NULL, one where it does not', !!task.querySelector('circle') && !!dept && !dept.querySelector('circle'));
    const pts = r => r.querySelector('.erdline').getAttribute('d').slice(1).split('L').map(p => p.split(' ').map(Number));
    G.check('the lines are right-angled', rels.every(r => pts(r).every((p, i, a) => !i || p[0] === a[i - 1][0] || p[1] === a[i - 1][1])), rels.map(r => r.querySelector('.erdline').getAttribute('d')));
    erdHi(task, 1);
    const hi = [...document.querySelectorAll('#erdBox rect.erdrow.hi')].map(r => r.id);
    const ti = window._erdTableNames.indexOf('task'), ei = window._erdTableNames.indexOf('emp');
    G.eq('and marks the two rows it joins', hi.sort(), ['erd_row_' + ei + '_0', 'erd_row_' + ti + '_1'].sort());
    erdHi(task, 0);
    G.eq('until the pointer leaves', document.querySelectorAll('#erdBox .hi').length, 0);

    $('erdFind').value = 'tas'; erdFindTable();
    G.eq('Find marks the table it finds', g('task').querySelector('rect.erdframe').getAttribute('stroke'), '#f5c518');
    $('erdFind').value = ''; erdFindTable();

    // a table moved by hand stays where it was put
    window._erdPos.loner = { x: 900, y: 500 }; erdRender();
    G.check('a dragged table keeps its place', window._erdCurPos.loner.x === 900 && window._erdCurPos.loner.y === 500);

    erdSetZoom(1);
    const wheel = (ctrlKey, deltaY) => { const w = new WheelEvent('wheel', { bubbles: true, cancelable: true, ctrlKey, deltaY, clientX: 200, clientY: 200 }); $('erdBox').dispatchEvent(w); return w; };
    const wz = wheel(true, -100);
    G.check('Ctrl + wheel zooms in, and not the window', window._erdZoom === 1.1 && wz.defaultPrevented, window._erdZoom);
    wheel(true, 100); wheel(true, 100);
    G.eq('and out', window._erdZoom, 0.91);
    const wp = wheel(false, 100);
    G.check('the wheel alone leaves the zoom and scrolls', window._erdZoom === 0.91 && !wp.defaultPrevented);

    erdSetZoom(3); erdFit();
    const box = $('erdBox'), sv = box.querySelector('svg').getBoundingClientRect();
    G.check('Fit brings the whole diagram into view', sv.width <= box.clientWidth + 1 && sv.height <= box.clientHeight + 1, { svg: [sv.width, sv.height], box: [box.clientWidth, box.clientHeight], zoom: window._erdZoom });

    const bg0 = document.querySelector('#erdBox svg > rect').getAttribute('fill');
    toggleTheme(); await G.wait(50);
    const bg1 = document.querySelector('#erdBox svg > rect').getAttribute('fill');
    toggleTheme(); await G.wait(50);
    G.check('a new theme redraws it in that theme\'s colours', bg0 !== bg1 && document.querySelector('#erdBox svg > rect').getAttribute('fill') === bg0, [bg0, bg1]);

    const svg = erdSvgText();
    G.check('the SVG export has the diagram and none of its handlers', svg.startsWith('<?xml') && svg.includes('fk_task_emp') && !/\son[a-z]+=/i.test(svg) && !svg.includes('erdhit') && !svg.includes('var(--'), svg.slice(0, 200));

    erdFocusTable('task');
    G.eq('a table\'s own relations', names(), ['emp', 'task']);
    erdFocusTable(null); erdSetZoom(1);

    // a line across a column of tables goes around them, not under one
    await G.run(`CREATE TABLE ${DB}.note (id INT PRIMARY KEY, dept_id INT, task_id INT, FOREIGN KEY (dept_id) REFERENCES ${DB}.dept(id), FOREIGN KEY (task_id) REFERENCES ${DB}.task(id))`);
    await openErd(DB); $('erdOnlyRelated').checked = false; erdRender();
    const Q = window._erdCurPos, far = [...document.querySelectorAll('#erdBox g.erdrel')].find(r => r.querySelector('title').textContent.includes('note.dept_id'));
    const fp = pts(far), across = fp.slice(1).some((p, i) => { const q = fp[i]; return ['emp', 'task'].some(n => { const b = Q[n]; return Math.max(p[0], q[0]) > b.x && Math.min(p[0], q[0]) < b.x + b.w && Math.max(p[1], q[1]) > b.y && Math.min(p[1], q[1]) < b.y + b.h; }); });
    G.check('a line across a column of tables goes around them', Q.note.x > Q.task.x && !across, fp);
  } finally {
    $('erdOnlyRelated').checked = true; hide('mErd');
    await G.run(`DROP DATABASE IF EXISTS ${DB}`);
  }
  return G.report();
})()
