//! `solos` — one turn from the terminal, on the host's shell.
//!
//! ```sh
//! SOLOS_BASE_URL=https://api.example.com/v1 SOLOS_API_KEY=... SOLOS_MODEL=... \
//!   solos "list the files in the workspace"
//! ```
//!
//! Optional: `SOLOS_DATA_DIR` (default: a temporary directory),
//! `SOLOS_CAPTURE_DIR` (write request bodies and raw streams there),
//! `SOLOS_EVENTS=1` (print every event as JSON).

use solos_api::{Endpoint, Event, EventKind, ModelChoice, Protocol, Settings, TurnOutcome};
use solos_core::sandbox::host::HostSandbox;
use solos_core::tools::Registry;
use solos_core::{Engine, EngineConfig, SecretResolver};
use std::path::PathBuf;
use std::sync::Arc;

struct EnvSecret;
impl SecretResolver for EnvSecret {
    fn secret(&self, _reference: &str) -> Option<String> {
        std::env::var("SOLOS_API_KEY").ok()
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        eprintln!("{name} is not set");
        std::process::exit(2)
    })
}

#[tokio::main]
async fn main() {
    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if prompt.trim().is_empty() {
        eprintln!("usage: solos <prompt>");
        std::process::exit(2);
    }
    let data_dir = std::env::var("SOLOS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("solos-cli"));
    let engine = Engine::open(EngineConfig {
        data_dir: data_dir.clone(),
        sandbox: Arc::new(HostSandbox::new(data_dir.join("guest"))),
        tools: Registry::builtin(),
        secrets: Arc::new(EnvSecret),
        capture_dir: std::env::var("SOLOS_CAPTURE_DIR").ok().map(PathBuf::from),
        provider_factory: None,
    })
    .await
    .unwrap_or_else(|e| {
        eprintln!("could not start: {e}");
        std::process::exit(1)
    });
    let endpoint = Endpoint {
        id: "env".into(),
        name: "env".into(),
        protocol: match std::env::var("SOLOS_PROTOCOL").as_deref() {
            Ok("anthropic") => Protocol::Anthropic,
            Ok("gemini") => Protocol::Gemini,
            _ => Protocol::OpenAi,
        },
        base_url: env("SOLOS_BASE_URL"),
        secret_ref: "env".into(),
    };
    engine
        .set_settings(Settings {
            endpoints: vec![endpoint],
            default_model: Some(ModelChoice { endpoint_id: "env".into(), model: env("SOLOS_MODEL") }),
            thinking: true,
        })
        .await
        .expect("settings");

    let verbose = std::env::var("SOLOS_EVENTS").is_ok();
    let mut events = engine.subscribe();
    let session = engine.create_session(None).await.expect("session");
    if let Err(e) = engine.send(session.id.clone(), prompt).await {
        eprintln!("could not send: {e}");
        std::process::exit(1);
    }
    loop {
        let Ok(ev) = events.recv().await else { break };
        if verbose {
            println!("{}", serde_json::to_string(&ev).unwrap_or_default());
        }
        match ev.kind {
            EventKind::TextDelta { delta, .. } if !verbose => print!("{delta}"),
            EventKind::ToolCallReady { input_json, title, .. } if !verbose => {
                println!("\n[tool] {} {input_json}", title.unwrap_or_default())
            }
            EventKind::TurnFinished { outcome, .. } => {
                println!();
                match outcome {
                    TurnOutcome::Completed => {
                        print_title(&engine, &session.id, &mut events).await;
                        break;
                    }
                    TurnOutcome::Cancelled => std::process::exit(130),
                    TurnOutcome::Failed { error } => {
                        eprintln!("turn failed: {error}");
                        std::process::exit(1)
                    }
                }
            }
            _ => {}
        }
    }
}

/// The session's title, once the core has written it (it is written in the
/// background after the first reply).
async fn print_title(engine: &Engine, id: &str, events: &mut tokio::sync::broadcast::Receiver<Event>) {
    let wait = async {
        loop {
            if let Ok(Some(t)) = engine.snapshot(id.to_string()).await.map(|s| s.session.title) {
                return t;
            }
            match events.recv().await {
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(200)).await,
            }
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(90), wait).await {
        Ok(t) => eprintln!("[title] {t}"),
        Err(_) => eprintln!("[title] none after 90s"),
    }
}
