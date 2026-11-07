// Shared by the browser suites for the network extensions: serves the real
// console files, a fake dashboard stream and a caller-supplied fake API. It
// never contacts a router.
const http=require('node:http'),fs=require('node:fs'),path=require('node:path');
const envelope=data=>({ok:true,data});
const readings={modem:envelope({operate_mode:'ONLINE'}),signal:envelope({network_type:'5G'}),wan:envelope({up:true}),wan6:envelope({up:true}),speed:envelope({}),battery:envelope({capacity:80}),device:envelope({hostname:'U60'}),clients:envelope({hosts:{},dhcp_leases:[]}),sms_capacity:envelope({})};
const send=(res,data,status=200)=>{res.writeHead(status,{'Content-Type':'application/json'});res.end(JSON.stringify(status<300?{ok:true,data}:{ok:false,error:data}));};

// `api({method,url,body,socket},reply,fail)` returns true when it answered.
async function start(api){
  const requests=[],streams=new Set();
  const server=http.createServer((req,res)=>{
    if(req.url.startsWith('/ui/mihomo/dashboard/')){res.writeHead(200,{'Content-Type':'text/html'});res.end('<!doctype html><title>Zashboard stand-in</title>');return;}
    if(!req.url.startsWith('/api/')){
      const file=req.url==='/'?'index.html':/^\/(?:ui|console\/a)\/[\w.-]+$/.test(req.url)?req.url.split('/').pop():'',full=path.join(__dirname,'..',file);
      if(!file||!fs.existsSync(full)){res.writeHead(404);res.end();return;}
      res.setHeader('Content-Type',file.endsWith('.css')?'text/css':file.endsWith('.js')?'text/javascript':'text/html');
      res.setHeader('Content-Security-Policy',"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'");
      res.end(fs.readFileSync(full));return;
    }
    if(req.url.startsWith('/api/dashboard/events')){
      res.writeHead(200,{'Content-Type':'text/event-stream'});streams.add(res);res.on('close',()=>streams.delete(res));
      res.write(`event: snapshot\ndata: ${JSON.stringify({readings})}\n\n`);return;
    }
    let raw='';req.on('data',chunk=>raw+=chunk);
    req.on('end',()=>{
      const body=raw?JSON.parse(raw):null,entry={url:req.url,method:req.method,body};requests.push(entry);
      const handled=api({...entry,socket:req.socket,res},data=>send(res,data),(message,status=503)=>send(res,message,status));
      if(!handled)send(res,{});
    });
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const heartbeat=setInterval(()=>{for(const res of streams)res.write(': heartbeat\n\n');},1000);
  return {
    requests,
    url:`http://127.0.0.1:${server.address().port}`,
    count:(method,url)=>requests.filter(r=>r.method===method&&r.url===url).length,
    async close(){clearInterval(heartbeat);for(const stream of streams)stream.destroy();server.closeAllConnections();server.close();}
  };
}
module.exports={start,envelope};
