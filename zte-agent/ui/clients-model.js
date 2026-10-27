'use strict';
(() => {
  const mac=value=>/^(?:[0-9a-f]{2}:){5}[0-9a-f]{2}$/i.test(String(value)) ? String(value).toLowerCase() : null;
  const number=value=>value!==null && value!==undefined && String(value).trim()!=='' && Number.isFinite(Number(value)) && Number(value)>=0 ? Number(value) : null;
  const addresses=value=>[...new Set((Array.isArray(value) ? value : String(value || '').split(/[,\s]+/)).filter(value=>typeof value==='string' && value.trim()).map(value=>value.trim()))];
  function merge(base,details) {
    const found=new Map(), own=new Set((details?.router_macs || []).map(mac));
    function entry(address){const key=mac(address);if(!key)return null;if(!found.has(key))found.set(key,{mac:key,name:'Unnamed device',ipv4:[],ipv6:[],connected:details?.inventory_complete ? false : null,source:'Host hint'});return found.get(key);}
    for(const [address,host]of Object.entries(base?.hosts || {})){
      if(!host || typeof host!=='object')continue;const item=entry(address);if(!item)continue;
      item.name=host.name || item.name;item.ipv4=addresses(host.ipaddrs || host.ipv4);item.ipv6=addresses(host.ip6addrs || host.ipv6);
    }
    for(const lease of Array.isArray(base?.dhcp_leases) ? base.dhcp_leases : []){
      const existed=found.has(mac(lease.macaddr)),item=entry(lease.macaddr);if(!item)continue;
      if(item.name==='Unnamed device')item.name=lease.hostname || item.name;
      item.ipv4=addresses([...item.ipv4,...addresses(lease.ipaddr)]);item.source=existed ? 'Host hint / DHCP' : 'DHCP lease';
    }
    for(const device of Array.isArray(details?.devices) ? details.devices : []){
      const item=entry(device.mac);if(!item)continue;
      const reported=addresses(device.ipv4),ipv4=reported.length ? reported : item.ipv4,ipv6=addresses([...item.ipv6,...addresses(device.ipv6)]);
      Object.assign(item,device,{mac:item.mac,name:device.name || item.name,ipv4,ipv6,source:'Router client list'});
    }
    return [...found.values()].filter(item=>!own.has(item.mac) || item.connected===true).sort((a,b)=>Number(b.connected===true)-Number(a.connected===true) || String(a.name).localeCompare(String(b.name)) || a.mac.localeCompare(b.mac)).slice(0,256);
  }
  function rate(value,missing='Not reported') { const n=number(value);if(n===null)return missing;const [scale,unit]=n>=1e12 ? [1e12,'Tbps'] : n>=1e9 ? [1e9,'Gbps'] : [1e6,'Mbps'];return `${(n/scale).toFixed(2)} ${unit}`; }
  function bytes(value) { const n=number(value);if(n===null)return 'Not reported';const scale=n>=1e12?['TB',1e12]:n>=1e9?['GB',1e9]:n>=1e6?['MB',1e6]:n>=1e3?['KB',1e3]:['B',1];return `${(n/scale[1]).toFixed(scale[1]===1?0:2)} ${scale[0]}`; }
  function connection(item) { const kind=item.connection_type;if(kind==='usb')return 'USB';if(kind==='wired')return 'Ethernet';if(kind==='wifi')return ({'2.4G':'2.4 GHz Wi-Fi','2G':'2.4 GHz Wi-Fi','5G':'5 GHz Wi-Fi','6G':'6 GHz Wi-Fi'}[item.band] || 'Wi-Fi');return 'Connection not reported'; }
  function matches(item,query) {return [item.name,item.mac,...item.ipv4,...item.ipv6,connection(item),item.interface,item.source].filter(Boolean).join(' ').toLowerCase().includes(query.trim().toLowerCase());}
  const api={mac,number,addresses,merge,rate,bytes,connection,matches};
  if(typeof module==='object' && module.exports)module.exports=api;else globalThis.OpenU60Clients=api;
})();
