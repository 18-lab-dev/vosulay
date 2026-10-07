use super::*;
use serde_json::json;
use std::{
    io,
    net::{Shutdown, TcpListener},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};
use tokio_tungstenite::tungstenite::{self, Message, accept, client};

type Publication = Arc<Mutex<(u64, Option<Utf8Bytes>)>>;

struct Publisher {
    url: Url,
    connections: Arc<AtomicUsize>,
    publication: Publication,
    peers: Arc<Mutex<Vec<TcpStream>>>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let (count, published, connected, stopped) = (
            connections.clone(),
            publication.clone(),
            peers.clone(),
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
                let (count, published, stopped) =
                    (count.clone(), published.clone(), stopped.clone());
                workers.push(thread::spawn(move || {
                    serve_upstream(stream, count, published, stopped)
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

fn serve_upstream(
    stream: TcpStream,
    count: Arc<AtomicUsize>,
    publication: Publication,
    stop: Arc<AtomicBool>,
) {
    let Ok(mut socket) = accept(stream) else {
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
fn selected_subscription_emits_initial_delta_removal_and_rejects_overlap() {
    let publisher = Publisher::start();
    let broker = Broker::new(publisher.url.clone()).unwrap();
    let mut client = downstream(&broker, "/websocket/v2/subscribe");
    let mut full = downstream(&broker, "/websocket/v2");
    publisher.wait_connections(1);
    client
        .send(Message::Text(r#"["/beatmap/title","/state"]"#.into()))
        .unwrap();
    barrier(&mut client);
    publisher.publish(r#"{"beatmap":{"title":"Song"},"state":1}"#);
    let initial: Value = serde_json::from_str(&text(&mut client)).unwrap();
    assert_eq!(initial[0], 0);
    assert_eq!(initial[2], json!([[0, "Song"], [1, 1]]));
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut full)).unwrap()["state"],
        1
    );
    assert_eq!(publisher.connections.load(Ordering::Acquire), 1);

    publisher.publish(r#"{"beatmap":{"title":"Song"},"state":2}"#);
    let delta: Value = serde_json::from_str(&text(&mut client)).unwrap();
    assert_eq!(delta[0], 1);
    assert_eq!(delta[2], json!([[1, 2]]));
    assert_eq!(delta[3], json!([]));
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut full)).unwrap()["state"],
        2
    );

    publisher.publish(r#"{"state":3}"#);
    let removal: Value = serde_json::from_str(&text(&mut client)).unwrap();
    assert_eq!(removal[0], 1);
    assert_eq!(removal[2], json!([[1, 3]]));
    assert_eq!(removal[3], json!([0]));
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut full)).unwrap()["state"],
        3
    );

    let mut overlapping = downstream(&broker, "/websocket/v2/subscribe");
    overlapping
        .send(Message::Text(r#"["/beatmap","/beatmap/title"]"#.into()))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text(&mut overlapping)).unwrap(),
        json!([2, 2, "invalid JSON Pointer"])
    );
    match overlapping.read().unwrap() {
        Message::Close(Some(close)) => assert_eq!(u16::from(close.code), 1008),
        other => panic!("expected policy close, got {other:?}"),
    }
}

#[test]
fn legacy_nested_filters_do_not_erase_a_prior_whole_field_null() {
    let filters: Arc<[Filter]> = parse_filters(
        r#"["beatmap",{"field":"beatmap","keys":["title"]},{"field":"missing","keys":["title"]}]"#,
    )
    .unwrap()
    .into();
    let snapshot = Snapshot {
        revision: 1,
        text: r#"{"beatmap":null}"#.into(),
        document: OnceLock::new(),
        projections: Mutex::new(HashMap::new()),
    };
    let filtered: Value = serde_json::from_str(&snapshot.filtered(&filters).unwrap()).unwrap();
    assert_eq!(filtered, json!({"beatmap":null}));
}
