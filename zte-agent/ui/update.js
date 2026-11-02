'use strict';
(() => {
  const UI = globalThis.OpenU60UI;
  if (!UI) return;
  const $ = id => document.getElementById(id);
  const form = $('update-settings-form'), fields = $('update-settings-fields');
  const channel = $('update-channel'), autoCheck = $('update-auto-check'), autoDownload = $('update-auto-download');
  const save = $('update-save'), check = $('update-check'), download = $('update-download'), install = $('update-install'), rollback = $('update-rollback');
  const feedback = $('update-feedback');
  let status = null, busy = false, baseline = '';
  const bytes = value => {
    const n = Number(value);
    if (!Number.isFinite(n) || n < 0) return '—';
    const unit = n >= 1048576 ? 'MB' : n >= 1024 ? 'KB' : 'B';
    return `${(n / ({MB:1048576,KB:1024,B:1}[unit])).toFixed(unit === 'B' ? 0 : 1)} ${unit}`;
  };
  const title = value => String(value || 'idle').replaceAll('_',' ').replace(/^./,letter=>letter.toUpperCase());
  const currentSettings = () => JSON.stringify({channel:channel.value,automatic_checks:autoCheck.checked,automatic_downloads:autoDownload.checked});
  function setBusy(value) {
    busy = value; form.inert = value; check.disabled = value;
    render();
  }
  function render(next = status) {
    if (next) status = next;
    if (!status) return;
    const current = status.current || {}, progress = status.progress || {};
    $('update-current-version').textContent = current.version ? `v${current.version}` : 'Unknown';
    $('update-current-commit').textContent = [current.commit,current.target].filter(Boolean).join(' · ') || 'Build details unavailable';
    $('update-channel-state').textContent = status.channel === 'beta' ? 'Beta' : 'Stable';
    $('update-check-time').textContent = status.last_check ? `Last checked ${new Date(status.last_check * 1000).toLocaleString()}` : 'Not checked yet';
    $('update-phase').textContent = status.trial_in_progress ? 'Health trial' : title(status.phase);
    $('update-guard').textContent = status.bootstrap_ready ? 'A/B recovery ready' : 'Bootstrap required';
    $('update-slot').textContent = status.active_slot ? `Active slot ${status.active_slot.toUpperCase()} · agent-only restart` : 'Install once over SSH to enable OTA';
    const release = status.available, staged = status.staged;
    $('update-state-detail').textContent = status.last_error || (!status.signing_configured ? 'This development build has no release verification key.' : staged ? `v${staged.version} is verified and staged.` : release ? `v${release.version} is ready to download.` : 'No newer signed release reported.');
    $('update-release').hidden = !release;
    if (release) {
      $('update-available-version').textContent = `v${release.version}${staged?.version === release.version ? ' · STAGED' : ''}`;
      $('update-release-copy').textContent = release.channel === 'beta' ? 'Signed Beta release. Review the notes before installing.' : 'Signed Stable release available for this device.';
      $('update-notes').href = release.notes_url;
    }
    const total = Number(progress.total || 0), received = Number(progress.received || 0);
    const percent = total > 0 ? Math.min(100,Math.round(received / total * 100)) : 0;
    $('update-progress').hidden = status.phase !== 'downloading' && !(total && received);
    OpenU60Numbers.set($('update-percent'),percent);
    $('update-progress-bar').style.width = `${percent}%`;
    $('update-bytes').textContent = `${bytes(received)} of ${bytes(total)} received and verified`;
    if (!fields.matches(':focus-within') && !busy) {
      channel.value = status.settings?.channel || status.channel || 'stable';
      autoCheck.checked = status.settings?.automatic_checks !== false;
      autoDownload.checked = status.settings?.automatic_downloads === true;
      baseline = currentSettings();
    }
    fields.disabled = false;
    save.disabled = busy || currentSettings() === baseline;
    check.disabled = busy || !status.signing_configured;
    download.disabled = busy || !release || staged?.version === release.version;
    install.disabled = busy || !staged || !status.bootstrap_ready || status.trial_in_progress;
    rollback.disabled = busy || !status.bootstrap_ready || !status.rollback_available || status.trial_in_progress;
  }
  form.addEventListener('input',()=>{ save.disabled = busy || currentSettings() === baseline; });
  form.addEventListener('submit',async event => {
    event.preventDefault();
    if (channel.value === 'beta' && status?.settings?.channel !== 'beta' && !await UI.confirm('Use the Beta channel?','Beta releases arrive earlier and may be less tested. Every release is still signed and protected by automatic rollback.',{label:'Use Beta'})) return;
    await action('Saving update preferences…','Preferences saved.',()=>UI.request('/api/update/settings',UI.json('PUT',JSON.parse(currentSettings()))));
  });
  check.addEventListener('click',()=>action('Checking signed release channels…','Signed release check complete.',()=>UI.request('/api/update/check',{method:'POST',timeoutMs:360000})));
  download.addEventListener('click',()=>action('Downloading and verifying the inactive slot…','Release downloaded, verified and staged.',()=>UI.request('/api/update/download',{method:'POST',timeoutMs:360000})));
  install.addEventListener('click',async()=>{
    if (!await UI.confirm('Install the staged agent?',`Install v${status?.staged?.version || ''} and restart only the agent? The console reconnects after a 60-second health trial. Wi-Fi and cellular stay online.`,{label:'Install agent'})) return;
    await action('Handing off to the update supervisor…','Agent restart accepted. This console will reconnect automatically.',()=>UI.request('/api/update/install',UI.json('POST',{confirm:true})));
  });
  rollback.addEventListener('click',async()=>{
    if (!await UI.confirm('Return to the previous agent?','Restart the agent from its other verified slot. Wi-Fi and cellular stay online.',{label:'Return to previous'})) return;
    await action('Starting protected rollback…','Rollback accepted. This console will reconnect automatically.',()=>UI.request('/api/update/rollback',UI.json('POST',{confirm:true})));
  });
  async function action(working,done,request) {
    if (busy) return;
    await UI.mutate(feedback,async current => {
      setBusy(true); UI.say(feedback,working);
      try { const result = await request(); if (!current()) return; render(result); baseline = currentSettings(); UI.say(feedback,done); }
      finally { setBusy(false); }
    });
  }
  window.addEventListener('dashboard:readings',event=>{
    const reading = event.detail.readings?.ota_status;
    if (reading?.ok) render(reading.data);
    else if (reading && !reading.ok) UI.say(feedback,'Live update status is unavailable.',true);
  });
  window.addEventListener('dashboard:locked',()=>{
    status = null; baseline = ''; fields.disabled = true; save.disabled = check.disabled = download.disabled = install.disabled = rollback.disabled = true;
    UI.say(feedback,'Unlock the console to read update status.');
  });
})();
