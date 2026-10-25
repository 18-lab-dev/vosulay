#!/usr/bin/env node
// Real service + isolated headless Chromium; telemetry is synthetic, never game memory.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, readFile, writeFile, symlink, readdir, readlink, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const option = key => args[args.indexOf(key) + 1];
for (const key of ['--binary', '--chromium', '--cef-dir', '--counter-dir', '--output']) {
  assert(args.includes(key) && option(key), `${key} is required`);
}
if (!args.includes('--inside')) {
  const outer = await readlink('/proc/self/ns/net');
  const child = spawn('unshare', ['--user','--map-current-user','--net','--keep-caps','sh','-c',
    'ip link set lo up && exec setpriv --inh-caps=-all --ambient-caps=-all "$@"','sh',
    process.execPath,fileURLToPath(import.meta.url),...args,'--inside','--outer-netns',outer], {stdio:'inherit'});
  process.exit((await once(child,'exit'))[0] ?? 1);
}
assert.notEqual(await readlink('/proc/self/ns/net'), option('--outer-netns'), 'private network namespace is required');
const output = path.resolve(option('--output'));
await mkdir(output, { recursive: true });
const root = await mkdtemp(path.join(tmpdir(), 'vosulay-preview-security-'));
const environment = { ...process.env };
for (const key of Object.keys(environment)) {
  if (/^(XDG_|VOSU_|VOSULAY_)/.test(key) || ['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY'].includes(key)) delete environment[key];
}
for (const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','XDG_RUNTIME_DIR','XDG_STATE_HOME','XDG_CONFIG_DIRS','XDG_DATA_DIRS']) {
  environment[key] = path.join(root, key.toLowerCase());
  await mkdir(environment[key], { mode: 0o700 });
}
const cef = path.resolve(option('--cef-dir'));
const runtime = path.join(root, 'runtime');
await mkdir(runtime);
for (const directory of ['Release', 'Resources']) {
  for (const file of await readdir(path.join(cef, directory))) {
    try { await symlink(path.join(cef, directory, file), path.join(runtime, file)); }
    catch (error) { if (error.code !== 'EEXIST') throw error; }
  }
}
environment.LD_LIBRARY_PATH = runtime;
environment.VOSULAY_CEF_RUNTIME = runtime;
environment.VOSULAY_ENDPOINT = '@' + path.basename(root);
const processes = [];
const servers = [];
const sockets = new Set();
const secrets = [];
const redact = value => secrets.reduce((text, secret) => text.replaceAll(secret, '[preview-capability]'), String(value));
const logs = [];
let servicePort;
let upstream;
let preview;
let broker;
const token = randomBytes(32).toString('hex');
const localToken = randomBytes(16).toString('hex');
const localRoot = path.join(root, 'local');
await mkdir(localRoot);
await writeFile(path.join(localRoot, 'style.css'), 'body{color:red}');
const localPath = `/local/${localToken}/style.css`;
const font = await readFile(path.join(repository, 'assets/fonts/barlow-latin-500-normal.woff2'));

