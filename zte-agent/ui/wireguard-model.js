'use strict';
((globalThis) => {
  const list = value => String(value || '').split(',').map(item => item.trim()).filter(Boolean);
  function inspect(text) {
    const source = String(text || ''), result = {addresses:[],dns:[],peers:[],mtu:1420,hasPrivateKey:false};
    let section = '';
    for (const raw of source.split(/\r?\n/)) {
      const line = raw.trim();
      if (!line || line.startsWith('#') || line.startsWith(';')) continue;
      if (/^\[interface\]$/i.test(line)) { section='interface'; continue; }
      if (/^\[peer\]$/i.test(line)) { section='peer'; result.peers.push({endpoint:'',allowed:[]}); continue; }
      const separator=line.indexOf('='); if(separator<1)continue;
      const key=line.slice(0,separator).trim().toLowerCase(), value=line.slice(separator+1).trim();
      if(section==='interface'){
        if(key==='privatekey')result.hasPrivateKey=!!value;
        else if(key==='address')result.addresses.push(...list(value));
        else if(key==='dns')result.dns.push(...list(value));
        else if(key==='mtu'&&/^\d+$/.test(value))result.mtu=Number(value);
      }else if(section==='peer'&&result.peers.length){
        const peer=result.peers.at(-1); if(key==='endpoint')peer.endpoint=value; else if(key==='allowedips')peer.allowed.push(...list(value));
      }
    }
    return result;
  }
  function bytes(value){const amount=Number(value)||0;if(amount<1024)return `${amount} B`;const units=['KB','MB','GB','TB'];let scaled=amount,index=-1;do{scaled/=1024;index++;}while(scaled>=1024&&index<units.length-1);return `${scaled>=100?scaled.toFixed(0):scaled>=10?scaled.toFixed(1):scaled.toFixed(2)} ${units[index]}`;}
  function handshake(seconds,now=Math.floor(Date.now()/1000)){if(!Number.isFinite(Number(seconds))||Number(seconds)<=0)return 'No handshake yet';const age=Math.max(0,now-Number(seconds));if(age<60)return `${age}s ago`;if(age<3600)return `${Math.floor(age/60)}m ago`;if(age<86400)return `${Math.floor(age/3600)}h ago`;return `${Math.floor(age/86400)}d ago`;}
  const api={inspect,bytes,handshake};
  if(typeof module==='object'&&module.exports)module.exports=api;else globalThis.OpenU60WireGuard=api;
})(globalThis);
