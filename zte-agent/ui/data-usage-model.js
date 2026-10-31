'use strict';
(() => {
  const number = value => value !== null && value !== undefined && typeof value !== 'boolean' && String(value).trim() !== '' && Number.isFinite(Number(value)) ? Number(value) : null;
  const nonnegative = value => number(value) !== null && number(value) >= 0 ? number(value) : null;
  const flag = value => [true,1,'1','true','enabled'].includes(value) ? true : [false,0,'0','false','disabled'].includes(value) ? false : null;
  function period(value = {}) {
    const tx = nonnegative(value.tx_bytes), rx = nonnegative(value.rx_bytes);
    return {txBytes:tx,rxBytes:rx,totalBytes:tx === null && rx === null ? null : (tx || 0)+(rx || 0),timeSeconds:nonnegative(value.time_secs),txPackets:nonnegative(value.tx_packets),rxPackets:nonnegative(value.rx_packets)};
  }
  function parse(data = {}) {
    const limitRaw = data.limit || {}, cycleRaw = data.cycle || {};
    const limit = {available:flag(limitRaw.available) === true,enabled:flag(limitRaw.enabled),bytes:nonnegative(limitRaw.bytes),warningPercent:nonnegative(limitRaw.warning_percent),exceeded:flag(limitRaw.exceeded) === true,autoDisconnect:flag(limitRaw.auto_disconnect)};
    const cycle = {available:flag(cycleRaw.available) === true,enabled:flag(cycleRaw.enabled),resetDay:nonnegative(cycleRaw.reset_day)};
    const result = {day:period(data.day),month:period(data.month),total:period(data.total),limit,cycle,settingsErrors:Array.isArray(data.settings_errors) ? data.settings_errors.map(String) : []};
    const used = result.month.totalBytes, cap = limit.bytes;
    result.percent = used !== null && cap > 0 ? used/cap*100 : null;
    if (!limit.available || used === null || cap === null) result.state = 'unavailable';
    else if (!limit.enabled) result.state = 'disabled';
    else if (limit.exceeded || result.percent >= 100) result.state = 'exceeded';
    else if (limit.warningPercent !== null && result.percent >= limit.warningPercent) result.state = 'warning';
    else result.state = 'normal';
    result.remainingBytes = used !== null && cap !== null ? Math.max(0,cap-used) : null;
    return result;
  }
  function bytes(value) {
    const n=nonnegative(value);if(n===null)return 'Not reported';
    const units=[['TB',1e12],['GB',1e9],['MB',1e6],['KB',1e3],['B',1]],unit=units.find(([,size])=>n>=size)||units.at(-1),digits=unit[0]==='B'?0:n/unit[1]>=1000?1:2;
    return `${(n/unit[1]).toFixed(digits)} ${unit[0]}`;
  }
  function count(value) { const n=nonnegative(value); return n===null ? 'Not reported' : new Intl.NumberFormat().format(n); }
  function duration(value) {
    const seconds=nonnegative(value);if(seconds===null)return 'Not reported';
    const minutes=Math.floor(seconds/60),days=Math.floor(minutes/1440),hours=Math.floor(minutes%1440/60),rest=minutes%60;
    return [days?`${days}d`:'',hours?`${hours}h`:'',rest||(!days&&!hours)?`${rest}m`:''].filter(Boolean).join(' ');
  }
  function gigabytes(value) {
    const n=nonnegative(value);if(n===null)return '';
    return (n/1e9).toFixed(3).replace(/(\.\d*?[1-9])0+$/,'$1').replace(/\.0+$/,'');
  }
  function bytesFromGigabytes(value) {
    const text=String(value ?? '').trim();
    if(!/^\d+(?:\.\d{1,3})?$/.test(text))return null;
    const result=Math.round(Number(text)*1e9);
    return Number.isSafeInteger(result)&&result>0&&result<=100*1024**4?result:null;
  }
  function ordinal(value) {
    const n=number(value);if(!Number.isInteger(n)||n<1||n>31)return 'Not reported';
    const mod100=n%100,suffix=mod100>=11&&mod100<=13?'th':({1:'st',2:'nd',3:'rd'}[n%10]||'th');return `${n}${suffix}`;
  }
  const api={parse,bytes,count,duration,gigabytes,bytesFromGigabytes,ordinal};
  if(typeof module==='object'&&module.exports)module.exports=api;else globalThis.OpenU60DataUsage=api;
})();
