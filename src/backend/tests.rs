use super::*;
use serde_json::json;
use std::{
    io,
    net::{Shutdown, TcpListener},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};
use tokio_tungstenite::tungstenite::{
    self, Message, accept, accept_hdr, client,
    handshake::server::{Request, Response},
};

type Publication = Arc<Mutex<(u64, Option<Utf8Bytes>)>>;
type Paths = Arc<Mutex<Vec<String>>>;

struct Publisher {
    url: Url,
    connections: Arc<AtomicUsize>,
    publication: Publication,
    peers: Arc<Mutex<Vec<TcpStream>>>,
    paths: Paths,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Publisher {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let publication = Arc::new(Mutex::new((0, None)));
        let peers = Arc::new(Mutex::new(Vec::new()));
        let paths = Paths::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (count, published, connected, requested, stopped) = (
            connections.clone(),
            publication.clone(),
            peers.clone(),
            paths.clone(),
            stop.clone(),
        );
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !stopped.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("upstream accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                connected.lock().unwrap().push(stream.try_clone().unwrap());
                let (count, published, requested, stopped) = (
                    count.clone(),
                    published.clone(),
                    requested.clone(),
                    stopped.clone(),
                );
                workers.push(thread::spawn(move || {
                    serve_upstream(stream, count, published, requested, stopped)
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            connections,
            publication,
            peers,
            paths,
            stop,
            thread: Some(thread),
        }
    }

    fn publish(&self, json: &str) {
        let mut publication = self.publication.lock().unwrap();
        publication.0 += 1;
        publication.1 = Some(json.to_owned().into());
    }

    fn disconnect(&self) {
        self.publication.lock().unwrap().1 = None;
        for peer in self.peers.lock().unwrap().drain(..) {
            let _ = peer.shutdown(Shutdown::Both);
        }
    }

    fn wait_connections(&self, wanted: usize) {
        let until = Instant::now() + Duration::from_secs(3);
        while self.connections.load(Ordering::Acquire) < wanted {
            assert!(
                Instant::now() < until,
                "upstream connection was not established"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.disconnect();
        self.thread.take().unwrap().join().unwrap();
    }
}

// tungstenite's handshake callback signature fixes the large `ErrorResponse` variant.
#[allow(clippy::result_large_err)]
fn serve_upstream(
    stream: TcpStream,
    count: Arc<AtomicUsize>,
    publication: Publication,
    paths: Paths,
    stop: Arc<AtomicBool>,
) {
    let record = |request: &Request, response: Response| {
        paths.lock().unwrap().push(request.uri().to_string());
        Ok(response)
    };
    let Ok(mut socket) = accept_hdr(stream, record) else {
        return;
    };
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    count.fetch_add(1, Ordering::Release);
    let mut revision = 0;
    while !stop.load(Ordering::Acquire) {
        let (latest, text) = publication.lock().unwrap().clone();
        if latest != revision {
            revision = latest;
            if let Some(text) = text
                && socket.send(Message::Text(text)).is_err()
            {
                return;
            }
        }
        match socket.read() {
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => return,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
            Ok(_) => {}
        }
    }
}

fn downstream(broker: &Broker, target: &str) -> tungstenite::WebSocket<TcpStream> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("ws://{address}{target}");
    let client_thread = thread::spawn(move || {
        let stream = TcpStream::connect(address).unwrap();
        client(url, stream).unwrap().0
    });
    let (stream, _) = listener.accept().unwrap();
    // Complete the real handshake without consuming any downstream data frames.
    drop(accept(stream.try_clone().unwrap()).unwrap());
    broker.attach(stream, Vec::new(), target).unwrap();
    let mut socket = client_thread.join().unwrap();
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket
}

fn text(socket: &mut tungstenite::WebSocket<TcpStream>) -> String {
    match socket.read().unwrap() {
        Message::Text(text) => text.to_string(),
        other => panic!("expected text frame, got {other:?}"),
    }
}

fn barrier(socket: &mut tungstenite::WebSocket<TcpStream>) {
    socket
        .send(Message::Ping(b"applied".as_slice().into()))
        .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < until, "command barrier timed out");
        match socket.read().unwrap() {
            Message::Pong(payload) if payload.as_ref() == b"applied" => return,
            Message::Text(_) => {} // A cached frame is optional; it is not the assertion target.
            other => panic!("unexpected barrier frame: {other:?}"),
        }
    }
}

fn wait_for_snapshot(broker: &Broker, target: &str, expected: Option<&str>) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let current = broker.snapshot(target);
        if current.as_ref().map(|(_, value)| value.as_str()) == expected {
            return;
        }
        assert!(
            Instant::now() < until,
            "snapshot did not update to {expected:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn telemetry_clients_share_upstream_and_keep_filter_state_local() {
    let publisher = Publisher::start();
    let broker = Broker::new(publisher.url.clone()).unwrap();
    let mut a = downstream(&broker, "/websocket/v2");
    let mut b = downstream(&broker, "/websocket/v2");
    publisher.wait_connections(1);
    a.send(Message::Text(
        r#"applyFilters:[{"field":"beatmap","keys":["title"]}]"#.into(),
    ))
    .unwrap();
    barrier(&mut a);
    publisher.publish(r#"{"beatmap":{"title":"Song","artist":"Artist"},"state":7}"#);
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut a)).unwrap(),
        json!({"beatmap":{"title":"Song"}})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut b)).unwrap(),
        json!({"beatmap":{"title":"Song","artist":"Artist"},"state":7})
    );
    assert_eq!(publisher.connections.load(Ordering::Acquire), 1);

    a.send(Message::Text("applyFilters:[]".into())).unwrap();
    barrier(&mut a);
    publisher.publish(r#"{"beatmap":{"title":"Next"},"state":8}"#);
    let expected = json!({"beatmap":{"title":"Next"},"state":8});
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut a)).unwrap(),
        expected
    );
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut b)).unwrap(),
        expected
    );
}

