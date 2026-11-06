'use strict';
(() => {
  function ipv4(value,label = 'IPv4 address') {
    const text = String(value ?? '').trim(), parts = text.split('.');
    if (parts.length !== 4 || parts.some(part=>!/^(0|[1-9]\d{0,2})$/.test(part) || Number(part)>255)) throw new Error(`Enter a valid ${label}.`);
    return parts.reduce((sum,part)=>sum*256+Number(part),0);
  }
  const address = value => [24,16,8,0].map(shift=>(value>>>shift)&255).join('.');
  function subnet(ip,mask) {
    const host = ipv4(ip,'router address'), bits = ipv4(mask,'subnet mask'), inverse = 0xffffffff-bits;
    if (inverse < 3 || inverse >= 0x80000000 || (inverse & (inverse+1)) !== 0) throw new Error('Use a continuous subnet mask from /1 to /30.');
    const network = (host & bits)>>>0, broadcast = network+inverse;
    if (host<=network || host>=broadcast || (host>>>24)===0 || (host>>>24)===127 || (host>>>24)>=224) throw new Error('The router address must be a usable unicast host in this subnet.');
    return {host,network,broadcast,size:inverse-1};
  }
  function host(value,lan,label = 'device address') {
    const target = ipv4(value,label), net = subnet(lan.lan_ipaddr,lan.lan_netmask);
    if (target<=net.network || target>=net.broadcast || target===net.host || (target>>>24)===0 || (target>>>24)===127 || (target>>>24)>=224) throw new Error(`The ${label} must be inside the LAN subnet and different from the router, network and broadcast addresses.`);
    return target;
  }
  function switchValue(value,label) { if (!['0','1'].includes(String(value))) throw new Error(`Choose whether ${label} is enabled.`); return String(value); }
  function normalizeSwitches(data) {
    const result={...data};
    for(const key of ['dhcp_enable','firewall_switch','nat_switch','dmz_enabled','port_forward_switch','upnp_switch','qos_switch','enabled']){
      if(!Object.hasOwn(result,key))continue;
      const value=String(result[key]).toLowerCase();
      if(['1','true','on'].includes(value))result[key]='1';else if(['0','false','off'].includes(value))result[key]='0';
    }
    return result;
  }
  function lease(value) {
    const match = /^([1-9]\d*)([smhdw]?)$/.exec(String(value));
    const seconds = match ? Number(match[1])*({s:1,m:60,h:3600,d:86400,w:604800}[match[2]] || 1) : 0;
    if (!Number.isSafeInteger(seconds) || seconds<1 || seconds>2147483647) throw new Error('Use a positive lease duration, such as 43200, 12h or 1d.');
    return seconds;
  }
  function normalizeLAN(data) {
    if (!data || typeof data !== 'object') throw new Error('LAN settings are unavailable.');
    const result = normalizeSwitches(data);
    for (const key of ['lan_ipaddr','lan_netmask','dhcp_enable','dhcp_start','dhcp_end','dhcp_lease_time']) {
      if (data[key] === undefined || data[key] === null || String(data[key]) === '') throw new Error('The router returned incomplete LAN settings.');
      result[key] = String(result[key]);
    }
    const net = subnet(result.lan_ipaddr,result.lan_netmask);
    for (const key of ['dhcp_start','dhcp_end']) if (/^\d+$/.test(result[key])) {
      const offset = Number(result[key]);
      if (!Number.isSafeInteger(offset) || offset<1 || offset>=net.broadcast-net.network) throw new Error('The reported DHCP range is outside the LAN subnet.');
      result[key] = address(net.network+offset);
    }
    switchValue(result.dhcp_enable,'DHCP');
    return result;
  }
  function lanBody(all) {
    const body = Object.fromEntries(['lan_ipaddr','lan_netmask','dhcp_enable','dhcp_start','dhcp_end','dhcp_lease_time'].map(key=>[key,String(all[key] ?? '').trim()]));
    const net = subnet(body.lan_ipaddr,body.lan_netmask); switchValue(body.dhcp_enable,'DHCP');
    if(body.dhcp_start)ipv4(body.dhcp_start,'DHCP start address');
    if(body.dhcp_end)ipv4(body.dhcp_end,'DHCP end address');
    if(body.dhcp_lease_time)lease(body.dhcp_lease_time);
    if (body.dhcp_enable === '1') {
      const start = host(body.dhcp_start,body,'DHCP start address'), end = host(body.dhcp_end,body,'DHCP end address');
      if (end<start) throw new Error('The DHCP end address must be at or after the start address.');
      if (net.host>=start && net.host<=end) throw new Error('The DHCP pool must not include the router’s own address.');
      lease(body.dhcp_lease_time);
    }
    return body;
  }
  const ordered = value => Array.isArray(value) ? value.map(ordered) : value && typeof value==='object' ? Object.fromEntries(Object.keys(value).sort().map(key=>[key,ordered(value[key])])) : value;
  const canonical = value => JSON.stringify(ordered(value));
  function rules(data) {
    if (!data || !Array.isArray(data.rule_list) || data.rule_list.some(rule=>!rule || typeof rule!=='object' || Array.isArray(rule))) throw new Error('The router returned no usable port-forward rule list.');
    const entries = data.rule_list.map((raw,index)=>({...normalizeSwitches(raw),id:String(raw.id ?? index),raw}));
    const ids = entries.map(rule=>rule.id);
    return entries.map(rule=>({...rule,removable:ids.filter(id=>id===rule.id).length===1}));
  }
  function port(value,label) { if (!/^\d{1,5}$/.test(String(value)) || Number(value)<1 || Number(value)>65535) throw new Error(`Use one ${label} between 1 and 65535.`); return String(Number(value)); }
  const protocols = value => value==='tcp+udp' ? ['tcp','udp'] : [String(value).toLowerCase()];
  function portBody(all,lan,existing) {
    const name = String(all.name ?? '').trim();
    if (!name || new TextEncoder().encode(name).length>64 || /['";$`\\|<>&\x00-\x1f\x7f]/.test(name)) throw new Error('Use a rule name of 1–64 bytes without quotes or special command characters.');
    if (!['tcp','udp','tcp+udp'].includes(all.protocol)) throw new Error('Choose TCP, UDP or TCP + UDP.');
    const body = {action:'add',name,protocol:all.protocol,wan_port:port(all.wan_port,'external port'),lan_ip:String(all.lan_ip).trim(),lan_port:port(all.lan_port,'internal port'),enabled:switchValue(all.enabled,'this rule')};
    host(body.lan_ip,lan);
    for (const rule of existing) {
      const range = /^(\d+)(?:[:-](\d+))?$/.exec(String(rule.wan_port));
      if (range && Number(body.wan_port)>=Number(range[1]) && Number(body.wan_port)<=Number(range[2] || range[1]) && protocols(String(rule.protocol).toLowerCase()).some(value=>protocols(body.protocol).includes(value))) throw new Error('That external port and protocol already belong to a rule. Remove the old rule or choose another port.');
    }
    return body;
  }
  function reconnectURL(origin,ip) { ipv4(ip); const url = new URL(origin); url.hostname = ip; url.pathname='/'; url.search=''; url.hash='network/lan'; return url.href; }
  const api = {ipv4,address,subnet,host,lease,normalizeLAN,lanBody,switchValue,normalizeSwitches,canonical,rules,portBody,reconnectURL};
  if (typeof module==='object' && module.exports) module.exports=api; else globalThis.OpenU60Routing=api;
})();
