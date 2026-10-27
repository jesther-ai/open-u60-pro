'use strict';
(() => {
  function list(data,family) {
    return [...new Set((Array.isArray(data?.[`ipv${family}-address`]) ? data[`ipv${family}-address`] : []).map(item=>item?.address).filter(value=>typeof value==='string' && value.trim() && value!=='::' && value!=='0.0.0.0'))];
  }
  function collect(wan,wan6) {return {ipv4:[...new Set([...list(wan,4),...list(wan6,4)])],ipv6:[...new Set([...list(wan,6),...list(wan6,6)])]};}
  const api={list,collect};if(typeof module==='object'&&module.exports)module.exports=api;else globalThis.OpenU60Addresses=api;
})();
