// The bars settle at every width. 1.3.17 fitted the top bar from its labels on every change to it,
// fitting it changed the connection box, and at some widths - 1040px among them - that went round
// forever after connecting and the page answered nothing. Here the page is made each width in turn
// and the top bar is changed the way connecting changes it; the page must still get to run a timer.
// A bar that went round again would stop this scenario here, and the runner fails it on its timeout.
(async () => {
  const body = document.body, was = body.style.width;
  const answers = () => new Promise(r => setTimeout(() => r(true), 0));
  const stuck = [];
  try {
    for (let w = 760; w <= 1920; w += 40) {
      body.style.width = w + 'px';
      await new Promise(r => requestAnimationFrame(() => r()));
      body.classList.add('disconnected'); body.classList.remove('disconnected');
      const cs = $('connStatus'); if (cs) cs.textContent = cs.textContent + ' ';
      if (typeof refreshConns === 'function') await refreshConns();
      if (typeof connTitle === 'function') connTitle();
      const ok = await Promise.race([answers(), new Promise(r => setTimeout(() => r(false), 3000))]);
      if (!ok) stuck.push(w);
    }
    G.eq('the page answers after the top bar changes, at every width from 760 to 1920', stuck, []);
  } finally {
    body.style.width = was;
  }
  return G.report();
})()
