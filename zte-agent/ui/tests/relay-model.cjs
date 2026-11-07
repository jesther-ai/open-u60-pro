const assert=require('node:assert/strict'),R=require('../relay-model.js');

// Bands, labels and signal.
assert.equal(R.band(2412),'2.4 GHz');assert.equal(R.band(5260),'5 GHz');assert.equal(R.band('2484'),'2.4 GHz');
assert.equal(R.widthLabel('auto'),'Auto tuning');assert.equal(R.widthLabel('off'),'Keep negotiated bandwidth');assert.equal(R.widthLabel(undefined),'Keep negotiated bandwidth');
assert.equal(R.securityLabel({supported:false,security:'unsupported'}),'Unsupported');assert.equal(R.securityLabel({supported:true,security:'open'}),'Open');assert.equal(R.securityLabel({supported:true,security:'wpa2'}),'WPA2');
assert.deepEqual([-40,-60,-70,-90].map(R.level),['strong','good','fair','weak']);
assert.equal(R.ipv6Label('configured'),'Configured (internet not verified)');assert.equal(R.ipv6Label('no_upstream'),'No usable upstream IPv6; IPv4 only');assert.equal(R.ipv6Label('whatever'),'Off');

// The state of the relay decides the headline, the detail and which stop action exists.
let v=R.view({connected:true,ssid:'Hotel',autostart:true});
assert.deepEqual([v.key,v.chip,v.title,v.stop,v.stopLabel],['linked','WI-FI LINKED','Hotel',true,'Disconnect relay']);
v=R.view({connected:false,restoring:'Hotel',autostart:true});
assert.deepEqual([v.key,v.chip,v.title,v.stopLabel],['restoring','RECONNECTING','Hotel','Stop reconnecting']);
assert.match(v.detail,/Stop reconnecting cancels/);
v=R.view({connected:false,recovery_required:true});
assert.deepEqual([v.key,v.stop,v.stopLabel],['recovery',true,'Retry relay cleanup']);
v=R.view({connected:false,autostart:true});
assert.deepEqual([v.key,v.stop,v.stopLabel],['off',true,'Disconnect & clear restart choice']);
v=R.view({connected:false,autostart:false});
assert.deepEqual([v.key,v.stop,v.title],['off',false,'Share a Wi-Fi connection']);
assert.equal(R.view(null).key,'off');assert.equal(R.view({connected:true}).title,'Wi-Fi relay');
assert.equal(R.view({connected:true,restoring:'Old'}).restoring,'','a linked relay is not also reconnecting');
assert.equal(R.view({connected:false,restoring:''}).key,'off');

// Facts only describe a linked upstream and never invent values.
assert.deepEqual(R.facts({connected:false,address:'10.0.0.2'}),[]);
const facts=R.facts({connected:true,address:'192.168.8.23',gateway:'192.168.8.1',frequency:5260,width:80,width_state:'stable',width_mode:'auto',autostart:true,ipv6:{state:'configured',upstream:true,address:'2001:db8::2',lan_prefix:'fd00:1::/64'}});
assert.deepEqual(facts.map(f=>f[0]),['Upstream IPv4','Gateway','Upstream link','Bandwidth policy','Upstream IPv6 / NAT66','Upstream IPv6','LAN IPv6 prefix','After a restart']);
assert.equal(facts.find(f=>f[0]==='Upstream link')[1],'5 GHz · 80 MHz · stable');assert.equal(facts.find(f=>f[0]==='Bandwidth policy')[1],'Auto tuning');
assert.equal(facts.find(f=>f[0]==='After a restart')[1],'Automatic reconnect enabled');
const bare=R.facts({connected:true});
assert.equal(bare.find(f=>f[0]==='Upstream IPv4')[1],'—');assert.equal(bare.find(f=>f[0]==='Upstream link')[1],'—');assert.equal(bare.find(f=>f[0]==='After a restart')[1],'Unknown');
assert.equal(R.facts({connected:true,frequency:2437,ipv6:{state:'no_upstream'}}).find(f=>f[0]==='Upstream link')[1],'2.4 GHz');

// Password rules mirror the router so mistakes are caught before the hotspot is disturbed.
const wpa={supported:true,security:'wpa2'},open={supported:true,security:'open'};
assert.equal(R.passwordProblem(null,'x',false),'Select a network first.');
assert.equal(R.passwordProblem({supported:false,security:'unsupported'},'',false),'This network is not supported.');
assert.equal(R.passwordProblem(open,'',false),'');assert.match(R.passwordProblem(open,'secret123',false),/do not use a password/);
assert.equal(R.passwordProblem(wpa,'',true),'','a remembered password may be left empty');
assert.match(R.passwordProblem(wpa,'',false),/Enter the Wi-Fi password/);
assert.equal(R.passwordProblem(wpa,'correct horse',false),'');
assert.equal(R.passwordProblem(wpa,'g'.repeat(63),false),'');assert.match(R.passwordProblem(wpa,'g'.repeat(64),false),/8–63/,'64 non-hex characters are too long');
assert.equal(R.passwordProblem(wpa,'a'.repeat(64),false),'','64 hex digits are a raw key, not a passphrase');
assert.equal(R.passwordProblem(wpa,'ab12'.repeat(16),false),'','a 64-digit hex key is accepted');
assert.match(R.passwordProblem(wpa,'short',false),/8–63/);assert.match(R.passwordProblem(wpa,'pass\u00e9word',false),/printable ASCII/);assert.match(R.passwordProblem(wpa,'tab\tpassword',false),/printable ASCII/);

// Request bodies: the saved bandwidth choice is expressed by omitting the field.
assert.deepEqual(R.connectRequest({id:'abc',password:'pw123456',remember:true,ipv6:true,widthMode:'saved'}),{network:'abc',password:'pw123456',confirmed:true,ipv6:true,remember:true});
assert.deepEqual(R.connectRequest({id:'abc',password:'',remember:false,ipv6:false,widthMode:'auto'}),{network:'abc',password:'',confirmed:true,ipv6:false,remember:false,width_mode:'auto'});
assert.equal(R.connectRequest({id:'abc',widthMode:'bogus'}).width_mode,undefined);assert.equal(R.connectRequest({id:'abc'}).ipv6,true);

// Saved-network notes and the consent wording.
assert.equal(R.savedNote({security:'open'}),'Open network; no password needed');
assert.equal(R.savedNote({security:'wpa2',remembered:true}),'Password remembered; leave it empty when connecting');
assert.equal(R.savedNote({security:'wpa2',remembered:false}),'Password not saved; enter it when connecting');
assert.match(R.consent({ssid:'Hotel',frequency:5180},null),/Hotel \(5 GHz\)/);
assert.match(R.consent({ssid:'Hotel',frequency:5180},{connected:true,ssid:'Cafe'}),/replaces the current relay \(Cafe\)/);
assert.doesNotMatch(R.consent({ssid:'Hotel',frequency:5180},{connected:false}),/replaces/);

// Unsupported networks sink; the router’s strongest-first order is otherwise kept; junk is dropped.
const list=[{id:'a',supported:false},{id:'b',supported:true},{id:'c',supported:false},{id:'d',supported:true},null,{ssid:'no id'}];
assert.deepEqual(R.usable(list).map(n=>n.id),['b','d','a','c']);assert.deepEqual(R.usable(undefined),[]);
console.log('Passed: relay state wording, facts, password rules, request bodies and network ordering.');
