'use strict';
(() => {
  const presets={
    cloudflare:{name:'Cloudflare',primary:'1.1.1.1',secondary:'1.0.0.1',doh_url:'https://cloudflare-dns.com/dns-query',dot_endpoint:'1.1.1.1:853',dot_server_name:'one.one.one.one'},
    quad9:{name:'Quad9',primary:'9.9.9.9',secondary:'149.112.112.112',doh_url:'https://dns.quad9.net/dns-query',dot_endpoint:'9.9.9.9:853',dot_server_name:'dns.quad9.net'},
    nextdns:{name:'NextDNS',personalized:true,modes:['doh','dot']}
  };
  const modes={automatic:'Carrier automatic',manual:'Manual DNS',doh:'DNS over HTTPS',dot:'DNS over TLS'};
  function ipv4(value){const parts=String(value||'').trim().split('.');return parts.length===4&&parts.every(part=>/^\d{1,3}$/.test(part)&&Number(part)>=0&&Number(part)<=255&&String(Number(part))===part);}
  function validNextdnsId(value){const id=String(value||'').trim();return /^[a-z0-9-]{1,63}$/i.test(id)&&!id.startsWith('-')&&!id.endsWith('-');}
  function nextdnsPreset(value){const id=String(value||'').trim().toLowerCase();if(!validNextdnsId(id))return null;const host=`${id}.dns.nextdns.io`;return {name:'NextDNS',primary:'1.1.1.1',secondary:'1.0.0.1',doh_url:`https://dns.nextdns.io/${id}`,dot_endpoint:`${host}:853`,dot_server_name:host};}
  function nextdnsId(data){const doh=String(data?.doh_url||'').match(/^https:\/\/dns\.nextdns\.io\/([^/?#]+)/i)?.[1],dot=String(data?.dot_server_name||data?.dot_endpoint||'').match(/^([^.]+)\.dns\.nextdns\.io(?::\d+)?$/i)?.[1],id=doh||dot||'';return validNextdnsId(id)?id.toLowerCase():'';}
  function payload(values){const mode=String(values.mode||'automatic');return {mode,primary:mode==='automatic'?'':String(values.primary||'').trim(),secondary:mode==='automatic'?'':String(values.secondary||'').trim(),doh_url:mode==='doh'?String(values.doh_url||'').trim():'',dot_endpoint:mode==='dot'?String(values.dot_endpoint||'').trim():'',dot_server_name:mode==='dot'?String(values.dot_server_name||'').trim():''};}
  function endpoint(data){if(data?.mode==='doh')return data.doh_url||'Not reported';if(data?.mode==='dot')return `${data.dot_endpoint||'Not reported'} · ${data.dot_server_name||'TLS name unavailable'}`;if(data?.mode==='manual')return [data.primary,data.secondary].filter(Boolean).join(' · ')||'Not reported';return (data?.carrier_servers||[]).join(' · ')||'Assigned by your carrier';}
  function ttl(value){const seconds=Math.max(0,Number(value)||0);if(seconds>=3600)return `${Math.floor(seconds/3600)}h ${Math.floor(seconds%3600/60)}m`;if(seconds>=60)return `${Math.floor(seconds/60)}m ${Math.floor(seconds%60)}s`;return `${Math.floor(seconds)}s`;}
  function rows(value){return (Array.isArray(value)?value:[]).map(entry=>({domain:String(entry.domain||'Unknown'),type:String(entry.type||'OTHER'),ttl:ttl(entry.ttl)})).sort((a,b)=>a.domain.localeCompare(b.domain)||a.type.localeCompare(b.type));}
  function cacheKey(entry){const domain=String(entry?.domain||'Unknown').trim().toLowerCase(),type=entry?.type_id===undefined?String(entry?.type||'OTHER').toUpperCase():String(entry.type_id);return `${domain}\u001f${type}`;}
  function cacheEntries(value){return (Array.isArray(value)?value:[]).map(entry=>{const domain=String(entry?.domain||'Unknown').trim()||'Unknown',type=String(entry?.type||'OTHER').toUpperCase(),ttlSeconds=Math.max(0,Math.floor(Number(entry?.ttl)||0));return {key:cacheKey(entry),domain,type,typeId:entry?.type_id,ttlSeconds};}).sort((a,b)=>a.domain.localeCompare(b.domain)||a.type.localeCompare(b.type));}
  const api={presets,modes,ipv4,validNextdnsId,nextdnsPreset,nextdnsId,payload,endpoint,ttl,rows,cacheKey,cacheEntries};if(typeof module==='object'&&module.exports)module.exports=api;else globalThis.OpenU60DNS=api;
})();
