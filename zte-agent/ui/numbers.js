'use strict';
(() => {
  const duration = 380;
  const motion = matchMedia('(prefers-reduced-motion: reduce)');
  let states = new WeakMap();
  const active = new Set();
  const span = className => { const node = document.createElement('span'); node.className = className; return node; };

  function presentation(text) {
    const match = /^([+\-−]?)(\d[\d,]*)(\.\d+)?([^\d]*)$/.exec(text);
    if (!match) return null;
    const numeric = Number(`${match[1].replace('−','-')}${match[2].replaceAll(',','')}${match[3] || ''}`);
    if (!Number.isFinite(numeric)) return null;
    const tokens = [];
    let place = match[2].replaceAll(',','').length;
    for (const character of match[2]) {
      if (character === ',') tokens.push({key:`comma:${place}`,text:character});
      else tokens.push({key:`integer:${--place}`,text:character,digit:true});
    }
    if (match[3]) {
      tokens.push({key:'point',text:'.'});
      [...match[3].slice(1)].forEach((text,index)=>tokens.push({key:`fraction:${index}`,text,digit:true}));
    }
    return {numeric,sign:match[1],suffix:match[4],tokens};
  }

  function allowed(node) {
    if (motion.matches || document.hidden || typeof node.animate !== 'function') return false;
    const rect = node.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0 && rect.bottom > 0 && rect.top < innerHeight && rect.right > 0 && rect.left < innerWidth;
  }
  function mark(state) {
    if (state.running.size) { active.add(state); state.node.dataset.numberAnimating = 'true'; }
    else { active.delete(state); delete state.node.dataset.numberAnimating; }
  }
  function cancel(state,slot) {
    slot.generation++;
    for (const animation of slot.animations) { animation.onfinish = animation.oncancel = null; animation.cancel(); }
    slot.animations = []; slot.ghost.hidden = true; delete slot.ghost.dataset.text;
    state.running.delete(slot); mark(state);
  }
  function settle(state) { for (const slot of [...state.running]) cancel(state,slot); }
  function finish() { for (const state of [...active]) settle(state); }
  function clear() { finish(); states = new WeakMap(); }

  function slot(text) {
    const node = span('number-slot'), ghost = span('number-ghost'), current = span('number-current');
    ghost.setAttribute('aria-hidden','true'); ghost.hidden = true;
    current.textContent = text; node.append(ghost,current);
    return {node,ghost,current,text,animations:[],generation:0};
  }
  function animate(state,item,previous,direction) {
    cancel(state,item);
    const generation = item.generation;
    item.ghost.dataset.text = previous; item.ghost.hidden = !previous;
    const distance = direction === 'up' ? '95%' : '-95%';
    const outgoing = direction === 'up' ? '-95%' : '95%';
    const incomingFrames = direction === 'fade' ? [{opacity:0},{opacity:1}] : [{opacity:0,transform:`translateY(${distance})`},{opacity:1,transform:'translateY(0)'}];
    const outgoingFrames = direction === 'fade' ? [{opacity:1},{opacity:0}] : [{opacity:1,transform:'translateY(0)'},{opacity:0,transform:`translateY(${outgoing})`}];
    const options = {duration,easing:'cubic-bezier(.22,.7,.26,1)'};
    try {
      item.animations = [item.current.animate(incomingFrames,options),...(previous ? [item.ghost.animate(outgoingFrames,options)] : [])];
    } catch { item.ghost.hidden = true; item.animations = []; return; }
    state.running.add(item); mark(state);
    let remaining = item.animations.length;
    const done = () => { if (item.generation === generation && --remaining === 0) { cancel(state,item); if (state.whole && states.get(state.node) === state) mount(state.node,state.text,state.parsed); } };
    for (const animation of item.animations) animation.onfinish = animation.oncancel = done;
  }

  function mount(node,text,parsed) {
    const state = {node,text,parsed,slots:new Map(),running:new Set(),display:span('number-display')};
    node.classList.add('animated-number'); node.setAttribute('aria-label',text);
    node.replaceChildren(state.display); states.set(node,state);
    if (!parsed) { state.display.textContent = text; return state; }
    const sign = span('number-sign'); sign.textContent = parsed.sign; state.display.append(sign);
    for (const token of parsed.tokens) {
      if (token.digit) { const digit = slot(token.text); state.slots.set(token.key,digit); state.display.append(digit.node); }
      else { const punctuation = span('number-punctuation'); punctuation.textContent = token.text; state.display.append(punctuation); }
    }
    const suffix = span('number-suffix'); suffix.textContent = parsed.suffix; state.display.append(suffix);
    return state;
  }

  function set(node,value) {
    if (!node) return;
    const text = String(value ?? '—'), parsed = presentation(text), previous = states.get(node);
    if (previous?.text === text) return;
    // First readings and unavailable states never count up from invented zeroes.
    if (!previous || !previous.parsed || !parsed || !allowed(node)) {
      if (previous) settle(previous);
      mount(node,text,parsed); return;
    }
    previous.text = text; node.setAttribute('aria-label',text);
    // Units, signs and disappearing columns transition together, without padding fake zeroes.
    if (previous.parsed.suffix !== parsed.suffix || previous.parsed.sign !== parsed.sign || previous.parsed.tokens.length > parsed.tokens.length) {
      const oldText = previous.display.textContent; settle(previous);
      const state = mount(node,text,parsed), whole = slot(text);
      whole.node.classList.add('number-presentation'); state.display.replaceChildren(whole.node); state.slots.clear();
      state.whole = true; animate(state,whole,oldText,'fade'); return;
    }
    if (previous.whole) { const oldText = previous.display.textContent; settle(previous); mount(node,oldText,previous.parsed); set(node,text); return; }
    const direction = parsed.numeric >= previous.parsed.numeric ? 'up' : 'down';
    const keep = new Set(), children = [];
    const sign = previous.display.querySelector('.number-sign'); children.push(sign);
    for (const token of parsed.tokens) {
      if (token.digit) {
        let digit = previous.slots.get(token.key);
        if (!digit) { digit = slot(token.text); previous.slots.set(token.key,digit); animate(previous,digit,'',direction); }
        else if (digit.text !== token.text) { const old = digit.text; digit.text = token.text; digit.current.textContent = token.text; animate(previous,digit,old,direction); }
        keep.add(token.key); children.push(digit.node);
      } else {
        const punctuation = span('number-punctuation'); punctuation.textContent = token.text; children.push(punctuation);
      }
    }
    for (const [key,digit] of previous.slots) if (!keep.has(key)) { cancel(previous,digit); previous.slots.delete(key); }
    const suffix = previous.display.querySelector('.number-suffix'); children.push(suffix);
    // Retain digit elements by place value, including when the integer gains a column.
    const wanted = new Set(children);
    for (const child of [...previous.display.children]) if (!wanted.has(child)) child.remove();
    children.forEach((child,index)=>{ if (previous.display.children[index] !== child) previous.display.insertBefore(child,previous.display.children[index] || null); });
    previous.parsed = parsed;
  }
  motion.addEventListener('change',()=>{ if (motion.matches) finish(); });
  document.addEventListener('visibilitychange',()=>{ if (document.hidden) finish(); });
  window.addEventListener('dashboard:navigate',finish);
  window.addEventListener('dashboard:locked',clear);
  globalThis.OpenU60Numbers = {set,finish,clear};
})();
