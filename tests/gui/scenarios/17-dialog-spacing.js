// Every dialog is spaced the same way: one gap under the title, and the last row - usually the
// buttons - kept off the box's own edges. Measured against the box's content, not the part of it
// on screen, so a dialog that scrolls is judged by the same rule as one that does not. A dialog
// with a fixed height that hides its overflow is worse than cramped: it cuts the buttons off.
(async () => {
  const tight = [], gaps = new Set();
  for (const m of [...document.querySelectorAll('.modal')]) {
    const had = m.classList.contains('show');
    m.classList.add('show');
    await G.wait(60);
    const box = m.querySelector('.box');
    if (box) {
      const head = box.querySelector(':scope > div[onmousedown]');
      if (head) {
        const h = head.querySelector('h2,h3');
        if (h && parseFloat(getComputedStyle(h).marginBottom) > 0) tight.push(m.id + ': the title keeps a margin of its own');
        const next = head.nextElementSibling;
        if (next && next.getBoundingClientRect().height > 2) gaps.add(Math.round(next.getBoundingClientRect().top - head.getBoundingClientRect().bottom));
      }
      const kids = [...box.children].filter(k => k.getBoundingClientRect().height > 2);
      const last = kids[kids.length - 1];
      if (last) {
        const bottom = box.scrollHeight - (last.offsetTop + last.offsetHeight);
        const right = box.scrollWidth - (last.offsetLeft + last.offsetWidth);
        if (bottom < 10 || right < 10) tight.push(m.id + ': last row ' + bottom + 'px from the bottom, ' + right + 'px from the right');
      }
      if (getComputedStyle(box).overflowY === 'hidden' && box.scrollHeight > box.clientHeight + 1)
        tight.push(m.id + ': ' + (box.scrollHeight - box.clientHeight) + 'px of it is cut off - the box is too short for what is in it');
    }
    if (!had) m.classList.remove('show');
  }
  G.check('nothing is pressed against a dialog edge', tight.length === 0, tight);
  // One gap, give or take whatever the first element under the header brings of its own
  // margin: what this guards against is a title with its small print pressed against it.
  G.check('and every title has room under it', [...gaps].every(g => g >= 8 && g <= 18), [...gaps]);
  return G.report();
})()
