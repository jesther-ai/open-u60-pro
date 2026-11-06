'use strict';
(() => {
  const reduced = matchMedia('(prefers-reduced-motion:reduce)');
  const running = new Set(), dialogs = new WeakMap();
  let pageAnimation, pageVersion = 0;
  const allowed = () => !reduced.matches && !document.hidden;
  function animate(node, frames, duration) {
    if (!allowed() || typeof node.animate !== 'function') return null;
    try {
      const animation = node.animate(frames,{duration,easing:'cubic-bezier(.2,.7,.2,1)'});
      running.add(animation);
      animation.finished.catch(() => {}).then(() => running.delete(animation));
      return animation;
    } catch { return null; }
  }
  const finished = animation => animation ? animation.finished.catch(() => {}) : Promise.resolve();
  function changePage(update,{animate:withMotion=true} = {}) {
    const main = document.querySelector('#main'), version = ++pageVersion;
    const opacity = getComputedStyle(main).opacity;
    pageAnimation?.cancel();
    main.inert = false;
    main.removeAttribute('aria-busy');
    if (!withMotion || !allowed() || typeof main.animate !== 'function') { update(); return; }
    main.inert = true;
    main.setAttribute('aria-busy','true');
    pageAnimation = animate(main,[{opacity},{opacity:.2}],90);
    finished(pageAnimation).then(() => {
      if (version !== pageVersion) return;
      main.inert = false;
      main.removeAttribute('aria-busy');
      update();
      pageAnimation = animate(main,[{opacity:.2,transform:'translateY(6px)'},{opacity:1,transform:'translateY(0)'}],170);
    });
  }
  function showDialog(dialog) {
    const previous = dialogs.get(dialog);
    previous?.animation?.cancel();
    const state = {};
    dialogs.set(dialog,state);
    if (!dialog.open) dialog.showModal();
    if (!allowed()) { delete dialog.dataset.motion; return; }
    dialog.dataset.motion = 'opening';
    state.animation = animate(dialog,[{opacity:0,transform:'translateY(10px) scale(.985)'},{opacity:1,transform:'translateY(0) scale(1)'}],180);
    finished(state.animation).then(() => { if (dialogs.get(dialog) === state) delete dialog.dataset.motion; });
  }
  function closeDialog(dialog) {
    if (!dialog.open) return Promise.resolve();
    const previous = dialogs.get(dialog);
    if (previous?.closing) return previous.closing;
    const opacity = getComputedStyle(dialog).opacity;
    previous?.animation?.cancel();
    const state = {};
    dialogs.set(dialog,state);
    const close = () => {
      if (dialogs.get(dialog) !== state) return;
      delete dialog.dataset.motion;
      dialog.close();
    };
    if (!allowed() || typeof dialog.animate !== 'function') { close(); return Promise.resolve(); }
    dialog.dataset.motion = 'closing';
    state.animation = animate(dialog,[{opacity,transform:'translateY(0) scale(1)'},{opacity:0,transform:'translateY(6px) scale(.985)'}],120);
    state.closing = finished(state.animation).then(close);
    return state.closing;
  }
  function settle() {
    for (const animation of running) {
      try { animation.finish(); } catch { animation.cancel(); }
    }
  }
  reduced.addEventListener('change',() => { if (reduced.matches) settle(); });
  document.addEventListener('visibilitychange',() => { if (document.hidden) settle(); });
  globalThis.OpenU60Motion = {changePage,showDialog,closeDialog};
})();
