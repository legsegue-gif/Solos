//! Whole turns through the real engine, store and host shell, with a
//! scripted model.

use async_trait::async_trait;
use futures::StreamExt;
use solos_api::*;
use solos_core::providers::{Capture, ChatRequest, EventStream, FinishReason, Provider, StreamEvent};
use solos_core::sandbox::host::HostSandbox;
use solos_core::tools::Registry;
use solos_core::{Engine, EngineConfig, SecretResolver};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// One scripted reply: events, then optionally an error instead of an end.
#[derive(Clone)]
struct Reply {
    events: Vec<StreamEvent>,
    error: Option<CoreError>,
    /// Wait this long before each event, so tests can act mid-stream.
    delay_ms: u64,
}

fn text(t: &str) -> Reply {
    Reply { events: vec![StreamEvent::Text(t.into()), StreamEvent::Finish(FinishReason::Stop)], error: None, delay_ms: 0 }
}

fn call(id: &str, args: &str) -> Reply {
    Reply {
        events: vec![
            StreamEvent::ToolCallStart { index: 0, id: id.into(), name: "shell".into() },
            StreamEvent::ToolCallArgs { index: 0, fragment: args.into() },
            StreamEvent::Finish(FinishReason::ToolCalls),
        ],
        error: None,
        delay_ms: 0,
    }
}

/// Answers turn requests from its script, and title requests (the ones
/// without tools) from `titles`, falling back to a fixed title.
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<ChatRequest>>,
    titles: Mutex<VecDeque<Reply>>,
    title_requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl Provider for Scripted {
    async fn stream(&self, req: ChatRequest, _c: CancellationToken) -> Result<EventStream, CoreError> {
        let reply = if req.tools.is_empty() {
            self.title_requests.lock().unwrap().push(req);
            self.titles.lock().unwrap().pop_front().unwrap_or_else(|| text("Scripted title"))
        } else {
            self.requests.lock().unwrap().push(req);
            self.replies.lock().unwrap().pop_front().expect("the script ran out")
        };
        let delay = reply.delay_ms;
        let mut items: Vec<Result<StreamEvent, CoreError>> = reply.events.into_iter().map(Ok).collect();
        if let Some(e) = reply.error {
            items.push(Err(e));
        }
        Ok(futures::stream::iter(items)
            .then(move |i| async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                i
            })
            .boxed())
    }
}