const counterProbe = `
(async()=>{
  await document.fonts.ready;
  await new Promise((resolve,reject)=>{const until=Date.now()+8000;function check(){
    if(document.getElementById('pp')?.textContent==='123')resolve();
    else if(Date.now()>until)reject(new Error('synthetic pp timed out'));else setTimeout(check,25);
  }check();});
  const denied=async fn=>{try{await fn();return false}catch{return true}};
  const proof={pp:document.getElementById('pp').textContent,font:getComputedStyle(document.querySelector('.base')).fontFamily,fontLoaded:Array.from(document.fonts).some(font=>font.status==='loaded'),
    parentDenied:await denied(()=>parent.document.body),bridgeDenied:await denied(()=>parent.__VOSULAY_POST_MESSAGE__),
    managerDenied:await denied(async()=>{const r=await fetch('__PARENT__/tools/vosulay/api/v1/manage/state');if(!r.ok)throw new Error('denied');})};
  parent.postMessage('editingEnded','*');parent.postMessage({type:'preview-security-proof',proof},'*');
})().catch(()=>parent.postMessage({type:'preview-security-proof',proof:{failed:true}},'*'));
`;
const counterDirectory = path.resolve(option('--counter-dir'));
const mime = file => ({'.html':'text/html','.js':'text/javascript','.css':'text/css','.woff2':'font/woff2','.ttf':'font/ttf'})[path.extname(file)] || 'application/octet-stream';
const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url, upstream || 'http://127.0.0.1');
    const send = (status, body, type='text/plain', headers={}) => { response.writeHead(status, { 'Content-Type':type, ...headers }); response.end(body); };
    if (url.pathname === '/manager') {
      const reply = await fetch(`http://127.0.0.1:${servicePort}/v1/manage/document`, {headers:{'X-Vosu-Tool-Token':token}});
      let html = await reply.text();
      const bootstrap = `<script>window.__previewProof=new Promise(resolve=>window.addEventListener('message',event=>{if(event.data?.type==='preview-security-proof'&&Array.from(document.querySelectorAll('iframe')).some(frame=>frame.contentWindow===event.source))resolve(event.data.proof);}));window.__VOSULAY_TOOL_FETCH__=(p,i)=>fetch('/tools/vosulay/api/'+p.replace(/^\\//,''),i);</script>`;
      html = html.replace('<head>', '<head>'+bootstrap);
      return send(reply.status, html, 'text/html');
    }
    if (url.pathname.startsWith('/tools/vosulay/api/')) {
      if (request.headers.origin && request.headers.origin !== upstream.slice(0,-1)) return send(403, 'No manager authority');
      const body = [];
      for await (const chunk of request) body.push(chunk);
      const reply = await fetch(`http://127.0.0.1:${servicePort}/`+url.pathname.slice('/tools/vosulay/api/'.length), {
        method:request.method, headers:{'X-Vosu-Tool-Token':token}, ...(body.length ? {body:Buffer.concat(body)} : {}) });
      return send(reply.status, Buffer.from(await reply.arrayBuffer()), reply.headers.get('content-type') || 'text/plain');
    }
    if (url.pathname === '/api/counters/search/') return send(200, '<div class="no-results">No installed counters</div>', 'text/html');
    if (url.pathname === '/api/counters/profiles') return send(200, JSON.stringify({active:'default',profiles:['default'],writable:false}), 'application/json');
    if (url.pathname === '/style.css') return send(200, 'body{color:red}', 'text/css', {'Cross-Origin-Resource-Policy':'same-origin'});
    if (url.pathname === '/module.js') return send(200, 'export default 1;', 'text/javascript', {'Cross-Origin-Resource-Policy':'same-origin'});
    if (url.pathname === '/font.woff2') return send(200, font, 'font/woff2', {'Cross-Origin-Resource-Policy':'same-origin'});
    const match = url.pathname.match(/^\/(?:InGame2|counter)\/(.*)$/);
    if (match) {
      const file = path.resolve(counterDirectory, match[1] || 'index.html');
      if (!file.startsWith(counterDirectory+path.sep)) return send(403, 'Confined');
      let bytes = await readFile(file);
      if (path.extname(file) === '.html') bytes = Buffer.from(bytes.toString()+`<script>${counterProbe.replace('__PARENT__',upstream.slice(0,-1))}</script>`);
      return send(200, bytes, mime(file), {'Cross-Origin-Resource-Policy':'same-origin'});
    }
    send(404, 'Not found');
  } catch (error) { response.writeHead(500); response.end('Fixture error'); logs.push(redact(error.stack)); }
});
server.on('upgrade', (request, socket) => {
  const key = request.headers['sec-websocket-key'];
  if (!key) return socket.destroy();
  sockets.add(socket);
  socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: '+createHash('sha1').update(key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64')+'\r\n\r\n');
  const payload = request.url.startsWith('/websocket/v2/subscribe') ? [0,1,[{number:0,name:'osu'}]] : {
    menu:{state:2,pp:{100:222},bm:{time:{current:50,mp3:100}}},gameplay:{pp:{current:123,fc:234},hits:{100:1,50:2,0:3,sliderBreaks:4}}};
  const bytes=Buffer.from(JSON.stringify(payload));
  const header=bytes.length<126?Buffer.from([0x81,bytes.length]):Buffer.from([0x81,126,bytes.length>>8,bytes.length&255]);
  const timer=setInterval(()=>socket.write(Buffer.concat([header,bytes])),100);
  socket.on('data',()=>{});socket.on('error',()=>{});
  socket.on('close',()=>{clearInterval(timer);sockets.delete(socket);});
});
async function listen(server) { servers.push(server);server.listen(0,'127.0.0.1');await once(server,'listening');return `http://127.0.0.1:${server.address().port}/`; }
async function launch(binary, argv, env=environment) {
  const child=spawn(binary,argv,{env,stdio:['pipe','pipe','pipe']});processes.push(child);
  child.stderr.on('data',bytes=>logs.push(redact(bytes.toString())));
  return child;
}
async function startTool() {
  const child=await launch(path.resolve(option('--binary')),['--server-only','--config',path.join(root,'config.json')],{
    ...environment,VOSU_TOOL_PROTOCOL:'1',VOSU_TOOL_TOKEN:token,VOSU_URL:upstream,VOSU_TOOL_DATA_DIR:path.join(root,'tool-data'),VOSU_TOOL_CACHE_DIR:path.join(root,'tool-cache')});
  const line=await new Promise((resolve,reject)=>{let text='';const timer=setTimeout(()=>reject(new Error('readiness timeout')),20000);child.stdout.on('data',bytes=>{text+=bytes;if(text.includes('\n')){clearTimeout(timer);resolve(text.split('\n')[0]);}});child.once('exit',()=>{clearTimeout(timer);reject(new Error('service exited before readiness'));});});
  assert.match(line,/^vosu-tool-ready \d+$/);servicePort=Number(line.split(' ')[1]);
  const reply=await fetch(`http://127.0.0.1:${servicePort}/v1/manage/state`,{headers:{'X-Vosu-Tool-Token':token}});
  preview=new URL((await reply.json()).routing.view);secrets.push(preview.hostname);
  broker=`http://127.0.0.1:${preview.port}`;
  return child;
}