#[test]
fn sc_raw_array_filters_are_client_local_and_empty_array_resets() {
    let publisher = Publisher::start();
    let broker = Broker::new(publisher.url.clone()).unwrap();
    let mut a = downstream(&broker, "/tokens");
    let mut b = downstream(&broker, "/tokens");
    publisher.wait_connections(1);
    a.send(Message::Text(r#"["foo"]"#.into())).unwrap();
    barrier(&mut a);
    publisher.publish(r#"{"foo":1,"bar":2}"#);
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut a)).unwrap(),
        json!({"foo":1})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut b)).unwrap(),
        json!({"foo":1,"bar":2})
    );
    assert_eq!(publisher.connections.load(Ordering::Acquire), 1);
    a.send(Message::Text("[]".into())).unwrap();
    barrier(&mut a);
    publisher.publish(r#"{"foo":3,"bar":4}"#);
    let expected = json!({"foo":3,"bar":4});
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut a)).unwrap(),
        expected
    );
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut b)).unwrap(),
        expected
    );
}

#[test]
fn disconnect_clears_snapshot_and_later_publication_recovers() {
    let publisher = Publisher::start();
    let broker = Broker::new(publisher.url.clone()).unwrap();
    let mut client = downstream(&broker, "/websocket/v2");
    publisher.wait_connections(1);
    publisher.publish(r#"{"state":1}"#);
    assert_eq!(text(&mut client), r#"{"state":1}"#);
    wait_for_snapshot(&broker, "/json/v2", Some(r#"{"state":1}"#));
    publisher.disconnect();
    wait_for_snapshot(&broker, "/json/v2", None);
    publisher.wait_connections(2);
    publisher.publish(r#"{"state":2}"#);
    assert_eq!(text(&mut client), r#"{"state":2}"#);
    wait_for_snapshot(&broker, "/json/v2", Some(r#"{"state":2}"#));
}

#[test]
fn selected_subscription_relays_vosu_precise_stream_verbatim() {
    let publisher = Publisher::start();
    let broker = Broker::new(publisher.url.clone()).unwrap();
    let mut client = downstream(&broker, "/websocket/v2/subscribe?l=overlay");
    publisher.wait_connections(1);
    assert_eq!(
        *publisher.paths.lock().unwrap(),
        ["/websocket/v2/subscribe?l=overlay"]
    );
    // The upstream frame is forwarded as-is; a local projection of the regular v2 stream would
    // have withheld it until a pointer list arrived and then re-encoded it.
    publisher.publish(r#"[1,7,[[0,12.5]],[]]"#);
    assert_eq!(text(&mut client), r#"[1,7,[[0,12.5]],[]]"#);
}

#[test]
fn legacy_nested_filters_do_not_erase_a_prior_whole_field_null() {
    let filters: Arc<[Filter]> = parse_filters(
        r#"["beatmap",{"field":"beatmap","keys":["title"]},{"field":"missing","keys":["title"]}]"#,
    )
    .unwrap()
    .into();
    let snapshot = Snapshot {
        text: r#"{"beatmap":null}"#.into(),
        document: OnceLock::new(),
        projections: Mutex::new(HashMap::new()),
    };
    let filtered: Value = serde_json::from_str(&snapshot.filtered(&filters).unwrap()).unwrap();
    assert_eq!(filtered, json!({"beatmap":null}));
}

#[test]
fn whole_field_filters_keep_backend_key_order() {
    // Counters that iterate object keys render rows in backend order (tosu emits
    // `beatmap.stats` as stars, ar, cs, ...), so the projection must not sort them.
    let filters: Arc<[Filter]> = parse_filters(r#"[{"field":"beatmap","keys":["stats"]}]"#)
        .unwrap()
        .into();
    let snapshot = Snapshot {
        text: r#"{"beatmap":{"title":"Song","stats":{"stars":5.1,"ar":9,"cs":4,"od":8}}}"#.into(),
        document: OnceLock::new(),
        projections: Mutex::new(HashMap::new()),
    };
    assert_eq!(
        snapshot.filtered(&filters).unwrap().as_str(),
        r#"{"beatmap":{"stats":{"stars":5.1,"ar":9,"cs":4,"od":8}}}"#
    );
}