struct Key(Option<&'static str>);
impl SecretResolver for Key {
    fn secret(&self, _r: &str) -> Option<String> {
        self.0.map(str::to_string)
    }
}

async fn engine(script: Vec<Reply>, key: Option<&'static str>) -> (Engine, Arc<Scripted>) {
    let dir = std::env::temp_dir().join(format!("solos-turns-{}", uuid::Uuid::new_v4()));
    let scripted = Arc::new(Scripted {
        replies: Mutex::new(script.into()),
        requests: Mutex::new(vec![]),
        titles: Mutex::new(VecDeque::new()),
        title_requests: Mutex::new(vec![]),
    });
    let p = scripted.clone();
    let engine = Engine::open(EngineConfig {
        data_dir: dir.clone(),
        sandbox: Arc::new(HostSandbox::new(dir.join("guest"))),
        tools: Registry::builtin(),
        secrets: Arc::new(Key(key)),
        capture_dir: None,
        provider_factory: Some(Arc::new(move |_ep: &Endpoint, _k: String, _c: &Capture| Ok(p.clone() as Arc<dyn Provider>))),
    })
    .await
    .unwrap();
    engine
        .set_settings(Settings {
            endpoints: vec![Endpoint {
                id: "e".into(),
                name: "Test".into(),
                protocol: Protocol::OpenAi,
                base_url: String::new(),
                secret_ref: "k".into(),
            }],
            default_model: Some(ModelChoice { endpoint_id: "e".into(), model: "m".into() }),
            thinking: false,
        })
        .await
        .unwrap();
    (engine, scripted)
}

/// Collect a session's events until `n` turns have finished.
async fn until_turns_finish(rx: &mut tokio::sync::broadcast::Receiver<Event>, n: usize) -> Vec<Event> {
    let mut out = Vec::new();
    let mut done = 0;
    while done < n {
        let ev = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await.expect("timed out").unwrap();
        if matches!(ev.kind, EventKind::TurnFinished { .. }) {
            done += 1;
        }
        out.push(ev);
    }
    out
}

fn outcomes(events: &[Event]) -> Vec<TurnOutcome> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::TurnFinished { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_tool_round_trip_is_stored_and_the_events_are_numbered_without_gaps() {
    let (engine, scripted) = engine(vec![call("c1", r#"{"title":"say hi","command":"echo hi"}"#), text("done")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "run it".into()).await.unwrap();
    let events = until_turns_finish(&mut rx, 1).await;

    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>(), "gapless");
    assert_eq!(outcomes(&events), vec![TurnOutcome::Completed]);

    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    assert_eq!(snap.seq, *seqs.last().unwrap(), "the snapshot reflects every event");
    assert!(snap.turn.is_none());
    let roles: Vec<Role> = snap.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::User, Role::Assistant, Role::Tool, Role::Assistant]);
    match &snap.messages[1].parts[0] {
        Part::ToolCall { title, .. } => assert_eq!(title.as_deref(), Some("say hi")),
        p => panic!("{p:?}"),
    }
    match &snap.messages[2].parts[0] {
        Part::ToolResult { output, is_error, .. } => {
            assert!(output.starts_with("hi\n[exit code 0]"), "{output}");
            assert!(!is_error);
        }
        p => panic!("{p:?}"),
    }
    // The second request carried the call and its result back.
    assert_eq!(scripted.requests.lock().unwrap()[1].messages.len(), 3);
}

#[tokio::test]
async fn arguments_that_are_not_json_are_reported_to_the_model_as_they_arrived() {
    let (engine, scripted) = engine(vec![call("c1", r#"{"command": "ls"#), text("ok")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "go".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    let req = &scripted.requests.lock().unwrap()[1];
    match &req.messages[2].parts[0] {
        Part::ToolResult { output, is_error, .. } => {
            assert!(*is_error);
            assert!(output.contains(r#"{"command": "ls"#), "{output}");
        }
        p => panic!("{p:?}"),
    }
}

#[tokio::test]
async fn input_queued_during_a_failed_turn_still_runs() {
    let failing = Reply {
        events: vec![StreamEvent::Text("partial".into())],
        error: Some(CoreError::Protocol { detail: "boom".into() }),
        delay_ms: 150,
    };
    let (engine, _) = engine(vec![failing, text("second answer")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "first".into()).await.unwrap();
    engine.send(s.id.clone(), "second".into()).await.unwrap();
    let events = until_turns_finish(&mut rx, 2).await;
    let got = outcomes(&events);
    assert!(matches!(got[0], TurnOutcome::Failed { .. }), "{got:?}");
    assert_eq!(got[1], TurnOutcome::Completed);
    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    assert!(snap.queued.is_empty());
    let texts: Vec<String> = snap.messages.iter().map(|m| m.text()).collect();
    assert!(texts.contains(&"second".to_string()) && texts.contains(&"second answer".to_string()), "{texts:?}");
}

#[tokio::test]
async fn a_missing_key_is_the_answer_to_send_and_nothing_is_written() {
    let (engine, _) = engine(vec![], None).await;
    let s = engine.create_session(None).await.unwrap();
    let err = engine.send(s.id.clone(), "hi".into()).await.unwrap_err();
    assert_eq!(err, CoreError::MissingKey { endpoint_name: "Test".into() });
    assert!(engine.snapshot(s.id.clone()).await.unwrap().messages.is_empty());
}

#[tokio::test]
async fn a_session_reaches_the_list_only_once_something_is_said() {
    let (engine, _) = engine(vec![text("hi")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    assert!(engine.list_sessions().await.unwrap().is_empty());
    engine.send(s.id.clone(), "hello".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    let list = engine.list_sessions().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].preview.as_deref(), Some("hi"));
}

#[tokio::test]
async fn stopping_a_running_command_seals_the_call_and_the_transcript_stays_sendable() {
    let (engine, _) = engine(vec![call("c1", r#"{"command":"sleep 30"}"#)], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "wait".into()).await.unwrap();
    loop {
        let ev = rx.recv().await.unwrap();
        if matches!(ev.kind, EventKind::ToolRunning { .. }) {
            break;
        }
    }
    engine.cancel(s.id.clone()).await.unwrap();
    let events = until_turns_finish(&mut rx, 1).await;
    assert_eq!(outcomes(&events), vec![TurnOutcome::Cancelled]);
    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    let last = snap.messages.last().unwrap();
    assert_eq!(last.role, Role::Tool, "every call has a result");
}

/// Wait for the session's title to be set, or time out.
async fn until_titled(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> String {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await.expect("timed out").unwrap();
        if let EventKind::SessionUpdated { info: SessionInfo { title: Some(t), .. } } = ev.kind {
            return t;
        }
    }
}

#[tokio::test]
async fn the_first_reply_titles_the_session_with_thinking_off() {
    let (engine, scripted) = engine(vec![text("Alpine 3.21")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "which alpine?".into()).await.unwrap();
    assert_eq!(until_titled(&mut rx).await, "Scripted title");
    let reqs = scripted.title_requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(!reqs[0].thinking);
    assert!(reqs[0].messages[0].text().contains("User: which alpine?"));
    drop(reqs);
    assert_eq!(engine.list_sessions().await.unwrap()[0].title.as_deref(), Some("Scripted title"));
}

#[tokio::test]
async fn a_failed_title_request_falls_back_to_the_first_message() {
    let (engine, scripted) = engine(vec![text("ok")], Some("k")).await;
    scripted.titles.lock().unwrap().push_back(Reply {
        events: vec![],
        error: Some(CoreError::Http { status: 500, detail: "x".into() }),
        delay_ms: 0,
    });
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "list the files in my workspace please".into()).await.unwrap();
    assert_eq!(until_titled(&mut rx).await, "list the files in my workspace…");
}

#[tokio::test]
async fn thinking_follows_the_session_over_the_settings() {
    let (engine, scripted) = engine(vec![text("a"), text("b")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "one".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    let info = engine.set_session_thinking(s.id.clone(), Some(true)).await.unwrap();
    assert_eq!(info.thinking, Some(true));
    engine.send(s.id.clone(), "two".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    let reqs = scripted.requests.lock().unwrap();
    assert_eq!(reqs.iter().map(|r| r.thinking).collect::<Vec<_>>(), [false, true]);
    drop(reqs);
    assert_eq!(engine.list_sessions().await.unwrap()[0].thinking, Some(true), "stored");
}

#[tokio::test]
async fn retry_deletes_the_old_answer_and_answers_the_last_message_again() {
    let (engine, scripted) = engine(vec![call("c1", r#"{"command":"echo a"}"#), text("first"), text("second")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "go".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    engine.retry(s.id.clone(), None, None).await.unwrap();
    let events = until_turns_finish(&mut rx, 1).await;
    let user_id = engine.snapshot(s.id.clone()).await.unwrap().messages[0].id.clone();
    assert!(events.iter().any(|e| e.kind == EventKind::Truncated { after: Some(user_id.clone()) }));
    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    assert_eq!(snap.messages.iter().map(|m| m.text()).collect::<Vec<_>>(), ["go", "second"]);
    // The retried request carried only the user message.
    assert_eq!(scripted.requests.lock().unwrap()[2].messages.len(), 1);
}

#[tokio::test]
async fn editing_an_earlier_message_replaces_it_and_drops_what_followed() {
    let (engine, scripted) = engine(vec![text("a1"), text("a2"), text("edited answer")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "q1".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    engine.send(s.id.clone(), "q2".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    let first = engine.snapshot(s.id.clone()).await.unwrap().messages[0].id.clone();
    engine.retry(s.id.clone(), Some(first.clone()), Some("q1 again".into())).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    assert_eq!(snap.messages.iter().map(|m| m.text()).collect::<Vec<_>>(), ["q1 again", "edited answer"]);
    assert_eq!(snap.messages[0].id, first, "the same message, edited");
    let req = &scripted.requests.lock().unwrap()[2];
    assert_eq!(req.messages.iter().map(|m| m.text()).collect::<Vec<_>>(), ["q1 again"]);
}

#[tokio::test]
async fn resume_carries_on_after_a_stop_and_refuses_after_a_finished_answer() {
    let (engine, _) = engine(vec![call("c1", r#"{"command":"sleep 30"}"#), text("carried on")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "wait".into()).await.unwrap();
    loop {
        if matches!(rx.recv().await.unwrap().kind, EventKind::ToolRunning { .. }) {
            break;
        }
    }
    engine.cancel(s.id.clone()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    engine.resume(s.id.clone()).await.unwrap();
    assert_eq!(outcomes(&until_turns_finish(&mut rx, 1).await), vec![TurnOutcome::Completed]);
    assert_eq!(engine.snapshot(s.id.clone()).await.unwrap().messages.last().unwrap().text(), "carried on");
    assert_eq!(engine.resume(s.id.clone()).await.unwrap_err(), CoreError::NothingToResume);
}

#[tokio::test]
async fn clear_rename_and_search() {
    let (engine, _) = engine(vec![text("the answer is 391")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "17 × 23?".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    until_titled(&mut rx).await;

    assert_eq!(engine.search_sessions("391".into()).await.unwrap().len(), 1, "finds reply text");
    assert_eq!(engine.search_sessions("scripted".into()).await.unwrap().len(), 1, "finds titles, any case");
    assert!(engine.search_sessions("100%".into()).await.unwrap().is_empty(), "% is literal");

    let exported: Snapshot = serde_json::from_str(&engine.export_session(s.id.clone()).await.unwrap()).unwrap();
    assert_eq!(exported, engine.snapshot(s.id.clone()).await.unwrap(), "the export is the whole snapshot");
    assert!(!engine.export_session(s.id.clone()).await.unwrap().contains("\"k\""), "no key in it");

    let info = engine.rename_session(s.id.clone(), "  Arithmetic ".into()).await.unwrap();
    assert_eq!(info.title.as_deref(), Some("Arithmetic"));

    engine.clear_session(s.id.clone()).await.unwrap();
    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    assert!(snap.messages.is_empty());
    assert_eq!(snap.session.title.as_deref(), Some("Arithmetic"), "the session stays");
    assert!(engine.search_sessions("391".into()).await.unwrap().is_empty());
}

#[tokio::test]
async fn attachments_are_copied_in_and_queued_ones_arrive_with_their_turn() {
    let (engine, scripted) = engine(
        vec![Reply { events: vec![StreamEvent::Text("looking".into()), StreamEvent::Finish(FinishReason::Stop)], error: None, delay_ms: 150 }, text("done")],
        Some("k"),
    )
    .await;
    let src = std::env::temp_dir().join(format!("solos-src-{}.txt", uuid::Uuid::new_v4()));
    std::fs::write(&src, "notes").unwrap();
    let file = || AttachmentSource { name: "notes.txt".into(), host_path: src.to_string_lossy().into(), mime: "text/plain".into() };
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send_with(s.id.clone(), "read this".into(), vec![file()]).await.unwrap();
    engine.send_with(s.id.clone(), String::new(), vec![file()]).await.unwrap();
    assert_eq!(engine.snapshot(s.id.clone()).await.unwrap().queued, ["[notes.txt]"]);
    until_turns_finish(&mut rx, 2).await;

    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    let paths: Vec<String> = snap
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|p| match p {
            Part::Attachment { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(paths, ["/solos/ws/attachments/notes.txt", "/solos/ws/attachments/notes (2).txt"]);
    assert!(engine.resolve_file(&paths[1]).unwrap().exists(), "copied into the workspace");
    // The queued message reached the model with its attachment.
    let last = scripted.requests.lock().unwrap().last().unwrap().messages.last().unwrap().clone();
    assert!(matches!(&last.parts[..], [Part::Attachment { name, .. }] if name == "notes.txt"));
}

#[tokio::test]
async fn near_the_window_the_turn_stops_to_ask_and_a_summary_lets_it_continue() {
    let long = "word ".repeat(12_000); // ~15k tokens a message
    let (engine, scripted) = engine(vec![text("a0"), text("a1"), text("a2"), text("a3"), text("after summary")], Some("k")).await;
    engine.set_model_window(ModelChoice { endpoint_id: "e".into(), model: "m".into() }, Some(80_000)).await.unwrap();
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    for i in 0..4 {
        engine.send(s.id.clone(), format!("{i} {long}")).await.unwrap();
        until_turns_finish(&mut rx, 1).await;
    }
    // The fifth message pushes it past 70k (80k - 10k): the turn asks.
    engine.send(s.id.clone(), format!("4 {long}")).await.unwrap();
    let got = outcomes(&until_turns_finish(&mut rx, 1).await);
    assert!(
        matches!(&got[0], TurnOutcome::Failed { error: CoreError::ContextNearlyFull { window: 80_000, can_summarize: true, .. } }),
        "{got:?}"
    );
    assert_eq!(scripted.requests.lock().unwrap().len(), 4, "nothing was sent");
    assert!(
        matches!(engine.snapshot(s.id.clone()).await.unwrap().last_error, Some(CoreError::ContextNearlyFull { .. })),
        "a chat opened later sees why"
    );

    scripted.titles.lock().unwrap().push_back(text("SUMMARY OF 0 AND 1"));
    let c = engine.compact(s.id.clone()).await.unwrap();
    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    assert_eq!(snap.compactions, vec![c.clone()]);
    assert_eq!(snap.messages.len(), 9, "nothing deleted");
    engine.resume(s.id.clone()).await.unwrap();
    assert_eq!(outcomes(&until_turns_finish(&mut rx, 1).await), vec![TurnOutcome::Completed]);
    let sent = scripted.requests.lock().unwrap().last().unwrap().messages.clone();
    assert!(sent[0].text().contains("SUMMARY OF 0 AND 1"));
    assert!(sent[1].text().starts_with("2 "), "the recent three user turns follow the summary");

    engine.undo_compaction(s.id.clone(), c.id.clone()).await.unwrap();
    assert!(engine.snapshot(s.id.clone()).await.unwrap().compactions[0].undone);
}

#[tokio::test]
async fn identical_calls_in_one_message_run_once_and_share_the_result() {
    let twice = Reply {
        events: vec![
            StreamEvent::ToolCallStart { index: 0, id: "c1".into(), name: "shell".into() },
            StreamEvent::ToolCallArgs { index: 0, fragment: r#"{"command":"echo $$ >> runs.txt; wc -l < runs.txt"}"#.into() },
            StreamEvent::ToolCallStart { index: 1, id: "c2".into(), name: "shell".into() },
            StreamEvent::ToolCallArgs { index: 1, fragment: r#"{"command":"echo $$ >> runs.txt; wc -l < runs.txt"}"#.into() },
            StreamEvent::Finish(FinishReason::ToolCalls),
        ],
        error: None,
        delay_ms: 0,
    };
    let (engine, _) = engine(vec![twice, text("ok")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "go".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    let outs: Vec<(String, String)> = snap.messages[2]
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { call_id, output, .. } => Some((call_id.clone(), output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(outs.len(), 2, "each call has its result");
    // `wc -l` counts the lines written: 1 means the command ran once.
    assert!(outs[0].1.trim_start().starts_with("1\n"), "ran once: {outs:?}");
    assert!(outs[1].1.trim_start().starts_with("1\n") && outs[1].1.contains("ran once"), "{outs:?}");
}

/// Serve one `/models` answer per connection, reporting `window` for `m`.
async fn models_server(window: u64) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = format!(r#"{{"data":[{{"id":"m","context_length":{window}}}]}}"#);
            let reply = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            let _ = sock.write_all(reply.as_bytes()).await;
        }
    });
    format!("http://{addr}/v1")
}

#[tokio::test]
async fn a_window_set_by_hand_outlives_fetching_the_model_list_again() {
    let (engine, _) = engine(vec![], Some("k")).await;
    let choice = ModelChoice { endpoint_id: "e".into(), model: "m".into() };
    let ep = |url: String| Endpoint { id: "e".into(), name: "Test".into(), protocol: Protocol::OpenAi, base_url: url, secret_ref: "k".into() };

    engine.list_models(ep(models_server(32_000).await)).await.unwrap();
    assert_eq!(engine.model_window(choice.clone()), ModelWindow { reported: Some(32_000), custom: None });

    engine.set_model_window(choice.clone(), Some(128_000)).await.unwrap();
    engine.list_models(ep(models_server(64_000).await)).await.unwrap();
    assert_eq!(engine.model_window(choice.clone()), ModelWindow { reported: Some(64_000), custom: Some(128_000) });

    engine.set_model_window(choice.clone(), None).await.unwrap();
    assert_eq!(engine.model_window(choice), ModelWindow { reported: Some(64_000), custom: None }, "cleared: the endpoint's figure again");
}

#[tokio::test]
async fn a_terminal_runs_a_shell_in_the_workspace_and_loses_no_input() {
    let (engine, _) = engine(vec![], Some("k")).await;
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Option<i32>>();
    let sink = seen.clone();
    let id = engine
        .open_terminal(24, 80, Arc::new(move |b| sink.lock().unwrap().extend(b)), Box::new(move |code| {
            let _ = done_tx.send(code);
        }))
        .await
        .unwrap();
    let text = || String::from_utf8_lossy(&seen.lock().unwrap()).into_owned();
    let until = |needle: &'static str| {
        let text = text.clone();
        async move {
            // Up to a minute: on a busy CI machine the paste alone takes 7-8 s.
            for _ in 0..2400 {
                if text().contains(needle) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("never saw {needle:?} in {:?}", text());
        }
    };

    engine.terminal_input(&id, b"echo at-$(basename $(pwd))-$((40+2))\n".to_vec()).unwrap();
    until("at-ws-42").await;

    // 400 lines of 100 characters in one go, as a paste: every byte arrives.
    let mut paste = b"wc -c <<'EOF'\n".to_vec();
    for _ in 0..400 {
        paste.extend_from_slice(&[b'x'; 99]);
        paste.push(b'\n');
    }
    paste.extend_from_slice(b"EOF\n");
    engine.terminal_input(&id, paste).unwrap();
    until("40000").await;

    engine.terminal_input(&id, b"exit\n".to_vec()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), done_rx).await.expect("the shell ended").unwrap();
    for _ in 0..50 {
        if engine.terminal_input(&id, b"x".to_vec()).is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(engine.terminal_input(&id, b"x".to_vec()), Err(CoreError::NoSuchTerminal { terminal_id: id.clone() }));
}

/// A script run by the model reaches the same tools through the bridge,
/// with nothing but what the sandbox put in its environment.
#[tokio::test]
async fn a_script_calls_a_tool_through_the_bridge_in_its_own_session() {
    let script = r#"echo "sid=$SOLOS_SESSION_ID"; curl -s -X POST -H \"Authorization: Bearer $SOLOS_API_TOKEN\" -H \"X-Solos-Session: $SOLOS_SESSION_ID\" -H 'Content-Type: application/json' -d '{\"path\":\"from-script.txt\",\"content\":\"written by a script\"}' \"$SOLOS_API_URL/v1/tools/file_write\""#;
    let args = serde_json::json!({"title": "call a tool", "command": script.replace("\\\"", "\"")}).to_string();
    let (engine, _) = engine(vec![call("c1", &args), text("done")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "run it".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;

    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    let Part::ToolResult { output, .. } = &snap.messages[2].parts[0] else { panic!() };
    assert!(output.contains(&format!("sid={}", s.id)), "{output}");
    assert!(output.contains(r#""ok":true"#) && output.contains(r#""tool":"file_write""#), "{output}");
    let written = std::fs::read_to_string(engine.workspace_dir().join("from-script.txt")).unwrap();
    assert_eq!(written, "written by a script");
}

#[tokio::test]
async fn the_sandbox_status_counts_the_workspace_and_the_database() {
    let (engine, _) = engine(vec![], Some("k")).await;
    std::fs::create_dir_all(engine.workspace_dir().join("sub")).unwrap();
    std::fs::write(engine.workspace_dir().join("sub/a.txt"), vec![0u8; 1000]).unwrap();
    let status = engine.sandbox_status().await;
    assert_eq!(status.error, None);
    assert_eq!(status.workspace_bytes, 1000);
    assert!(status.database_bytes > 0);
    assert_eq!(status.system_bytes, None, "the host shell has no system of its own");
}

#[tokio::test]
async fn temporary_files_are_counted_and_cleared_and_nothing_else_is() {
    let (engine, _) = engine(vec![], Some("k")).await;
    let ws = engine.workspace_dir();
    for (path, len) in [(".solos/pages/p.txt", 300), (".solos/screenshots/s.png", 200), ("mine.txt", 50), (".solos/other.txt", 7)] {
        std::fs::create_dir_all(ws.join(path).parent().unwrap()).unwrap();
        std::fs::write(ws.join(path), vec![0u8; len]).unwrap();
    }
    assert_eq!(engine.sandbox_status().await.temporary_bytes, 500);
    assert_eq!(engine.clear_temporary_files().await.unwrap(), 500);
    assert!(!ws.join(".solos/pages").exists() && !ws.join(".solos/screenshots").exists());
    assert!(ws.join("mine.txt").exists() && ws.join(".solos/other.txt").exists(), "only the two folders go");
    assert_eq!(engine.sandbox_status().await.temporary_bytes, 0);
    assert_eq!(engine.clear_temporary_files().await.unwrap(), 0, "nothing there is not an error");
}

#[tokio::test]
async fn pin_duplicate_retitle_and_export_as_text() {
    let (engine, scripted) = engine(vec![call("c1", r#"{"title":"say hi","command":"echo hi"}"#), text("all done")], Some("k")).await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "run it".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;

    let pinned = engine.set_pinned(s.id.clone(), true).await.unwrap();
    assert!(pinned.pinned_at.is_some());
    let listed = engine.list_sessions().await.unwrap();
    assert!(listed.iter().find(|i| i.id == s.id).unwrap().pinned_at.is_some(), "pinning is stored");
    assert!(engine.set_pinned(s.id.clone(), false).await.unwrap().pinned_at.is_none());

    let copy = engine.duplicate_session(s.id.clone(), Some("Scripted title (Copy)".into())).await.unwrap();
    assert_ne!(copy.id, s.id);
    assert_eq!(copy.title.as_deref(), Some("Scripted title (Copy)"));
    let a = engine.snapshot(s.id.clone()).await.unwrap().messages;
    let b = engine.snapshot(copy.id.clone()).await.unwrap().messages;
    assert_eq!(a.len(), b.len());
    assert!(a.iter().zip(&b).all(|(x, y)| x.parts == y.parts && x.id != y.id));
    assert_eq!(engine.list_sessions().await.unwrap().len(), 2);

    let plain = engine.export_session_text(s.id.clone()).await.unwrap();
    assert!(plain.contains("User:\nrun it") && plain.contains("> say hi (shell)") && plain.contains("Assistant:\nall done"), "{plain}");

    scripted.titles.lock().unwrap().push_back(text("A better title"));
    let retitled = engine.regenerate_title(s.id.clone()).await.unwrap();
    assert_eq!(retitled.title.as_deref(), Some("A better title"));
}

#[tokio::test]
async fn package_mirrors_are_chosen_per_kind_kept_across_a_restart_and_unknown_ones_refused() {
    let dir = std::env::temp_dir().join(format!("solos-mirror-{}", uuid::Uuid::new_v4()));
    let open = || async {
        Engine::open(EngineConfig {
            data_dir: dir.clone(),
            sandbox: Arc::new(HostSandbox::new(dir.join("guest"))),
            tools: Registry::builtin(),
            secrets: Arc::new(Key(Some("k"))),
            capture_dir: None,
            provider_factory: None,
        })
        .await
        .unwrap()
    };
    let chosen = |e: &Engine| e.chosen_package_mirrors().into_iter().map(|m| (m.kind, m.id)).collect::<Vec<_>>();
    let engine = open().await;
    assert_eq!(chosen(&engine), vec![(MirrorKind::Alpine, "official".into()), (MirrorKind::Pip, "official".into()), (MirrorKind::Npm, "official".into())]);
    engine.set_package_mirror(MirrorKind::Pip, "tuna".into()).await.unwrap();
    engine.set_package_mirror(MirrorKind::Npm, "npmmirror".into()).await.unwrap();
    assert!(engine.set_package_mirror(MirrorKind::Npm, "aliyun".into()).await.is_err(), "no such npm mirror");
    drop(engine);
    let engine = open().await;
    assert_eq!(chosen(&engine), vec![(MirrorKind::Alpine, "official".into()), (MirrorKind::Pip, "tuna".into()), (MirrorKind::Npm, "npmmirror".into())]);
    assert!(engine.choose_package_mirrors_if_fresh().await.unwrap().is_empty(), "the host shell is never a fresh system");
}

#[tokio::test]
async fn a_skill_folder_reaches_the_prompt_until_turned_off_or_removed() {
    let (engine, scripted) = engine(vec![text("one"), text("two"), text("three")], Some("k")).await;
    let dir = engine.workspace_dir().join("skills/trains");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: trains\ndescription: Look up train tickets.\n---\nRun q.py").unwrap();
    let listed = "- trains: Look up train tickets. (/solos/ws/skills/trains/SKILL.md)";
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    let system = |i: usize| scripted.requests.lock().unwrap()[i].system.clone();

    engine.send(s.id.clone(), "a".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    assert!(system(0).contains(listed), "{}", system(0));
    assert_eq!(engine.skill_instructions("trains".into()).unwrap(), "---\nname: trains\ndescription: Look up train tickets.\n---\nRun q.py");

    engine.set_skill_enabled("trains".into(), false).await.unwrap();
    assert!(!engine.skills()[0].enabled);
    engine.send(s.id.clone(), "b".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    assert!(!system(1).contains(listed));
    assert!(system(1).contains("No skills are installed."));

    engine.remove_skill("trains".into()).await.unwrap();
    assert!(engine.skills().is_empty());
    assert!(!dir.exists());
    assert!(matches!(engine.remove_skill("trains".into()).await, Err(CoreError::NoSuchSkill { .. })));
}

#[tokio::test]
async fn turned_off_skills_stay_off_across_a_restart() {
    let dir = std::env::temp_dir().join(format!("solos-skills-{}", uuid::Uuid::new_v4()));
    let open = || async {
        Engine::open(EngineConfig {
            data_dir: dir.clone(),
            sandbox: Arc::new(HostSandbox::new(dir.join("guest"))),
            tools: Registry::builtin(),
            secrets: Arc::new(Key(Some("k"))),
            capture_dir: None,
            provider_factory: None,
        })
        .await
        .unwrap()
    };
    let engine = open().await;
    for folder in ["a", "b"] {
        let d = engine.workspace_dir().join("skills").join(folder);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), "x").unwrap();
    }
    engine.set_skill_enabled("b".into(), false).await.unwrap();
    drop(engine);
    let engine = open().await;
    let on: Vec<(String, bool)> = engine.skills().into_iter().map(|s| (s.folder, s.enabled)).collect();
    assert_eq!(on, vec![("a".into(), true), ("b".into(), false)]);
}

fn call_tool(id: &str, name: &str, args: &str) -> Reply {
    Reply {
        events: vec![
            StreamEvent::ToolCallStart { index: 0, id: id.into(), name: name.into() },
            StreamEvent::ToolCallArgs { index: 0, fragment: args.into() },
            StreamEvent::Finish(FinishReason::ToolCalls),
        ],
        error: None,
        delay_ms: 0,
    }
}

/// A stdio MCP server in plain `sh`: it logs a line to stdout first (which
/// a client must skip), lists one tool, `add`, and answers it.
fn fake_stdio_server(dir: &std::path::Path) -> String {
    let path = dir.join("fake-mcp.sh");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        &path,
        r#"echo "fake server starting"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}\n' "$id" ;;
    *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"add","description":"Adds a and b.","inputSchema":{"type":"object","properties":{"a":{"type":"number"},"b":{"type":"number"}},"required":["a","b"]}}]}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      a=$(printf '%s' "$line" | sed -n 's/.*"a":\([0-9]*\).*/\1/p'); b=$(printf '%s' "$line" | sed -n 's/.*"b":\([0-9]*\).*/\1/p')
      echo "called with $a $b" >&2
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$((a+b))" ;;
  esac
done
"#,
    )
    .unwrap();
    path.to_string_lossy().into_owned()
}

fn tool_output(snap: &Snapshot, call_id: &str) -> (String, bool) {
    snap.messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .find_map(|p| match p {
            Part::ToolResult { call_id: c, output, is_error, .. } if c == call_id => Some((output.clone(), *is_error)),
            _ => None,
        })
        .expect("a result for the call")
}

#[tokio::test]
async fn a_server_added_in_a_turn_is_usable_on_its_next_round() {
    let dir = std::env::temp_dir().join(format!("solos-mcp-{}", uuid::Uuid::new_v4()));
    let script = fake_stdio_server(&dir);
    let add = format!(r#"{{"title":"add the server","name":"fake","config":{{"command":"sh","args":["{script}"]}}}}"#);
    let (engine, scripted) = engine(
        vec![call_tool("c1", "mcp_add", &add), call_tool("c2", "mcp_fake_add", r#"{"title":"sum","a":3,"b":4}"#), text("7")],
        Some("k"),
    )
    .await;
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "add it and add 3 and 4".into()).await.unwrap();
    assert_eq!(outcomes(&until_turns_finish(&mut rx, 1).await), vec![TurnOutcome::Completed]);

    let snap = engine.snapshot(s.id.clone()).await.unwrap();
    let (added, err) = tool_output(&snap, "c1");
    assert!(!err && added.starts_with("Added the MCP server `fake` with 1 tool:\n- mcp_fake_add: Adds a and b."), "{added}");
    assert_eq!(tool_output(&snap, "c2"), ("7".to_string(), false));

    let names = |i: usize| scripted.requests.lock().unwrap()[i].tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert!(!names(0).contains(&"mcp_fake_add".to_string()));
    assert!(names(1).contains(&"mcp_fake_add".to_string()), "offered on the round after it was added");
    let req = scripted.requests.lock().unwrap()[1].clone();
    let spec = req.tools.iter().find(|t| t.name == "mcp_fake_add").unwrap();
    assert_eq!(spec.schema["required"], serde_json::json!(["title", "a", "b"]));
    assert!(req.system.contains("No servers are added."), "the prompt is from the turn's start");

    let servers = engine.mcp_servers();
    assert_eq!((servers.len(), servers[0].tools.len(), servers[0].error.clone()), (1, 1, None));
}

#[tokio::test]
async fn a_server_that_will_not_start_is_kept_with_what_it_said() {
    let (engine, _) = engine(vec![], Some("k")).await;
    let added = engine
        .add_mcp_servers(r#"{"mcpServers": {"broken": {"command": "sh", "args": ["-c", "echo missing dependency >&2; exit 3"]}}}"#.into())
        .await
        .unwrap();
    let err = added[0].error.clone().expect("an error");
    assert!(err.contains("missing dependency"), "{err}");
    assert!(added[0].tools.is_empty());
    assert_eq!(engine.mcp_servers().len(), 1, "kept, so it can be fixed or deleted");
    engine.remove_mcp_server("broken".into()).await.unwrap();
    assert!(engine.mcp_servers().is_empty());
    assert!(matches!(engine.remove_mcp_server("broken".into()).await, Err(CoreError::NoSuchMcpServer { .. })));
}

#[tokio::test]
async fn a_saved_server_starts_on_its_first_call_and_stops_when_turned_off() {
    let dir = std::env::temp_dir().join(format!("solos-mcp-{}", uuid::Uuid::new_v4()));
    let script = fake_stdio_server(&dir);
    let data = std::env::temp_dir().join(format!("solos-mcp-data-{}", uuid::Uuid::new_v4()));
    let open = |script: Vec<Reply>| {
        let data = data.clone();
        async move {
            let scripted = Arc::new(Scripted {
                replies: Mutex::new(script.into()),
                requests: Mutex::new(vec![]),
                titles: Mutex::new(VecDeque::new()),
                title_requests: Mutex::new(vec![]),
            });
            let p = scripted.clone();
            let engine = Engine::open(EngineConfig {
                data_dir: data.clone(),
                sandbox: Arc::new(HostSandbox::new(data.join("guest"))),
                tools: Registry::builtin(),
                secrets: Arc::new(Key(Some("k"))),
                capture_dir: None,
                provider_factory: Some(Arc::new(move |_ep: &Endpoint, _k: String, _c: &Capture| Ok(p.clone() as Arc<dyn Provider>))),
            })
            .await
            .unwrap();
            (engine, scripted)
        }
    };
    let (engine, _) = open(vec![]).await;
    engine.add_mcp_servers(format!(r#"{{"mcpServers": {{"fake": {{"command": "sh", "args": ["{script}"]}}}}}}"#)).await.unwrap();
    drop(engine);

    // A new engine knows the tools from the store, and starts the server
    // only when one is called.
    let (engine, scripted) = open(vec![call_tool("c1", "mcp_fake_add", r#"{"title":"sum","a":20,"b":22}"#), text("42"), text("off")]).await;
    engine
        .set_settings(Settings {
            endpoints: vec![Endpoint { id: "e".into(), name: "Test".into(), protocol: Protocol::OpenAi, base_url: String::new(), secret_ref: "k".into() }],
            default_model: Some(ModelChoice { endpoint_id: "e".into(), model: "m".into() }),
            thinking: false,
        })
        .await
        .unwrap();
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "add 20 and 22".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    assert!(scripted.requests.lock().unwrap()[0].system.contains("Added servers: fake (1 tool)"));
    assert_eq!(tool_output(&engine.snapshot(s.id.clone()).await.unwrap(), "c1"), ("42".to_string(), false));

    engine.set_mcp_server_enabled("fake".into(), false).await.unwrap();
    engine.send(s.id.clone(), "again".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    let last = scripted.requests.lock().unwrap().last().unwrap().clone();
    assert!(!last.tools.iter().any(|t| t.name.starts_with("mcp_fake_")), "a server turned off offers nothing");
    assert!(last.system.contains("No servers are added."));
}

/// A streamable-HTTP MCP server: JSON answers, an event-stream answer for
/// `tools/call`, and a session it insists on after `initialize`.
async fn fake_http_server() -> String {
    use axum::{http::HeaderMap, response::IntoResponse, routing::post, Json, Router};
    async fn handle(headers: HeaderMap, Json(msg): Json<serde_json::Value>) -> axum::response::Response {
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or_default().to_string();
        let session = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
        if method != "initialize" && session.as_deref() != Some("s-1") {
            return (axum::http::StatusCode::BAD_REQUEST, "no session").into_response();
        }
        match method.as_str() {
            "initialize" => (
                [("mcp-session-id", "s-1")],
                Json(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "h", "version": "1"}}})),
            )
                .into_response(),
            "notifications/initialized" => axum::http::StatusCode::ACCEPTED.into_response(),
            "tools/list" => Json(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [{"name": "echo", "description": "Echoes.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}})).into_response(),
            "tools/call" => {
                let text = msg["params"]["arguments"]["text"].as_str().unwrap_or_default();
                let note = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}});
                let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": format!("echo: {text}")}]}});
                ([("content-type", "text/event-stream")], format!("event: message\ndata: {note}\n\nevent: message\ndata: {reply}\n\n")).into_response()
            }
            _ => (axum::http::StatusCode::NOT_FOUND, "?").into_response(),
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, Router::new().route("/mcp", post(handle))).await.unwrap() });
    format!("http://{addr}/mcp")
}

#[tokio::test]
async fn an_http_server_is_used_with_its_session_and_event_stream_answers() {
    let url = fake_http_server().await;
    let (engine, _) = engine(vec![call_tool("c1", "mcp_web_echo", r#"{"title":"echo","text":"hi"}"#), text("done")], Some("k")).await;
    let added = engine.add_mcp_servers(format!(r#"{{"mcpServers": {{"web": {{"type": "http", "url": "{url}"}}}}}}"#)).await.unwrap();
    assert_eq!((added[0].error.clone(), added[0].tools.len()), (None, 1));
    let mut rx = engine.subscribe();
    let s = engine.create_session(None).await.unwrap();
    engine.send(s.id.clone(), "echo hi".into()).await.unwrap();
    until_turns_finish(&mut rx, 1).await;
    assert_eq!(tool_output(&engine.snapshot(s.id.clone()).await.unwrap(), "c1"), ("echo: hi".to_string(), false));
}
