// Settings › Appearance = System only (owner decision 60). When the system
// switches between light and dark with a page open, iOS 26 keeps the old
// status bar colour (tested in the iOS 26.5 simulator, both directions):
// - the home-screen web app takes it from `theme-color` but does not
//   evaluate the tags' `media` again: one tag with the new colour instead;
// - a Safari tab samples the sticky nav bar and keeps that, also after
//   navigating or reloading in the tab: taking the nav bar out of the layout
//   for two frames makes it sample the bar again.
// Nothing runs until that switch: no timer, no polling.
const dark = matchMedia('(prefers-color-scheme: dark)');
const tags = [...document.querySelectorAll('meta[name="theme-color"][media]')];
dark.addEventListener('change', () => {
  const colour = tags.find((m) => matchMedia(m.media).matches)?.content;
  let one = document.querySelector('meta[name="theme-color"]:not([media])');
  if (colour && !one) {
    one = document.createElement('meta');
    one.name = 'theme-color';
    tags.forEach((m) => m.remove());
    document.head.append(one);
  }
  if (colour) one.content = colour;
  const bar = document.querySelector('.bar');
  if (!bar) return;
  bar.style.display = 'none';
  requestAnimationFrame(() => requestAnimationFrame(() => bar.style.removeProperty('display')));
});
