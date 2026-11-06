'use strict';
(() => {
  const num = value => value !== null && value !== undefined && typeof value !== 'boolean' && String(value).trim() !== '' && Number.isFinite(Number(value)) ? Number(value) : null;
  const positive = value => num(value) > 0 ? num(value) : null;
  const nonnegative = value => num(value) !== null && num(value) >= 0 ? num(value) : null;
  const percent = value => num(value) !== null && num(value) >= 0 && num(value) <= 100 ? num(value) : null;
  const data = value => value?.ok ? value.data || {} : {};
  const flag = value => [true,1,'1','true','enable','enabled'].includes(value) ? true : [false,0,'0','false','disable','disabled'].includes(value) ? false : null;
  const divide = (value,unit) => num(value) === null ? null : num(value)/unit;
  const validTemp = value => num(value) !== null && num(value) >= -40 && num(value) <= 150 ? num(value) : null;
  function parse(packet = {}) {
    const supplies = data(packet.supplies), battery = supplies.battery || {}, usb = supplies.usb || {};
    const firmware = data(packet.firmware), charger = data(packet.charger), control = data(packet.control);
    const status = String(battery.status || '').trim().toLowerCase();
    const state = ({charging:'Charging',discharging:'Discharging',full:'Full','not charging':'Not charging'})[status] || 'Not reported';
    const voltage = divide(positive(battery.voltage_now),1e6), current = divide(battery.current_now,1e3);
    const usbVoltage = divide(positive(usb.voltage_now),1e6), usbCurrent = divide(usb.current_now,1e3);
    const power = voltage !== null && current !== null ? voltage*Math.abs(current)/1000 : null;
    const usbPower = usbVoltage !== null && usbCurrent !== null ? usbVoltage*Math.abs(usbCurrent)/1000 : null;
    const reportedPower = divide(nonnegative(battery.power_now),1e6);
    function eta(kind,active) {
      if (kind === 'full' && status === 'full') return {seconds:0,source:'Battery is full',applicable:true};
      if (!active) return {seconds:null,source:status ? 'Not applicable in the current state' : 'Battery state not reported',applicable:false};
      for (const key of [`time_to_${kind}_now`,`time_to_${kind}_avg`]) {
        const seconds = positive(battery[key]);
        if (seconds !== null) return {seconds,source:'Device estimate · fuel gauge',applicable:true};
      }
      const minutes = positive(firmware[`battery_time_to_${kind}`]);
      return {seconds:minutes === null ? null : minutes*60,source:minutes === null ? 'No estimate reported' : 'Device estimate · firmware',applicable:true};
    }
    const full = divide(positive(battery.charge_full),1000), design = divide(positive(battery.charge_full_design),1000);
    const activeUsb = String(usb.usb_type || '').match(/\[([^\]]+)\]/)?.[1];
    return {state,status,capacity:percent(battery.capacity) ?? percent(firmware.battery_capacity),
      temperature:validTemp(divide(battery.temp,10)) ?? validTemp(firmware.battery_temperature),
      voltage,current,power,usbVoltage,usbCurrent,usbPower:flag(charger.otg_powerbank_state)===true?null:usbPower,reportedPower,
      powerMismatch:power !== null && reportedPower !== null && Math.abs(power-reportedPower)>Math.max(1,power*.25),
      full:eta('full',status==='charging'&&flag(control.charging_stopped)!==true),empty:eta('empty',status==='discharging'),
      health:battery.health || (num(firmware.battery_health) === null ? 'Not reported' : `Firmware code ${firmware.battery_health}`),
      present:flag(battery.present) ?? flag(firmware.battery_online),technology:battery.technology || 'Not reported',
      cycles:nonnegative(battery.cycle_count),chargeFull:full,chargeDesign:design,
      chargeNow:divide(nonnegative(battery.charge_now),1000),chargeCounter:divide(battery.charge_counter,1000),
      fullDesignRatio:full !== null && design !== null ? full/design*100 : null,
      energyNow:divide(nonnegative(battery.energy_now),1e6),energyFull:divide(positive(battery.energy_full),1e6),
      usbOnline:flag(usb.online) ?? flag(charger.charger_connect),usbType:activeUsb || usb.type || 'Not reported',
      usbTemperature:validTemp(divide(usb.temp,10)),usbMaxVoltage:divide(positive(usb.voltage_max),1e6),
      usbMaxCurrent:divide(positive(usb.current_max),1000),inputLimit:divide(nonnegative(usb.input_current_limit),1000),
      chargeType:battery.charge_type || 'Not reported',bypass:flag(charger.direct_power_supply_mode),powerbank:flag(charger.otg_powerbank_state),
      stopped:flag(control.charging_stopped),manual:flag(control.manual_override),limitEnabled:flag(control.charge_limit_enabled),
      limit:percent(control.charge_limit),hysteresis:percent(control.hysteresis),
      partial:['supplies','firmware','charger','control'].some(key=>!packet[key]?.ok)};
  }
  function duration(seconds) {
    if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return 'Not reported';
    if (seconds === 0) return 'Fully charged';
    const minutes = Math.ceil(seconds/60), days = Math.floor(minutes/1440), hours = Math.floor(minutes%1440/60), rest = minutes%60;
    return [days ? `${days}d` : '',hours ? `${hours}h` : '',rest || (!days&&!hours) ? `${rest}m` : ''].filter(Boolean).join(' ');
  }
  const api = {parse,duration};
  if (typeof module === 'object' && module.exports) module.exports = api; else globalThis.OpenU60Battery = api;
})();
