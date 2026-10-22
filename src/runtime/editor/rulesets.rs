use super::{Task, TaskSender};
use crate::ruleset::{Ruleset, subscription_value};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::{
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::oneshot;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use url::Url;

pub(super) struct Watcher {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}
impl Watcher {
    pub fn start(mut backend: Url, sender: TaskSender) -> Result<Self> {
        let scheme = if backend.scheme() == "https" {
            "wss"
        } else {
            "ws"
        };
        let _ = backend.set_scheme(scheme);
        backend.set_path("/websocket/v2/subscribe");
        backend.set_query(None);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (stop, stopped) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("vosulay-ruleset".into())
            .spawn(move || {
                runtime.block_on(async {
                    tokio::select! { _ = stopped => {}, _ = run(backend, sender) => {} }
                })
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn join(&mut self) -> Result<()> {
        self.stop();
        if self
            .thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
        {
            anyhow::bail!("ruleset watcher panicked");
        }
        Ok(())
    }
}
impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

async fn publish(sender: &TaskSender, ruleset: Option<Ruleset>) -> bool {
    loop {
        let result = match sender.lock() {
            Ok(sender) => match sender.as_ref() {
                Some(sender) => sender.try_send(Task::Ruleset(ruleset)),
                None => return false,
            },
            Err(_) => return false,
        };
        match result {
            Ok(()) => return true,
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return false,
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                tokio::time::sleep(Duration::from_millis(25)).await
            }
        }
    }
}
async fn run(backend: Url, sender: TaskSender) {
    loop {
        let config = WebSocketConfig::default()
            .max_message_size(Some(64 * 1024))
            .max_frame_size(Some(64 * 1024));
        let connected = tokio::time::timeout(
            Duration::from_secs(2),
            connect_async_with_config(backend.as_str(), Some(config), true),
        )
        .await;
        if let Ok(Ok((mut socket, _))) = connected
            && socket
                .send(Message::Text("[\"/settings/mode\"]".into()))
                .await
                .is_ok()
        {
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Text(text) => {
                        if let Ok(frame) = serde_json::from_str(&text)
                            && let Some(ruleset) = subscription_value(&frame)
                            && !publish(&sender, ruleset).await
                        {
                            return;
                        }
                    }
                    Message::Ping(bytes) => {
                        if socket.send(Message::Pong(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
        if !publish(&sender, None).await {
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{AppConfig, Profile, save_to_path},
        runtime::ProductResources,
    };
    use serde_json::{Value, json};
    use std::{
        net::TcpListener,
        sync::{Arc, mpsc},
        time::Instant,
    };

    #[test]
    fn live_subscription_switches_bound_profiles_and_respects_manual_override() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let backend = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let (publish, frames) = mpsc::channel();
        let publisher = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tokio_tungstenite::tungstenite::accept(stream).unwrap();
            assert_eq!(
                socket.read().unwrap().into_text().unwrap(),
                "[\"/settings/mode\"]"
            );
            for frame in frames {
                if socket.send(Message::Text(frame)).is_err() {
                    break;
                }
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = AppConfig::default();
        config.profiles.extend([
            Profile {
                id: "mania".into(),
                name: "Mania".into(),
                overlays: vec![],
                ruleset: Some(Ruleset::Mania),
            },
            Profile {
                id: "taiko".into(),
                name: "Taiko".into(),
                overlays: vec![],
                ruleset: Some(Ruleset::Taiko),
            },
        ]);
        save_to_path(&config, &path).unwrap();
        let resources = Arc::new(ProductResources::new(&config).unwrap());
        let mut worker =
            super::super::EditorWorker::start(config, path.clone(), backend, resources).unwrap();
        let editor = worker.manage_editor();
        let wait = |ruleset: Value, active: &str| {
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                let state = editor.state().unwrap();
                if state["ruleset"] == ruleset && state["config"]["active_profile"] == active {
                    return state;
                }
                assert!(Instant::now() < until, "{state}");
                thread::sleep(Duration::from_millis(10));
            }
        };
        publish
            .send(
                json!([0, 1, [[0, {"number":3,"name":"mania"}]]])
                    .to_string()
                    .into(),
            )
            .unwrap();
        let state = wait(json!("mania"), "mania");
        let mut config = state["config"].clone();
        config["active_profile"] = json!("default");
        editor.message(json!({"type":"save-config","save_id":"manual","config":config,"base_revision":state["revision"]})).unwrap();
        publish
            .send(json!([1, 2, [[0, {"number":3}]]]).to_string().into())
            .unwrap();
        publish
            .send(json!([1, 3, [], [0]]).to_string().into())
            .unwrap();
        wait(Value::Null, "default");
        publish
            .send(json!([0, 4, [[0, {"number":3}]]]).to_string().into())
            .unwrap();
        wait(json!("mania"), "default");
        publish
            .send(json!([1, 5, [[0, {"number":1}]]]).to_string().into())
            .unwrap();
        wait(json!("taiko"), "taiko");
        assert_eq!(
            crate::config::load_from_path(&path).unwrap().active_profile,
            "taiko"
        );
        worker.stop();
        drop(publish);
        publisher.join().unwrap();
        while !worker.finished() {
            while worker.next_reply().is_some() {}
            thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap();
    }
}