async function attackerProbe(plan) {
  const results=[];
  const check=async(kind,url)=>{
    if(kind==='fetch'){try{return(await fetch(url)).ok}catch{return false}}
    if(kind==='xhr')return await new Promise(resolve=>{const xhr=new XMLHttpRequest();xhr.open('GET',url);xhr.onload=()=>resolve(xhr.status>=200&&xhr.status<300);xhr.onerror=()=>resolve(false);xhr.timeout=2500;xhr.ontimeout=()=>resolve(false);xhr.send();});
    if(kind==='module'){try{await import(url);return true}catch{return false}}
    if(kind==='font'){try{await new FontFace('ProbeFont',`url(${JSON.stringify(url)})`).load();return true}catch{return false}}
    if(kind==='socket')return await new Promise(resolve=>{const ws=new WebSocket(url);const timer=setTimeout(()=>{ws.close();resolve(false)},2500);ws.onopen=()=>{clearTimeout(timer);ws.close();resolve(true)};ws.onerror=()=>{clearTimeout(timer);resolve(false)};});
  };
  for(const item of plan){const allowed=await check(item.kind,item.url);results.push({target:item.target,route:item.route,kind:item.kind,allowed,expected:item.expected});}
  parent.postMessage({type:'hostile-results',results},'*');
}
let debugSocket;
let onLoad;
let sequence=0;
const pending=new Map();
async function command(method,params={}) {const id=++sequence;const result=new Promise((resolve,reject)=>pending.set(id,{resolve,reject}));debugSocket.send(JSON.stringify({id,method,params}));return result;}
async function pageResult(url, expression) {
  const loaded = new Promise(resolve => { onLoad = resolve; });
  await command('Page.navigate',{url});
  await loaded;
  const result=await command('Runtime.evaluate',{expression:`(async()=>{while(document.readyState!=='complete')await new Promise(r=>setTimeout(r,20));return await (${expression});})()`,awaitPromise:true,returnByValue:true});
  if(result.exceptionDetails)throw new Error(result.exceptionDetails.text);
  return result.result.value;
}
try {
  upstream=await listen(server);
  const config={version:1,active_profile:'default',tosu_migration_completed:true,profiles:[{id:'default',name:'Preview security',overlays:[
    {counter_id:'InGame2',name:'InGame2',url:upstream+'InGame2/',top:180,left:300,width:800,height:100,scale:1,z_index:0},
    {counter_id:'local',url:`vosulay-overlay://${localToken}/${Buffer.from(localRoot).toString('base64url')}/index.html`,top:0,left:0,width:1,height:1,scale:1,z_index:1,enabled:false}]}],settings:{vosu_url:upstream,allow_local_profile_editing:true}};
  await writeFile(path.join(root,'config.json'),JSON.stringify(config));
  const child=await startTool();
  const plan=[];
  const reserved=['/overlay','/__vosulay/view-state','/__vosulay/backend.js','/__vosulay/manage/state','/v1/manage/state','/config.json'];
  for(const [target,origin] of [['plain',broker],['guessed',`http://guessed.preview.localhost:${preview.port}`],['known-capability',preview.origin]]) {
    for(const route of [...reserved,localPath,'/local/'+ '0'.repeat(32)+'/style.css','/module.js'])for(const kind of ['fetch','xhr','module','font']) {
      plan.push({target,route:route===localPath?'local/owned':route,url:origin+route,kind,expected:target==='known-capability'&&((route===localPath&&['fetch','xhr'].includes(kind))||(route==='/module.js'&&kind!=='font'))});
    }
    plan.push({target,route:'/ws',url:origin.replace('http:','ws:')+'/ws',kind:'socket',expected:target==='known-capability'});
  }
  for(const [kind,route] of [['fetch','/style.css'],['module','/module.js'],['font','/font.woff2']])plan.push({target:'known-capability',route,url:preview.origin+route,kind,expected:true});
  const childSource = JSON.stringify('<script>('+attackerProbe.toString()+')('+JSON.stringify(plan)+');</script>').replaceAll('<', '\\u003c');
  const hostile=createServer((request,response)=>{response.writeHead(200,{'Content-Type':'text/html'});response.end(`<!doctype html><body><script>window.__hostileDone=new Promise(resolve=>{const frame=document.createElement('iframe');frame.sandbox='allow-scripts';window.addEventListener('message',e=>{if(e.source===frame.contentWindow&&e.data?.type==='hostile-results')resolve(e.data.results)});frame.srcdoc=${childSource};document.body.append(frame);});</script>`);});
  const hostileOrigin=await listen(hostile);
  const chrome=await launch('bwrap',['--die-with-parent','--unshare-user','--unshare-pid','--unshare-ipc','--ro-bind','/','/','--dev','/dev','--proc','/proc','--tmpfs','/tmp','--bind',root,root,'--',path.resolve(option('--chromium')),'--headless=new','--no-sandbox','--disable-gpu','--remote-debugging-port=0','--user-data-dir='+path.join(root,'chrome'),'about:blank']);
  const endpoint=await new Promise((resolve,reject)=>{let text='';const timer=setTimeout(()=>reject(new Error('Chromium endpoint timeout')),15000);chrome.stderr.on('data',bytes=>{text+=bytes;const match=text.match(/DevTools listening on (ws:\/\/[^\s]+)/);if(match){clearTimeout(timer);resolve(match[1]);}});});
  const targets=await(await fetch('http://'+new URL(endpoint).host+'/json/list')).json();
  debugSocket=new WebSocket(targets.find(target=>target.type==='page').webSocketDebuggerUrl);await once(debugSocket,'open');
  debugSocket.addEventListener('message',event=>{const value=JSON.parse(event.data);if(value.method==='Page.loadEventFired')onLoad?.();const waiter=pending.get(value.id);if(waiter){pending.delete(value.id);if(value.error)waiter.reject(new Error(value.error.message));else waiter.resolve(value.result);}});
  await command('Page.enable');await command('Runtime.enable');
  await command('Emulation.setDeviceMetricsOverride',{width:1440,height:900,deviceScaleFactor:1,mobile:false});
  const hostileResults=await pageResult(hostileOrigin,'window.__hostileDone');
  for(const result of hostileResults)assert.equal(result.allowed,result.expected,`${result.target} ${result.route} ${result.kind}`);
  const counter=await pageResult(upstream+'manager','window.__previewProof');
  assert.equal(counter.pp,'123');assert.equal(counter.font,'Custom-Font');assert.equal(counter.fontLoaded,true);assert(counter.parentDenied&&counter.bridgeDenied&&counter.managerDenied,JSON.stringify(counter));
  const editor=await command('Runtime.evaluate',{expression:"document.body.classList.contains('editing')",returnByValue:true});assert.equal(editor.result.value,true);
  const screenshot=await command('Page.captureScreenshot',{format:'png'});await writeFile(path.join(output,'preview.png'),Buffer.from(screenshot.data,'base64'));
  if (args.includes('--cef-smoke')) {
    assert(args.includes('--xvfb'), '--xvfb is required with --cef-smoke');
    const cefOutput=path.join(root,'cef-proof');await mkdir(cefOutput);
    const runner=String.raw`
import os,sys,subprocess,selectors
binary,xvfb,runtime,output,url=sys.argv[1:]
r,w=os.pipe()
display_env=dict(os.environ)
display_env.pop('LD_LIBRARY_PATH',None)
display_env.update(__GLX_VENDOR_LIBRARY_NAME='mesa',__EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json')
server=subprocess.Popen([xvfb,'-displayfd',str(w),'-screen','0','1025x361x24','-nolisten','tcp','-noreset'],pass_fds=(w,),env=display_env)
os.close(w)
try:
    with selectors.DefaultSelector() as ready:
        ready.register(r,selectors.EVENT_READ)
        if not ready.select(10): raise RuntimeError('private Xvfb timeout')
        display=os.read(r,32).decode().strip()
    if not display.isdecimal(): raise RuntimeError('private Xvfb failed')
    env=dict(os.environ,DISPLAY=':'+display,XDG_SESSION_TYPE='x11',LIBGL_ALWAYS_SOFTWARE='1',GALLIUM_DRIVER='llvmpipe',LP_NUM_THREADS='2',__EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json',__GLX_VENDOR_LIBRARY_NAME='mesa')
    subprocess.run([binary,'--cef-dir',runtime,'--output',output,'--preview-url',url,'--ozone-platform=x11','--use-gl=angle','--use-angle=swiftshader','--no-sandbox'],env=env,check=True,timeout=30)
finally:
    os.close(r)
    server.terminate()
    try: server.wait(timeout=5)
    except subprocess.TimeoutExpired: server.kill();server.wait()
`;
    const cefProcess=await launch('bwrap',['--die-with-parent','--unshare-user','--unshare-pid','--unshare-ipc','--ro-bind','/','/','--dev','/dev','--proc','/proc','--tmpfs','/tmp','--ro-bind',path.resolve(option('--xvfb')),'/tmp/proof-Xvfb','--ro-bind',path.resolve(option('--cef-smoke')),'/tmp/proof-cef-smoke','--ro-bind',cef,cef,'--bind',root,root,'--','/usr/bin/python3','-c',runner,'/tmp/proof-cef-smoke','/tmp/proof-Xvfb',runtime,cefOutput,new URL('InGame2/',preview).href]);
    cefProcess.stdout.on('data',bytes=>logs.push(redact(bytes.toString())));
    assert.equal((await once(cefProcess,'exit'))[0],0,'private CEF resolver proof');
    for(const file of ['preview-result.json','preview.ppm'])await writeFile(path.join(output,'cef-'+file),await readFile(path.join(cefOutput,file)));
  }
  const oldHost=preview.hostname;child.stdin.end();assert.equal((await once(child,'exit'))[0],0);
  const replacement=await startTool();assert.notEqual(preview.hostname,oldHost);
  replacement.stdin.end();assert.equal((await once(replacement,'exit'))[0],0);
  const spare=createServer();const standaloneOrigin=await listen(spare);
  await new Promise(resolve=>spare.close(resolve));
  config.settings.remote_listen_host='127.0.0.1';
  config.settings.remote_listen_port=Number(new URL(standaloneOrigin).port);
  await writeFile(path.join(root,'config.json'),JSON.stringify(config));
  const standalone=await launch(path.resolve(option('--binary')),['--server-only','--config',path.join(root,'config.json')]);
  standalone.stdout.on('data',bytes=>logs.push(redact(bytes.toString())));
  const standaloneDeadline=Date.now()+15000;
  for(;;){
    try { if((await fetch(standaloneOrigin+'overlay')).ok) break; } catch {}
    assert.equal(standalone.exitCode,null,'standalone exited before readiness');
    assert(Date.now()<standaloneDeadline,'standalone listener timed out');
    await new Promise(resolve=>setTimeout(resolve,25));
  }
  const standaloneProbe=await command('Page.addScriptToEvaluateOnNewDocument',{source:"window.__standaloneProof=new Promise(resolve=>window.addEventListener('message',event=>{if(event.data?.type==='preview-security-proof'&&Array.from(document.querySelectorAll('iframe')).some(frame=>frame.contentWindow===event.source))resolve(event.data.proof);}));"});
  const standaloneCounter=await pageResult(standaloneOrigin.replace('127.0.0.1','manage.localhost')+'overlay','window.__standaloneProof');
  assert.equal(standaloneCounter.pp,'123');assert.equal(standaloneCounter.font,'Custom-Font');assert.equal(standaloneCounter.fontLoaded,true);
  await command('Page.removeScriptToEvaluateOnNewDocument',{identifier:standaloneProbe.identifier});
  standalone.kill('SIGTERM');assert.equal((await once(standalone,'exit'))[0],0);
  await writeFile(path.join(output,'results.json'),JSON.stringify({hostile:hostileResults,counter,standaloneCounter,childEditIgnored:true,restartRegenerated:true},null,2));
  console.log(`Passed ${hostileResults.length} hostile resource/socket cases; styled/live InGame2, parent isolation and capability rotation. No game or desktop used.`);
} finally {
  debugSocket?.close();
  for(const child of processes)if(child.exitCode===null){child.kill('SIGTERM');await Promise.race([once(child,'exit'),new Promise(resolve=>setTimeout(resolve,5000))]);if(child.exitCode===null)child.kill('SIGKILL');}
  for(const socket of sockets)socket.destroy();
  for(const server of servers)await new Promise(resolve=>server.close(resolve));
  await writeFile(path.join(output,'diagnostic.log'),redact(logs.join('')));
  await rm(root,{recursive:true,force:true});
}
