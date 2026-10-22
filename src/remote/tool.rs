//! Authenticated vosu-facing adapter to the existing editor authority.
use super::*;

pub struct ToolServer {
    stop: Arc<AtomicBool>,
    address: SocketAddr,
    thread: Option<JoinHandle<()>>,
}
impl ToolServer {
    pub fn start(
        env: &crate::tool::Environment,
        view: &Url,
        config: PathBuf,
        editor: Arc<dyn ManageEditor>,
    ) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let token = env.token.clone();
        let upstream = env.backend.clone();
        let view = view.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::Builder::new()
            .name("vosulay-tool-http".into())
            .spawn(move || {
                for incoming in listener.incoming() {
                    if stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(mut stream) = incoming else {
                        break;
                    };
                    let result = (|| -> Result<()> {
                        stream.set_read_timeout(Some(TIMEOUT))?;
                        stream.set_write_timeout(Some(TIMEOUT))?;
                        let mut head = false;
                        let response = match read_request(&mut stream) {
                            Err(error) => {
                                Response::plain(error.status, error.reason, &error.message)
                            }
                            Ok(request) => {
                                head = request.method == "HEAD";
                                match respond(
                                    &request,
                                    &token,
                                    &view,
                                    &upstream,
                                    &config,
                                    editor.as_ref(),
                                ) {
                                    Ok(response) => response,
                                    Err(error) => Response::plain(
                                        500,
                                        "Internal Server Error",
                                        &format!("{error:#}\n"),
                                    ),
                                }
                            }
                        };
                        response.write(&mut stream, head)?;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        eprintln!("vosulay tool HTTP: {error:#}");
                    }
                }
            })?;
        let server = Self {
            stop,
            address,
            thread: Some(thread),
        };
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "vosu-tool-ready {}", address.port())?;
        stdout.flush()?;
        Ok(server)
    }
}
impl Drop for ToolServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, TIMEOUT);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn respond(
    request: &Request,
    token: &str,
    view: &Url,
    upstream: &Url,
    config: &Path,
    editor: &dyn ManageEditor,
) -> Result<Response> {
    let mut tokens = request
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("x-vosu-tool-token"));
    let authorized = tokens
        .next()
        .is_some_and(|(_, value)| token_matches(value.as_bytes(), token.as_bytes()))
        && tokens.next().is_none();
    if !authorized {
        return Ok(Response::plain(
            401,
            "Unauthorized",
            "Tool token required\n",
        ));
    }
    let path = request.target.split('?').next().unwrap_or_default();
    let read = matches!(request.method.as_str(), "GET" | "HEAD");
    if read && path == "/healthz" {
        return Ok(Response::plain(200, "OK", "ok\n"));
    }
    if read && path == "/v1/manage/document" {
        return manage::page(view, upstream).map_err(|error| anyhow::anyhow!("{}", error.message));
    }
    if read && path == "/v1/view/state" {
        return Ok(view_state(&crate::config::load_from_path(config)?));
    }
    if read && path == "/v1/view" {
        return overlay_response(
            view,
            upstream,
            &crate::config::load_from_path(config)?,
            &request.target,
        )
        .map_err(|error| anyhow::anyhow!("{}", error.message));
    }
    let value = match (request.method.as_str(), path) {
        ("GET" | "HEAD", "/v1/manage/state") => editor.state()?,
        ("GET" | "HEAD", "/v1/manage/counters") => editor.counters(),
        ("POST", "/v1/manage/message") => {
            let message = match serde_json::from_slice(&request.body) {
                Ok(message) => message,
                Err(_) => return Ok(Response::plain(400, "Bad Request", "Invalid JSON\n")),
            };
            editor.message(message)?
        }
        _ => return Ok(Response::plain(404, "Not Found", "Route not found\n")),
    };
    let mut response = Response::bytes(
        200,
        "OK",
        "application/json",
        value.to_string().into_bytes(),
    );
    response
        .headers
        .push(("Cache-Control".into(), "no-store".into()));
    Ok(response)
}
fn token_matches(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0, |difference, (a, b)| difference | (a ^ b))
        == 0
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_is_exact() {
        assert!(token_matches(b"secret", b"secret"));
        assert!(!token_matches(b"Secret", b"secret"));
        assert!(!token_matches(b"", b"secret"));
    }
}
