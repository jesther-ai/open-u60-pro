const assert = require('node:assert/strict');
const {parse} = require('../cellular.js');
const fixture = {
  network_type:'LTE-NSA',lte_rsrp:'-90',lte_rsrq:'-11',lte_snr:'18',lte_rssi:'-62',lte_pci:'265',wan_active_band:'3',wan_active_channel:'1775',cell_id:'0x12AB01',
  lteca:'265,3,0,1775,15;265,1,1,150,20;19,7,2,2850,20;',ltecasig:'-94,-12,16,-65;-99,-14,10,-70;',
  nr5g_rsrp:'-84',nr5g_rsrq:'-9',nr5g_snr:'23',nr5g_rssi:'-58',nr5g_pci:'231',nr5g_action_band:'78',nr5g_action_channel:'633984',nr5g_cell_id:'0x3456701',
  nrca:'231,78,0,633984,100;231,78,1,641280,100;',nrcasig:'-91,-11,17,-64;'
};
let parsed = parse(fixture);
assert.equal(parsed.mode,'5G NSA');
assert.equal(parsed.lte.carriers.length,3);
assert.equal(parsed.nr.carriers.length,2);
assert.equal(parsed.lte.carriers[0].bandwidth,'15');
assert.equal(parsed.lte.carriers[1].rsrp,-94);
assert.equal(parsed.nr.carriers[1].rsrp,-91);
assert.equal(parsed.nr.carriers[1].cellId,'','secondary cell IDs must not be invented from the primary');
assert.equal(parsed.nr.carriers[1].channel,'641280','same PCI on a different channel is a distinct carrier');
parsed = parse({...fixture,network_type:'NR-SA'});
assert.equal(parsed.mode,'5G SA');
assert.equal(parse({...fixture,network_type:'NR5G-SA'}).mode,'5G SA'); assert.equal(parsed.lte.inUse,false); assert.equal(parsed.lte.carriers.length,3,'SA retains reported LTE metadata without calling it active');
parsed = parse({...fixture,lteca:fixture.lteca+'265,1,1,150,20;',ltecasig:fixture.ltecasig+'-93,-12,16,-63;'});
assert.equal(parsed.lte.carriers.length,3,'duplicate carrier entries are not counted twice');
parsed = parse({...fixture,nr5g_rsrp:0,nrca:'0,0,0,0,0;',nrcasig:'',network_type:'LTE-NSA'});
assert.equal(parsed.mode,'4G+'); assert.equal(parsed.nr.carriers[0].rsrp,null);
assert.equal(parse({network_type:'5G',nr5g_rsrp:-88}).mode,'5G · mode unreported');
assert.equal(parse({network_type:'NO SERVICE',lte_rsrp:0,nr5g_rsrp:'N/A',lteca:'0,0,0,0,0;'}).hasSignal,false);
parsed = parse({...fixture,lteca:'265,3,0,1775,15;bad;19,7,2,2850,20;',ltecasig:'bad;-101,-13,12,-74;'});
assert.equal(parsed.lte.carriers[1].rsrp,-101,'malformed entries do not shift later carrier measurements');
parsed = parse({...fixture,nrcasig:'',nr5g_rsrp:99999});
assert.equal(parsed.nr.carriers[0].rsrp,null); assert.equal(parsed.nr.carriers[1].rsrp,null);
assert.equal(parse({...fixture,network_type:'LTE',nr5g_rsrp:0,nrca:'',lteca_state:'0'}).mode,'4G');
assert.equal(parse({network_type:'NR-SA',lte_rsrp:-90,wan_active_band:3}).primary,null,'reported LTE must not become the serving summary in SA');
parsed = parse({network_type:'SA',wan_active_band:'n78',lte_rsrp:0,lte_rsrq:0,lte_rssi:0,lteca:'',nr5g_action_band:'n78',nr5g_action_channel:632640,nr5g_pci:635,nr5g_cell_id:12891590913,nr5g_rsrp:-88,nr5g_rsrq:-11,nr5g_snr:12.9,nr5g_rssi:-73,nrca:''});
assert.equal(parsed.mode,'5G SA');assert.equal(parsed.lte.carriers.length,0,'an NR-prefixed shared active band must not fabricate an LTE carrier');assert.equal(parsed.nr.carriers.length,1);assert.equal(parsed.nr.carriers[0].band,'n78');
console.log('Passed: LTE/NR CA parsing, PCI+channel primary matching, SCC measurements, deduplication, SA/NSA, missing and sentinel readings.');
