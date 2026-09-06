//! Continuously drain bounded browser events while policy replies run independently.
use super::*;
use futures::StreamExt;

pub(super) async fn run(
    mut reader: futures::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    sender: Cdp,
) {
    let mut controls = tokio::task::JoinSet::new();
    let mut events = 0u32;
    loop {
        let message = tokio::select! {
            biased;
            () = sender.shared.cancelled.cancelled() => break,
            Some(result) = controls.join_next(), if !controls.is_empty() => {
                if !matches!(result, Ok(Ok(()))) { sender.fail(BrowserError::Protocol); break; }
                continue;
            }
            message = reader.next() => message,
        };
        let text = match message {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                events += 1;
                if events > 20_000 {
                    sender.fail(BrowserError::Limit);
                    break;
                }
                continue;
            }
            _ => {
                sender.fail(BrowserError::Protocol);
                break;
            }
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            sender.fail(BrowserError::Protocol);
            break;
        };
        if let Some(id) = value["id"].as_u64() {
            #[cfg(all(test, windows))]
            {
                let mut state = sender.shared.state.lock().expect("browser transport");
                if state.suppressed_reply == Some(id) {
                    state.suppressed_reply = None;
                    continue;
                }
            }
            let reply = sender
                .shared
                .state
                .lock()
                .expect("browser transport")
                .pending
                .remove(&id);
            let Some(reply) = reply else {
                sender.fail(BrowserError::Protocol);
                break;
            };
            let result = if value.get("error").is_some() {
                Err(BrowserError::Protocol)
            } else {
                Ok(value["result"].clone())
            };
            let _ = reply.send(result);
            continue;
        }
        events += 1;
        if events > 20_000 {
            sender.fail(BrowserError::Limit);
            break;
        }
        match value["method"].as_str() {
            Some("Target.attachedToTarget") => {
                let session = value["params"]["sessionId"].as_str().unwrap_or("");
                let target = value["params"]["targetInfo"]["targetId"]
                    .as_str()
                    .unwrap_or("");
                let kind = value["params"]["targetInfo"]["type"].as_str().unwrap_or("");
                if session.is_empty()
                    || session.len() > 128
                    || target.is_empty()
                    || target.len() > 128
                    || controls.len() >= MAX_INFLIGHT
                {
                    sender.fail(BrowserError::Limit);
                    break;
                }
                let mut state = sender.shared.state.lock().expect("browser transport");
                if state.sessions.len() >= MAX_TARGETS {
                    drop(state);
                    sender.fail(BrowserError::Limit);
                    break;
                }
                state.sessions.insert(target.to_owned(), session.to_owned());
                state.epochs.insert(session.to_owned(), 0);
                drop(state);
                let (session, target, kind) =
                    (session.to_owned(), target.to_owned(), kind.to_owned());
                let control = sender.clone();
                controls.spawn(async move {
                    if kind == "page" || kind == "iframe" {
                        control.configure(&session).await
                    } else {
                        control
                            .call("Target.closeTarget", json!({"targetId":target}), None)
                            .await
                            .map(|_| ())
                    }
                });
            }
            Some(
                "Page.frameNavigated" | "DOM.documentUpdated" | "Page.navigatedWithinDocument",
            ) => {
                // A blocked/late child frame is not replacement of the owned
                // main document. Element fingerprints still bind actual inputs.
                if value["method"] == "Page.frameNavigated"
                    && value["params"]["frame"]["parentId"].is_string()
                {
                    continue;
                }
                if let Some(session) = value["sessionId"].as_str() {
                    if let Some(epoch) = sender
                        .shared
                        .state
                        .lock()
                        .expect("browser transport")
                        .epochs
                        .get_mut(session)
                    {
                        *epoch = epoch.saturating_add(1);
                    }
                }
            }
            Some("Page.lifecycleEvent")
                if matches!(
                    value["params"]["name"].as_str(),
                    Some("DOMContentLoaded" | "load")
                ) =>
            {
                if let (Some(session), Some(frame), Some(loader)) = (
                    value["sessionId"].as_str(),
                    value["params"]["frameId"].as_str(),
                    value["params"]["loaderId"].as_str(),
                ) {
                    let mut state = sender.shared.state.lock().expect("browser transport");
                    if frame.len() > 128 || loader.len() > 128 || state.ready.len() >= 64 {
                        drop(state);
                        sender.fail(BrowserError::Limit);
                        break;
                    }
                    if state.epochs.contains_key(session) {
                        state
                            .ready
                            .insert((session.to_owned(), frame.to_owned()), loader.to_owned());
                    }
                }
            }
            Some("Fetch.requestPaused") => {
                let session = value["sessionId"].as_str().unwrap_or("").to_owned();
                let request = value["params"]["requestId"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                if session.is_empty()
                    || request.is_empty()
                    || session.len() > 128
                    || request.len() > 128
                    || controls.len() >= MAX_INFLIGHT
                {
                    sender.fail(BrowserError::Limit);
                    break;
                }
                let permitted = sender
                    .shared
                    .policy
                    .permits(value["params"]["request"]["url"].as_str().unwrap_or(""));
                let control = sender.clone();
                controls.spawn(async move {
                    control
                        .call(
                            if permitted {
                                "Fetch.continueRequest"
                            } else {
                                "Fetch.failRequest"
                            },
                            if permitted {
                                json!({"requestId":request})
                            } else {
                                json!({"requestId":request,"errorReason":"BlockedByClient"})
                            },
                            Some(&session),
                        )
                        .await
                        .map(|_| ())
                });
            }
            Some("Fetch.authRequired") => {
                if controls.len() >= MAX_INFLIGHT {
                    sender.fail(BrowserError::Limit);
                    break;
                }
                let session = value["sessionId"].as_str().unwrap_or("").to_owned();
                let request = value["params"]["requestId"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                if session.is_empty()
                    || request.is_empty()
                    || session.len() > 128
                    || request.len() > 128
                {
                    sender.fail(BrowserError::Protocol);
                    break;
                }
                let control = sender.clone();
                controls.spawn(async move { control.call("Fetch.continueWithAuth", json!({"requestId":request,"authChallengeResponse":{"response":"CancelAuth"}}), Some(&session)).await.map(|_| ()) });
            }
            Some("Page.fileChooserOpened") => {
                // Interception prevents the OS picker opening. Supplying
                // an empty list is the only file-input operation allowed.
                let session = value["sessionId"].as_str().unwrap_or("").to_owned();
                let backend = value["params"]["backendNodeId"].as_i64().unwrap_or(0);
                if session.is_empty()
                    || session.len() > 128
                    || backend <= 0
                    || controls.len() >= MAX_INFLIGHT
                {
                    sender.fail(BrowserError::Protocol);
                    break;
                }
                let control = sender.clone();
                controls.spawn(async move {
                    control
                        .call(
                            "DOM.setFileInputFiles",
                            json!({"backendNodeId":backend,"files":[]}),
                            Some(&session),
                        )
                        .await
                        .map(|_| ())
                });
            }
            _ => {}
        }
        // Processing a hostile event flood is bounded in total and yields
        // to other host tasks, not a permanently ready reader loop.
        if events.is_multiple_of(64) {
            tokio::task::yield_now().await;
        }
    }
    controls.abort_all();
    while controls.join_next().await.is_some() {}
    sender.fail(BrowserError::Cancelled);
}
