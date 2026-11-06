'use strict';
(() => {
  const UI = OpenU60UI, API = OpenU60Console, {el} = UI, route = 'tools/battery';
  let sample = null, received = null, unavailable = false;
  const overview = UI.panel(route,'battery-overview','Battery & power','Live battery flow, charging progress and USB input, directly from your router.');
  const state = el('p','battery-state','Waiting for battery readings'), metrics = el('dl','battery-metrics');
  const nodes = {};
  function metric(parent,key,label,description) {
    const box = el('div'), term = el('dt','',label), value = el('dd','mono','—');value.id=`power-${key}`;
    box.append(term,value);if(description)box.append(el('small','field-help',description));parent.append(box);nodes[key]=value;return value;
  }
  metric(metrics,'capacity','Battery level');metric(metrics,'temperature','Battery temperature');
  metric(metrics,'power','Battery power','Calculated from battery voltage × current.');metric(metrics,'usb-power','USB input power','Includes the running router and battery charging.');
  const meter = el('progress','battery-progress');meter.max=100;meter.setAttribute('aria-label','Battery level');meter.removeAttribute('value');
  const powerNote = el('p','panel-note','Power is measured at the battery and USB input; these are different parts of the power path.');
  const caption = el('p','reading-caption','Waiting for battery readings.');overview.append(state,metrics,meter,powerNote,caption);
  const timePanel = UI.panel(route,'battery-estimates','Time estimates','Estimates come from the fuel gauge or firmware and change with load and charging conditions.');
  const times = el('dl','battery-metrics battery-times');metric(times,'until-full','Time until fully charged');metric(times,'until-empty','Runtime remaining');
  const fullSource=el('small','field-help'),emptySource=el('small','field-help');nodes['until-full'].parentElement.append(fullSource);nodes['until-empty'].parentElement.append(emptySource);timePanel.append(times);
  const batteryPanel = UI.panel(route,'battery-electrical','Battery details','Electrical measurements, fuel-gauge capacity and battery condition.');
  const batteryFacts = el('dl','settings-facts battery-facts');
  for(const [key,label] of [['status','Battery state'],['present','Battery present'],['health','Health'],['technology','Chemistry'],['voltage','Battery voltage'],['current','Battery current'],['charge-type','Charge phase'],['cycles','Charge cycles'],['charge-full','Reported full capacity'],['charge-design','Design capacity'],['charge-now','Reported charge remaining'],['charge-counter','Coulomb counter'],['full-design','Full / design ratio'],['energy-now','Reported stored energy'],['energy-full','Reported full energy']])metric(batteryFacts,key,label);
  batteryPanel.append(batteryFacts,el('p','panel-note','The coulomb counter is a relative gauge reading, not usable remaining capacity. Full/design ratios can exceed 100% after calibration. Current is shown as a magnitude; battery state indicates its direction.'));
  const chargerPanel=UI.panel(route,'battery-charger','USB & charge policy','Reported input limits, power modes and the charging policy currently in effect.');
  const chargerFacts=el('dl','settings-facts battery-facts');
  for(const [key,label] of [['usb-online','External power'],['usb-type','USB supply type'],['usb-voltage','USB voltage'],['usb-current','USB current'],['usb-temperature','USB sensor temperature'],['usb-max-voltage','Reported USB voltage limit'],['usb-max-current','Reported USB current limit'],['usb-input-limit','Input current limit'],['bypass','Direct power supply'],['powerbank','USB power bank'],['stopped','Charging stopped'],['manual','Manual override'],['limit-enabled','Charge limit'],['limit','Configured charge limit'],['resume','Configured resume level']])metric(chargerFacts,key,label);
  const settings=el('a','button','Open charging controls →');settings.href='#settings/device';chargerPanel.append(chargerFacts,el('p','panel-note','Configured charge and resume levels are inactive while the charge limit is disabled.'),settings);
  const raw=el('details','diagnostic-details battery-raw'),summary=el('summary','','All reported battery & power fields'),rawOutput=el('pre','','No readings received.');raw.append(summary,el('p','panel-note','Raw sensor and firmware values retain their source units. Kernel voltage/current use µV/µA, temperature uses tenths of °C and time uses seconds; firmware time estimates use minutes. Driver-reported power is diagnostic only; the main wattage uses voltage × current.'),rawOutput);chargerPanel.append(raw);
  const value=(key,n,suffix='',digits=1)=>OpenU60Numbers.set(nodes[key],n===null||n===undefined?'Not reported':`${Number(n).toFixed(digits)}${suffix}`);
  const literal=(key,text)=>OpenU60Numbers.set(nodes[key],text);
  const yes=value=>value===null?'Not reported':value?'Yes':'No';
  function stamp(){caption.textContent=received?`${API.connected&&!unavailable?'Live battery readings':'Last received battery readings'} · ${new Date(received).toLocaleTimeString()}${sample?.partial?' · some sources unavailable':''}`:'Waiting for battery readings.';caption.dataset.stale=String(!API.connected||unavailable||!!sample?.partial);}
  function render(packet){
    sample=OpenU60Battery.parse(packet);const b=sample;
    state.textContent=b.state+(b.stopped?' · charging stopped by policy':'');state.dataset.flow=b.status;
    value('capacity',b.capacity,' %',0);value('temperature',b.temperature,' °C');value('power',b.power,' W',2);if(b.powerbank)OpenU60Numbers.set(nodes['usb-power'],'Not applicable');else value('usb-power',b.usbOnline===false?null:b.usbPower,' W',2);
    if(b.capacity===null)meter.removeAttribute('value');else meter.value=b.capacity;
    powerNote.textContent=b.powerMismatch?'Battery wattage is calculated from measured voltage × current. The driver’s separate power reading is inconsistent; it is preserved in the reported fields below.':'Battery power uses measured voltage × current. USB input also powers the router, so it differs from battery power and the adapter’s rated output.';
    for(const [key,estimate]of[['until-full',b.full],['until-empty',b.empty]])OpenU60Numbers.set(nodes[key],estimate.applicable?OpenU60Battery.duration(estimate.seconds):'Not applicable');fullSource.textContent=b.full.source;emptySource.textContent=b.empty.source;
    literal('status',b.state);literal('present',yes(b.present));literal('health',b.health);literal('technology',b.technology);literal('charge-type',b.chargeType);
    for(const [key,n,unit,digits]of[['voltage',b.voltage,' V',3],['current',b.current===null?null:Math.abs(b.current),' mA',0],['cycles',b.cycles,'',0],['charge-full',b.chargeFull,' mAh',0],['charge-design',b.chargeDesign,' mAh',0],['charge-now',b.chargeNow,' mAh',0],['charge-counter',b.chargeCounter,' mAh',0],['full-design',b.fullDesignRatio,' %',1],['energy-now',b.energyNow,' Wh',2],['energy-full',b.energyFull,' Wh',2],['usb-voltage',b.usbVoltage,' V',3],['usb-current',b.usbCurrent===null?null:Math.abs(b.usbCurrent),' mA',0],['usb-temperature',b.usbTemperature,' °C',1],['usb-max-voltage',b.usbMaxVoltage,' V',2],['usb-max-current',b.usbMaxCurrent,' mA',0],['usb-input-limit',b.inputLimit,' mA',0]])value(key,n,unit,digits);
    literal('usb-online',b.usbOnline===null?'Not reported':b.usbOnline?'Connected':'Disconnected');literal('usb-type',b.usbType);
    for(const[key,n]of[['bypass',b.bypass],['powerbank',b.powerbank],['stopped',b.stopped],['manual',b.manual]])literal(key,yes(n));
    literal('limit-enabled',b.limitEnabled===null?'Not reported':b.limitEnabled?'Enabled':'Disabled');value('limit',b.limit,' %',0);value('resume',b.limit!==null&&b.hysteresis!==null?Math.max(0,b.limit-b.hysteresis):null,' %',0);
    rawOutput.textContent=JSON.stringify(packet,null,2);stamp();
  }
  function clear(){sample=null;received=null;unavailable=false;for(const node of Object.values(nodes))OpenU60Numbers.set(node,'—');meter.removeAttribute('value');state.textContent='Waiting for battery readings';rawOutput.textContent='No readings received.';fullSource.textContent=emptySource.textContent='';stamp();}
  window.addEventListener('dashboard:readings',event=>{const reading=event.detail.readings.battery_details;if(!reading||OpenU60Navigation.current.key!==route)return;if(reading.ok){received=Date.now();unavailable=false;render(reading.data);}else{unavailable=true;stamp();}});
  window.addEventListener('dashboard:connection',stamp);
  window.addEventListener('dashboard:locked',clear);
  window.addEventListener('dashboard:navigate',()=>{if(OpenU60Navigation.current.key!==route)clear();});
})();
