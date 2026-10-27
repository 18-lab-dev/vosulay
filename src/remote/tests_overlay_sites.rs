use super::*;

fn server() -> RemoteServer {
    RemoteServer::start_with_listener(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        Url::parse("http://127.0.0.1:24050/").unwrap(),
        Arc::new(|| Ok(AppConfig::default())),
    )
    .unwrap()
}
fn address(url: &Url) -> SocketAddr {
    url.socket_addrs(|| None).unwrap()[0]
}
fn get(address: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {address}\r\nSec-Fetch-Dest: iframe\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Site: cross-site\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn source_sites_are_stable_and_share_queries_but_not_folders() {
    let mut server = server();
    let first = server
        .overlay_site(&Url::parse("http://127.0.0.1:24050/first/?vosuProfile=one").unwrap())
        .unwrap();
    let repeated = server
        .overlay_site(&Url::parse("http://127.0.0.1:24050/first/?vosuProfile=two").unwrap())
        .unwrap();
    let second = server
        .overlay_site(&Url::parse("http://127.0.0.1:24050/second/").unwrap())
        .unwrap();
    assert_eq!(first.origin(), repeated.origin());
    assert_ne!(first.origin(), second.origin());
    assert_eq!(first.port(), server.browser_url.port());
    assert_eq!(first.query(), Some("vosuProfile=one"));
    assert_eq!(second.path(), "/second/");
    assert_eq!(server.sites.len(), 2);
    let external = Url::parse("https://example.org/counter/").unwrap();
    assert_eq!(server.overlay_site(&external).unwrap(), external);
    assert_eq!(server.sites.len(), 2);
}

#[test]
fn a_foreign_bind_is_never_advertised_or_approved_as_a_site() {
    let mut server = server();
    let foreign_address = SocketAddr::from((
        Ipv4Addr::new(127, 0, 0, 2),
        server.browser_url.port().unwrap(),
    ));
    let _foreign = TcpListener::bind(foreign_address).unwrap();
    let site = server
        .overlay_site(&Url::parse("http://127.0.0.1:24050/counter/").unwrap())
        .unwrap();
    assert_ne!(address(&site), foreign_address);
    assert_eq!(site.host_str(), Some("127.0.0.3"));
    assert!(
        server
            .sites
            .values()
            .all(|url| address(url) != foreign_address)
    );
}

#[test]
fn retired_sites_cannot_be_taken_over_before_browser_shutdown() {
    let mut server = server();
    let source = Url::parse("http://127.0.0.1:24050/counter/").unwrap();
    let site = server.overlay_site(&source).unwrap();
    let site_address = address(&site);
    // Source/frame grants are revoked by the browser independently. The server
    // intentionally retains this socket even if no placement uses the source.
    assert!(TcpListener::bind(site_address).is_err());
    assert_eq!(server.overlay_site(&source).unwrap(), site);
    drop(server);
    assert!(TcpListener::bind(site_address).is_ok());
}

#[test]
fn prepare_document_exists_only_on_owned_site_listeners() {
    let mut server = server();
    let site = server
        .overlay_site(&Url::parse("http://127.0.0.1:24050/counter/").unwrap())
        .unwrap();
    let owned = get(address(&site), "/__vosulay_prepare");
    assert!(owned.starts_with("HTTP/1.1 200"));
    assert!(owned.contains("default-src 'none'"));
    assert!(!owned.contains("<script"));
    let canonical = get(address(&server.browser_url), "/__vosulay_prepare");
    assert!(canonical.starts_with("HTTP/1.1 404"));
}

#[test]
fn site_listener_cap_falls_back_to_the_existing_shared_broker() {
    let mut server = server();
    for index in 0..MAX_OVERLAY_SITES {
        let source = Url::parse(&format!("http://127.0.0.1:24050/counter-{index}/")).unwrap();
        assert_ne!(
            server.overlay_site(&source).unwrap().origin(),
            server.browser_url.origin()
        );
    }
    let overflow = Url::parse("http://127.0.0.1:24050/overflow/").unwrap();
    assert_eq!(
        server.overlay_site(&overflow).unwrap().origin(),
        server.browser_url.origin()
    );
    assert_eq!(server.threads.len(), usize::from(MAX_OVERLAY_SITES) + 1);
}
