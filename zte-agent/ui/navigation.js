'use strict';
(() => {
  const icons = {
    dashboard: '<rect x="3" y="3" width="18" height="18" rx="3"/><path d="M9 3v18M9 10h12"/>',
    network: '<path d="M3 9a15 15 0 0 1 18 0M6 13a10 10 0 0 1 12 0m-9 4a5 5 0 0 1 6 0"/><circle cx="12" cy="21" r=".5"/>',
    sms: '<path d="M21 11a8 8 0 0 1-8 8H7l-4 3V5a3 3 0 0 1 3-3h7a8 8 0 0 1 8 9Z"/><path d="M7 8h10M7 12h7"/>',
    tools: '<path d="m14 6 4-4a6 6 0 0 1-7 8L4 21l-3-3L12 7a6 6 0 0 1 8-7l-4 4 1 3 3 1"/>',
    settings: '<path d="m10 3-1 3-3 1-3 3 2 3-1 3 3 3 3-1 3 2 3-3 1-3 3-1-1-4-3-1-1-3Z"/><circle cx="12" cy="12" r="3"/>'
  };
  const groups = [
    {id:'dashboard',label:'Dashboard',pages:[
      {id:'overview',label:'Overview',title:'Router overview',description:'A clear view of your connection and the devices on it.'},
      {id:'history',label:'Connection history',title:'Connection history',description:'Interruptions and recovery observed during this tab session.'}
    ]},
    {id:'network',label:'Network',pages:[
      {id:'signal',label:'Signal & Cells',title:'Signal & Cells',description:'Every reported LTE and NR carrier, with clear SA and NSA status.'},
      {id:'wifi',label:'Wi-Fi',title:'Wi-Fi networks',description:'Your local wireless networks and radio status.'},
      {id:'clients',label:'Connected Devices',title:'Connected devices',description:'Your devices, live traffic, observed peaks and reported speed limits.'},
      {id:'lan',label:'LAN & DHCP',title:'LAN & DHCP',description:'The router’s local address and the addresses it assigns to your devices.'},
      {id:'firewall',label:'Firewall & DMZ',title:'Firewall & DMZ',description:'Control inbound protection, address translation and automatic port access.'},
      {id:'ports',label:'Port forwarding',title:'Port forwarding',description:'Direct specific incoming connections to a device on your local network.'},
      {id:'qos',label:'Quality of service',title:'Quality of service',description:'Manage the router’s built-in traffic prioritization.'},
      {id:'mobile',label:'Mobile Network',title:'Mobile network',description:'The cellular data connection and modem radio state.'},
      {id:'esim',label:'eSIM management',title:'eSIM management',description:'Profiles, activation codes and connection status for the eSIM card in your router.'},
      {id:'operators',label:'Network operators',title:'Network operators',description:'Find available mobile networks and choose automatic or manual registration.'},
      {id:'apn',label:'APN profiles',title:'APN profiles',description:'Manage the access-point profiles used for mobile data.'},
      {id:'bands',label:'Bands & Cell Lock',title:'Bands & Cell Lock',description:'Choose preferred LTE and NR bands or a specific serving cell.'}
    ]},
    {id:'sms',label:'SMS',pages:[
      {id:'inbox',label:'Messages',title:'Messages',description:'Read, reply to and manage messages on your router and SIM.'},
      {id:'compose',label:'Compose',title:'New message',description:'Send a text message through the router’s SIM.'},
      {id:'forwarding',label:'Forwarding rules',title:'SMS forwarding',description:'Route incoming messages to the destinations you choose.'},
      {id:'storage',label:'Message storage',title:'Message storage',description:'Capacity and unread message counts reported by the modem.'}
    ]},
    {id:'tools',label:'Tools',pages:[
      {id:'device',label:'Device Info',title:'Device information',description:'System details reported by the on-device agent.'},
      {id:'battery',label:'Battery & Power',title:'Battery & power',description:'Battery condition, charging, runtime estimates and measured power.'},
      {id:'diagnostics',label:'System diagnostics',title:'System diagnostics',description:'Live resource use, temperatures, processes and interface details.'},
      {id:'radio',label:'Radio diagnostics',title:'Radio diagnostics',description:'Inspect neighboring cells and run a signal quality sweep.'},
      {id:'speedtest',label:'Internet speed test',title:'Internet speed test',description:'Measure the router’s internet connection, with live progress.'},
      {id:'data',label:'Data usage',title:'Data usage',description:'Track router-measured usage and set a monthly allowance, warning and billing cycle.'},
      {id:'terminal',label:'AT terminal',title:'AT terminal',description:'Send an explicit modem command and inspect its response.'},
      {id:'traffic',label:'Traffic monitor',title:'Traffic monitor',description:'Live transfer rates and interface totals, streamed from the router.'}
    ]},
    {id:'settings',label:'Settings',pages:[
      {id:'device',label:'Device & Power',title:'Device & power',description:'Charging, power settings and device maintenance.'},
      {id:'https',label:'HTTPS & Install',title:'HTTPS & install',description:'Secure local access and install this console on your devices.'},
      {id:'appearance',label:'Appearance',title:'Appearance',description:'A comfortable view, in any light.'},
      {id:'connection',label:'Console connection',title:'Console connection',description:'Connection status and access to this local console.'},
      {id:'about',label:'About',title:'About OpenU60',description:'A local console for your ZTE U60 Pro.'}
    ]}
  ];
  const aliases = {overview:'dashboard/overview',cellular:'network/signal',wireless:'network/wifi',devices:'network/clients',system:'tools/device'};
  const remembered = new Map(), positions = new Map();
  let current;
  const primary = document.querySelector('#primary-navigation');
  const mobileHost = document.querySelector('#mobile-navigation-host');
  const navigationSlot = document.createComment('Desktop navigation');
  primary.before(navigationSlot);
  const mobileLayout = matchMedia('(max-width:767px), (max-width:1023px) and (max-height:500px) and (hover:none) and (pointer:coarse)');
  let viewportFrame;
  for (const group of groups) {
    const details = document.createElement('details'); details.className = 'nav-group'; details.dataset.group = group.id;
    const summary = document.createElement('summary');
    // Icons are static application assets; page and modem values are never parsed as markup.
    summary.innerHTML = `<span class="nav-icon" aria-hidden="true"><svg viewBox="0 0 24 24">${icons[group.id]}</svg></span><span class="nav-text">${group.label}</span><span class="nav-chevron" aria-hidden="true">›</span>`;
    const children = document.createElement('div'); children.className = 'nav-children';
    for (const page of group.pages) {
      const link = document.createElement('a'); link.href = `#${group.id}/${page.id}`; link.textContent = page.label; link.dataset.route = `${group.id}/${page.id}`;
      children.append(link);
    }
    summary.addEventListener('click', event => {
      if (mobileLayout.matches) {
        event.preventDefault(); go(remembered.get(group.id) || `${group.id}/${group.pages[0].id}`);
      }
    });
    details.append(summary,children); primary.append(details);
  }
  function resolve(hash) {
    const key = aliases[hash] || hash;
    for (const group of groups) {
      const page = group.pages.find(page => `${group.id}/${page.id}` === key);
      if (page) return {group,page,key};
    }
    return {group:groups[0],page:groups[0].pages[0],key:'dashboard/overview'};
  }
  function apply(initial = false) {
    if (current) positions.set(current.key, scrollY);
    current = resolve(location.hash.slice(1));
    remembered.set(current.group.id,current.key);
    document.documentElement.dataset.route = current.key;
    document.title = `${current.page.title} · OpenU60`;
    const title = document.querySelector('#page-title');
    title.textContent = current.page.title;
    const dot = document.createElement('span'); dot.textContent = '.'; title.append(dot);
    document.querySelector('#page-description').textContent = current.page.description;
    document.querySelector('#page-section').textContent = current.group.label.toUpperCase();
    document.querySelector('#breadcrumb-section').textContent = current.group.label;
    document.querySelector('#breadcrumb-page').textContent = current.page.label;
    for (const details of primary.querySelectorAll('.nav-group')) {
      const active = details.dataset.group === current.group.id;
      details.classList.toggle('active',active);
      const summary = details.querySelector('summary');
      if (active) { details.open = true; summary.setAttribute('aria-current','true'); }
      else summary.removeAttribute('aria-current');
      for (const link of details.querySelectorAll('a')) {
        const selected = link.dataset.route === current.key;
        link.classList.toggle('active',selected);
        if (selected) link.setAttribute('aria-current','page'); else link.removeAttribute('aria-current');
      }
    }
    const subtabs = document.querySelector('#section-tabs'); subtabs.replaceChildren();
    for (const page of current.group.pages) {
      const link = document.createElement('a'); link.href = `#${current.group.id}/${page.id}`;
      link.textContent = page.label;
      if (page.id === current.page.id) link.setAttribute('aria-current','page');
      subtabs.append(link);
    }
    for (const panel of document.querySelectorAll('[data-views]')) {
      panel.dataset.viewHidden = String(!panel.dataset.views.split(' ').includes(current.key));
    }
    window.dispatchEvent(new CustomEvent('dashboard:navigate',{detail:{group:current.group.id,page:current.page.id,key:current.key}}));
    if (!initial) {
      scrollTo({top:positions.get(current.key) || 0,behavior:'instant'});
      title.focus({preventScroll:true});
    }
    revealCurrentPage();
  }
  function revealCurrentPage() {
    const tabs = document.querySelector('#section-tabs'), selected = tabs.querySelector('[aria-current]');
    if (!selected || !tabs.clientWidth) return;
    const left = selected.getBoundingClientRect().left - tabs.getBoundingClientRect().left + tabs.scrollLeft;
    if (left < tabs.scrollLeft) tabs.scrollLeft = Math.max(0,left - 12);
    else if (left + selected.offsetWidth > tabs.scrollLeft + tabs.clientWidth) tabs.scrollLeft = left + selected.offsetWidth - tabs.clientWidth + 12;
  }
  function updateViewport() {
    viewportFrame = undefined;
    const root = document.documentElement;
    if (!mobileLayout.matches) {
      root.style.removeProperty('--mobile-navigation-lift');
      root.style.removeProperty('--mobile-navigation-height');
      root.style.removeProperty('--mobile-viewport-height');
      root.style.removeProperty('--mobile-viewport-top');
      return;
    }
    const viewport = window.visualViewport;
    const normalScale = viewport && Math.abs(viewport.scale - 1) < .01 && viewport.height > 0;
    // The keyboard can shrink only the visible viewport while fixed elements stay below it.
    const lift = normalScale
      ? Math.max(0,Math.round(root.clientHeight - viewport.height - Math.max(0,viewport.offsetTop))) : 0;
    const sizes = {
      '--mobile-navigation-lift':lift,
      '--mobile-navigation-height':Math.ceil(primary.getBoundingClientRect().height),
      '--mobile-viewport-height':normalScale ? viewport.height : root.clientHeight,
      '--mobile-viewport-top':normalScale ? Math.max(0,viewport.offsetTop) : 0
    };
    for (const [name,value] of Object.entries(sizes)) {
      if (root.style.getPropertyValue(name) !== `${value}px`) root.style.setProperty(name,`${value}px`);
    }
  }
  function scheduleViewport() {
    if (viewportFrame === undefined) viewportFrame = requestAnimationFrame(updateViewport);
  }
  function placeNavigation() {
    const destination = mobileLayout.matches ? mobileHost : navigationSlot.parentElement;
    if (primary.parentElement !== destination) {
      const focused = primary.contains(document.activeElement) ? document.activeElement : null;
      if (mobileLayout.matches) mobileHost.append(primary); else navigationSlot.after(primary);
      focused?.focus({preventScroll:true});
    }
    scheduleViewport();
    revealCurrentPage();
  }
  function go(route) {
    if (location.hash.slice(1) === route) OpenU60Motion.changePage(() => apply(),{animate:false}); else location.hash = route;
  }
  document.querySelector('.skip').addEventListener('click',event => {
    event.preventDefault();
    const main = document.querySelector('#main');
    main.tabIndex = -1; main.focus({preventScroll:true}); scrollTo({top:0,behavior:'instant'});
  });
  window.addEventListener('hashchange',() => {
    OpenU60Motion.changePage(() => apply(),{animate:resolve(location.hash.slice(1)).key !== current.key});
  });
  mobileLayout.addEventListener('change',placeNavigation);
  window.addEventListener('resize',() => { placeNavigation(); scheduleViewport(); },{passive:true});
  window.addEventListener('pageshow',placeNavigation);
  window.visualViewport?.addEventListener('resize',scheduleViewport,{passive:true});
  window.visualViewport?.addEventListener('scroll',scheduleViewport,{passive:true});
  if (typeof ResizeObserver === 'function') new ResizeObserver(scheduleViewport).observe(primary);
  globalThis.OpenU60Navigation = {go,get current(){return {group:current.group.id,page:current.page.id,key:current.key};}};
  apply(true);
  placeNavigation();
})();
