use anyhow::{Context, Result, ensure};
use std::{
    cell::RefCell,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use vosulay_browser::{
    BrowserOptions, CefRuntime, RuntimeOptions,
    scheme::{MANAGER_URL, Resource, ResourceRoute, ResourceService},
};
use vosulay_frame::{BrowserFrame, BrowserFrameBackend};

struct Resources(Arc<[u8]>);
impl ResourceService for Resources {
    fn load(&self, route: ResourceRoute<'_>) -> Option<Resource> {
        matches!(
            route,
            ResourceRoute::Manager {
                path: "/index.html"
            }
        )
        .then(|| Resource {
            bytes: self.0.clone(),
            mime_type: "text/html".into(),
            status: 200,
        })
    }
}
struct Sink(Rc<RefCell<Vec<f64>>>, Instant);
impl BrowserFrameBackend for Sink {
    fn resize(&mut self, _: u32, _: u32, _: f32) -> Result<()> {
        Ok(())
    }
    fn set_visible(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
    fn submit_frame(&mut self, _: BrowserFrame<'_>) -> Result<()> {
        self.0.borrow_mut().push(self.1.elapsed().as_secs_f64());
        Ok(())
    }
}
const OVERLAY: &str = r#"<!doctype html><canvas width=500 height=750></canvas><script>
const id=+new URL(location).searchParams.get('id'), stamps=[], real=[], start=performance.now();
const c=document.querySelector('canvas').getContext('2d');
parent.postMessage({id,kind:'startup',routing:typeof window.__VOSULAY_BACKEND_URL__==='function'},'*');
function frame(t){stamps.push(t);real.push(performance.now());c.fillStyle=`hsl(${t%360} 100% 50%)`;c.fillRect(0,0,80,80);if(performance.now()-start<3000)requestAnimationFrame(frame);else parent.postMessage({id,stamps,real,startEpoch:performance.timeOrigin+start,endEpoch:performance.timeOrigin+performance.now(),duration:performance.now()-start,routing:typeof window.__VOSULAY_BACKEND_URL__==='function'},'*')}
requestAnimationFrame(frame);
</script>"#;
fn respond(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request = [0; 8192];
    let Ok(length) = stream.read(&mut request) else {
        return;
    };
    let request = String::from_utf8_lossy(&request[..length]);
    let target = request
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("");
    let prepare = target.starts_with("/__vosulay_prepare");
    let body = if prepare {
        "<!doctype html><title>Preparing</title>"
    } else {
        OVERLAY
    };
    let policy = if prepare {
        "Content-Security-Policy: default-src 'none'; frame-ancestors vosulay://manager\r\n"
    } else {
        ""
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nCache-Control: no-store\r\n{policy}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

const CRASH_RENDERER: &str = r#"
class CDP {
 constructor(url){this.ws=new WebSocket(url);this.id=0;this.pending=new Map();this.ws.onmessage=e=>{const m=JSON.parse(e.data);if(m.method==='Tracing.tracingComplete')this.trace=m.params;if(m.id&&this.pending.has(m.id)){const p=this.pending.get(m.id);this.pending.delete(m.id);m.error?p.reject(Error(JSON.stringify(m.error))):p.resolve(m.result)}}}
 async ready(){if(this.ws.readyState===1)return;await new Promise((resolve,reject)=>{this.ws.onopen=resolve;this.ws.onerror=reject})}
 call(method,params={}){const id=++this.id;return new Promise((resolve,reject)=>{this.pending.set(id,{resolve,reject});this.ws.send(JSON.stringify({id,method,params}))})}
}
const targets=await(await fetch('http://127.0.0.1:39230/json/list')).json();
const target=targets.find(t=>t.type==='iframe' && t.url.startsWith(process.argv.at(-1)+'/'));
if(!target)throw Error('No exact fixture iframe target');
const version=await(await fetch('http://127.0.0.1:39230/json/version')).json();
const browser=new CDP(version.webSocketDebuggerUrl);await browser.ready();
const before=(await browser.call('SystemInfo.getProcessInfo')).processInfo.filter(p=>p.type==='renderer').map(p=>p.id);
const frame=new CDP(target.webSocketDebuggerUrl);await frame.ready();
await browser.call('Tracing.start',{categories:'devtools.timeline',transferMode:'ReturnAsStream'});
await frame.call('Runtime.evaluate',{expression:"console.timeStamp('vosulay-fixture-crash-victim')"});
await browser.call('Tracing.end');
const traceDeadline=Date.now()+5000;
while(!browser.trace&&Date.now()<traceDeadline)await Bun.sleep(5);
if(!browser.trace?.stream)throw Error('No fixture process-identity trace');
let raw='';
while(true){const part=await browser.call('IO.read',{handle:browser.trace.stream});raw+=part.base64Encoded?Buffer.from(part.data,'base64').toString():part.data;if(part.eof)break}
await browser.call('IO.close',{handle:browser.trace.stream});
const marker=JSON.parse(raw).traceEvents.find(e=>e.name==='TimeStamp'&&e.args?.data?.message==='vosulay-fixture-crash-victim');
if(!marker||!before.includes(marker.pid))throw Error('Exact iframe marker was not in a live fixture renderer');
process.kill(marker.pid,'SIGKILL');
let disappeared=[];
const deadline=Date.now()+5000;
while(Date.now()<deadline){const after=(await browser.call('SystemInfo.getProcessInfo')).processInfo.map(p=>p.id);disappeared=before.filter(id=>!after.includes(id));if(disappeared.length)break;await Bun.sleep(50)}
frame.ws.close();browser.ws.close();
if(disappeared.length!==1)throw Error('Expected exactly one renderer process to exit, got '+JSON.stringify(disappeared));
console.log('Verified fixture renderer process exit',disappeared[0]);
"#;

/// Run only under the private-display wrapper. `shared` is the deliberately broken
/// seven-OOPIF baseline, not a production fallback or a disabled-isolation test.
pub(super) unsafe fn run(
    runtime: &Path,
    output: &Path,
    sandbox_info: *mut u8,
    fps: u32,
    shared: bool,
    security: bool,
) -> Result<()> {
    ensure!(
        !security || !shared,
        "security lifecycle requires separate sites"
    );
    if security {
        drop(
            TcpListener::bind((Ipv4Addr::LOCALHOST, 39230))
                .context("fixture DevTools port occupied")?,
        );
    }
    let canonical = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = canonical.local_addr()?.port();
    let mut listeners = vec![canonical];
    for last in 2..=8 {
        listeners.push(TcpListener::bind((Ipv4Addr::new(127, 0, 0, last), port))?);
    }
    // This listener simulates an unrelated local application. It is never authorized.
    listeners.push(TcpListener::bind((Ipv4Addr::new(127, 0, 0, 200), port))?);
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = vec![];
    for listener in listeners {
        listener.set_nonblocking(true)?;
        let stop = stop.clone();
        threads.push(thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => respond(stream),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => break,
                }
            }
        }));
    }
    let mut origins = Vec::new();
    for id in 0..7 {
        origins.push(format!(
            "http://127.0.0.{}:{port}",
            if shared { 2 } else { id + 2 }
        ));
    }
    let encoded = serde_json::to_string(&origins)?;
    let manager = format!(
        r#"<!doctype html><style>html,body{{margin:0;background:transparent;overflow:hidden}}.overlay{{position:absolute;transform-origin:top left;contain:layout style}}.overlay-clip{{position:absolute;inset:0;overflow:hidden}}iframe{{position:absolute;width:500px;height:750px;border:0;display:block}}</style><script>
const origins={encoded};
window.__VOSULAY_RECEIVE__=m=>{{const f=document.querySelector(`[name="vosulay-site-${{m.request}}"]`);if(f)f.src=m.ready?origins[+m.request]+'/?id='+m.request:origins[+m.request]+'/__vosulay_prepare'}};
addEventListener('message',e=>{{if(e.source && e.data && Number.isInteger(e.data.id))window.__VOSULAY_POST_MESSAGE__({{type:'pacing',...e.data}})}});
for(let id=0;id<7;id++){{const root=document.createElement('div');root.className='overlay';root.style.cssText=`left:${{id*230}}px;top:${{id*40}}px;width:500px;height:750px;transform:scale(.5)`;const clip=document.createElement('div');clip.className='overlay-clip';const f=document.createElement('iframe');f.name='vosulay-site-'+id;f.src=origins[id]+'/__vosulay_prepare';clip.append(f);root.append(clip);document.documentElement.append(root)}}
const foreign=document.createElement('iframe');foreign.src='http://127.0.0.200:{port}/?id=99';foreign.style.left='2000px';document.documentElement.append(foreign);
</script>"#
    );
    let cef = unsafe {
        CefRuntime::initialize(RuntimeOptions {
            runtime,
            sandbox_info,
            cache: &output.join("cache"),
            resources: Arc::new(Resources(Arc::from(manager.into_bytes()))),
            devtools_port: security.then_some(39230),
        })
    }?;
    let paints = Rc::new(RefCell::new(Vec::new()));
    let backend = format!("http://127.0.0.1:{port}/");
    let secret = "a".repeat(64);
    let browser = cef.open(
        BrowserOptions {
            url: MANAGER_URL,
            backend_url: &backend,
            broker: Some((&backend, &secret)),
            width: 2560,
            height: 1440,
            scale: 1.,
            fps_limit: fps,
            shared_texture: false,
            shared_array_buffer: false,
        },
        Box::new(Sink(paints.clone(), Instant::now())),
    )?;
    for (id, origin) in origins.iter().enumerate() {
        ensure!(
            browser.authorize_overlay_site(&id.to_string(), origin),
            "authorize site"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut results = vec![];
    let mut site_events = vec![];
    let mut console = vec![];
    let mut startups = vec![];
    let mut crash: Option<std::process::Child> = None;
    let mut lifecycle_started = false;
    let mut crashed = false;
    let mut recovery_started = false;
    let expected = if security { 10 } else { 8 };
    while Instant::now() < deadline && results.len() < expected {
        browser.pump()?;
        console.extend(browser.take_console());
        while let Some((request, ready, process)) = browser.next_overlay_site_event() {
            site_events
                .push(serde_json::json!({"request":request,"ready":ready,"process":process}));
            if security && request == "0" && !ready && crashed {
                recovery_started = true;
                browser.release_overlay_site("0");
                ensure!(
                    browser.authorize_overlay_site("recovered-0", &origins[0]),
                    "authorize replacement renderer"
                );
                browser.evaluate(&format!("{{const old=document.querySelector('[name=\"vosulay-site-0\"]');const f=old.cloneNode(false);f.removeAttribute('src');f.name='vosulay-site-recovered-0';f.src={};old.replaceWith(f);}}", serde_json::to_string(&(origins[0].clone()+"/__vosulay_prepare"))?));
            } else if request == "recovered-0" {
                ensure!(ready, "replacement renderer failed its prepare cycle");
                browser.evaluate(&format!(
                    "document.querySelector('[name=\"vosulay-site-recovered-0\"]').src={}",
                    serde_json::to_string(&(origins[0].clone() + "/?id=0&recovered=1"))?
                ));
            } else {
                let event = serde_json::json!({"request":request,"ready":ready});
                browser.evaluate(&format!("window.__VOSULAY_RECEIVE__({event})"));
            }
        }
        while let Some(message) = browser.next_message() {
            let message: serde_json::Value = serde_json::from_str(&message)?;
            if message["type"] == "pacing" {
                if message["kind"] == "startup" {
                    startups.push(message);
                } else {
                    results.push(message);
                }
            }
        }
        if security && results.len() >= 8 && !lifecycle_started {
            lifecycle_started = true;
            browser.release_overlay_site("6");
            ensure!(
                TcpListener::bind((Ipv4Addr::new(127, 0, 0, 8), port)).is_err(),
                "retired origin could be taken over"
            );
            browser.evaluate(&format!("{{const f=document.createElement('iframe');f.name='vosulay-site-retired-6';f.style.left='1900px';f.src={};document.documentElement.append(f);}}", serde_json::to_string(&(origins[6].clone()+"/?id=98"))?));
            crash = Some(
                std::process::Command::new("bun")
                    .arg("-e")
                    .arg(CRASH_RENDERER)
                    .arg(&origins[0])
                    .spawn()
                    .context("--site-security requires Bun for its isolated CDP crash probe")?,
            );
        }
        if let Some(child) = crash.as_mut()
            && let Some(status) = child.try_wait()?
        {
            ensure!(status.success(), "isolated renderer crash probe failed");
            crash = None;
            crashed = true;
            browser.evaluate(&format!(
                "document.querySelector('[name=\"vosulay-site-0\"]').src={}",
                serde_json::to_string(&(origins[0].clone() + "/?id=0&restart=1"))?
            ));
        }
        thread::park_timeout(
            browser
                .next_pump_deadline()
                .unwrap_or(deadline)
                .min(deadline)
                .saturating_duration_since(Instant::now()),
        );
    }
    let result = serde_json::json!({"fps":fps,"shared":shared,"frames":results,"paint_times":*paints.borrow(),"site_events":site_events,"console":console,"startups":startups,"security":security});
    std::fs::write(
        output.join("pacing.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    browser.close()?;
    drop(cef);
    stop.store(true, Ordering::Release);
    for thread in threads {
        thread.join().expect("fixture listener");
    }
    ensure!(
        results.len() == expected,
        "expected {expected} fixture reports, got {}",
        results.len()
    );
    if security {
        ensure!(
            crashed && recovery_started,
            "renderer crash/restart cycle did not complete"
        );
        ensure!(
            startups
                .iter()
                .any(|v| v["id"] == 0 && v["routing"] == false),
            "new renderer received routing before preparing"
        );
        let initial = site_events
            .iter()
            .find(|v| v["request"] == "0" && v["ready"] == true)
            .context("missing initial renderer identity")?;
        let replacement = site_events
            .iter()
            .find(|v| v["request"] == "recovered-0" && v["ready"] == true)
            .context("missing replacement renderer identity")?;
        ensure!(
            initial["process"] != replacement["process"],
            "renderer identity survived process crash"
        );
    }
    for report in &results {
        if report["id"] == 99 || report["id"] == 98 {
            ensure!(
                report["routing"] == false,
                "foreign or retired origin obtained native routing"
            );
            continue;
        }
        ensure!(
            report["routing"] == true,
            "approved frame did not receive its native bootstrap"
        );
        let stamps = report["stamps"].as_array().context("missing timestamps")?;
        if !shared {
            ensure!(
                stamps.windows(2).all(|w| w[0].as_f64() < w[1].as_f64()),
                "repeated rAF timestamps in one overlay"
            );
            let rate = stamps.len() as f64 * 1000.
                / report["duration"].as_f64().context("missing duration")?;
            ensure!(
                rate >= f64::from(fps) * 0.8 && rate <= f64::from(fps) * 1.1,
                "rAF rate {rate} differs from {fps}"
            );
        }
    }
    Ok(())
}
