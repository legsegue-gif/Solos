//! `solos` — the app's tools, from a shell inside the sandbox.
//!
//! The model reaches the calendar, the clipboard or the browser through its
//! tools; a script could not. This command is a thin call onto the same
//! tools, served by the core on 127.0.0.1: it implements no capability of
//! its own, so a tool behaves the same whether the model or a script asked.
//!
//!     solos device calendar list --from today --to +7d
//!     solos call browser --action navigate --url https://example.com
//!     solos files url /solos/ws/report.png
//!     solos tools
//!
//! The address and token arrive in the environment (`SOLOS_API_URL`,
//! `SOLOS_API_TOKEN`, and `SOLOS_SESSION_ID` inside a conversation).

mod http;
mod json;

use std::process::ExitCode;

/// Exit codes, so a script can branch without parsing anything.
mod exit {
    pub const OK: u8 = 0;
    pub const ERROR: u8 = 1;
    pub const INVALID_ARGS: u8 = 2;
    pub const AUTH_DENIED: u8 = 3;
    pub const NOT_AVAILABLE: u8 = 4;
}

const USAGE: &str = "\
solos — the app's tools, from inside the sandbox

USAGE
  solos device <capability> <action> [--key value | --flag]...
  solos call <tool> [--key value | --flag]...
  solos files url <path>
  solos tools

EXAMPLES
  solos device calendar list --from today --to +7d
  solos device notify --heading Done --body \"the build passed\"
  solos device clipboard read
  solos call browser --action navigate --url https://example.com
  solos files url /solos/ws/report.png

ARGUMENTS
  --key value   a string field (hyphens in the key become underscores)
  --flag        a flag with no value is true
  --key:json v  v sent as raw JSON, for numbers, arrays and objects

  `solos tools` lists every tool with its description; the fields each one
  takes are the same as for the model.

OPTIONS
  --raw         print the whole reply envelope, not just its data
  -h, --help    this text

EXIT CODES
  0 ok   1 the call failed   2 bad arguments   3 not authorised   4 no such tool

The app supplies SOLOS_API_URL and SOLOS_API_TOKEN to every process in the
sandbox. Without them this command cannot work and says so.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("solos: {}", e.message);
            ExitCode::from(e.code)
        }
    }
}

#[derive(Debug)]
struct Fail {
    code: u8,
    message: String,
}

fn fail(code: u8, message: impl Into<String>) -> Fail {
    Fail { code, message: message.into() }
}

/// What to send: a method, a path, and a body for a tool call.
fn request_for(args: &[String]) -> Result<(&'static str, String, Option<String>), Fail> {
    match args[0].as_str() {
        "device" => {
            // `device <capability>` with an optional bare action after it,
            // since nearly every capability takes one.
            let Some(capability) = args.get(1) else {
                return Err(fail(exit::INVALID_ARGS, "usage: solos device <capability> [<action>] [--key value]..."));
            };
            let mut rest = &args[2..];
            let mut fields = Vec::new();
            if let Some(action) = rest.first().filter(|a| !a.starts_with("--")) {
                fields.push(("action".to_string(), json::quote(action)));
                rest = &rest[1..];
            }
            fields.extend(parse_pairs(rest)?);
            Ok(("POST", format!("/v1/tools/device_{capability}"), Some(json::object(&fields))))
        }
        "call" => {
            let Some(tool) = args.get(1) else {
                return Err(fail(exit::INVALID_ARGS, "usage: solos call <tool> [--key value]..."));
            };
            let fields = parse_pairs(&args[2..])?;
            Ok(("POST", format!("/v1/tools/{tool}"), Some(json::object(&fields))))
        }
        "files" => match (args.get(1).map(String::as_str), args.get(2)) {
            (Some("url"), Some(path)) => Ok(("GET", format!("/v1/files/url?path={}", http::encode_query(path)), None)),
            _ => Err(fail(exit::INVALID_ARGS, "usage: solos files url <path>")),
        },
        "tools" => Ok(("GET", "/v1/tools".to_string(), None)),
        other => Err(fail(exit::INVALID_ARGS, format!("unknown command {other:?}. `solos --help` lists what there is."))),
    }
}

fn run(args: &[String]) -> Result<u8, Fail> {
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        println!("{USAGE}");
        return Ok(exit::OK);
    }
    let raw = args.iter().any(|a| a == "--raw");
    let args: Vec<String> = args.iter().filter(|a| *a != "--raw").cloned().collect();
    let (method, path, body) = request_for(&args)?;

    let base = std::env::var("SOLOS_API_URL")
        .map_err(|_| fail(exit::NOT_AVAILABLE, "SOLOS_API_URL is not set; this command only works inside the Solos sandbox"))?;
    let token = std::env::var("SOLOS_API_TOKEN")
        .map_err(|_| fail(exit::AUTH_DENIED, "SOLOS_API_TOKEN is not set; the app did not hand out a token"))?;
    let session = std::env::var("SOLOS_SESSION_ID").unwrap_or_default();

    let reply = http::request(&base, method, &path, &token, &session, body.as_deref()).map_err(|e| fail(exit::ERROR, e))?;

    // A refusal before the call is not the call saying no: its own codes.
    let error = || {
        json::top_level_field(&reply.body, "error")
            .and_then(json::unquote)
            .unwrap_or_else(|| reply.body.clone())
    };
    match reply.status {
        401 => return Err(fail(exit::AUTH_DENIED, "the app rejected the token")),
        404 => return Err(fail(exit::NOT_AVAILABLE, error())),
        400 => return Err(fail(exit::INVALID_ARGS, error())),
        _ => {}
    }
    let ok = json::top_level_field(&reply.body, "ok") == Some("true");
    if raw {
        println!("{}", reply.body);
    } else if ok {
        if let Some(data) = json::top_level_field(&reply.body, "data") {
            println!("{data}");
        } else if let Some(text) = json::top_level_field(&reply.body, "text").and_then(json::unquote) {
            println!("{text}");
        }
    }
    if ok {
        Ok(exit::OK)
    } else {
        Err(fail(exit::ERROR, error()))
    }
}

/// `--key value` → a string field; `--flag` → `true`; `--key:json v` → `v`
/// verbatim.
fn parse_pairs(args: &[String]) -> Result<Vec<(String, String)>, Fail> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let Some(name) = arg.strip_prefix("--").filter(|n| !n.is_empty()) else {
            return Err(fail(exit::INVALID_ARGS, format!("expected an option starting with --, got {arg:?}")));
        };
        let (name, is_json) = match name.strip_suffix(":json") {
            Some(n) => (n, true),
            None => (name, false),
        };
        let key = name.replace('-', "_");
        let value = args.get(i + 1).filter(|v| !v.starts_with("--"));
        match (is_json, value) {
            (true, Some(v)) => {
                out.push((key, v.clone()));
                i += 2;
            }
            (true, None) => return Err(fail(exit::INVALID_ARGS, format!("--{name}:json needs a value"))),
            (false, Some(v)) => {
                out.push((key, json::quote(v)));
                i += 2;
            }
            (false, None) => {
                out.push((key, "true".to_string()));
                i += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn pairs(args: &[&str]) -> Vec<(String, String)> {
        parse_pairs(&strings(args)).unwrap()
    }

    #[test]
    fn values_become_strings_and_bare_flags_become_true() {
        assert_eq!(pairs(&["--from", "today", "--to", "+7d"]), vec![("from".into(), "\"today\"".into()), ("to".into(), "\"+7d\"".into())]);
        assert_eq!(pairs(&["--all-day"]), vec![("all_day".into(), "true".into())]);
        assert_eq!(pairs(&["--completed", "--id", "x"]), vec![("completed".into(), "true".into()), ("id".into(), "\"x\"".into())]);
    }

    #[test]
    fn the_json_escape_hatch_passes_a_value_through_untouched() {
        assert_eq!(pairs(&["--count:json", "3"]), vec![("count".into(), "3".into())]);
        assert!(parse_pairs(&strings(&["--count:json"])).is_err());
    }

    #[test]
    fn a_stray_positional_is_an_error_not_a_silent_drop() {
        let e = parse_pairs(&strings(&["oops"])).unwrap_err();
        assert_eq!(e.code, exit::INVALID_ARGS);
    }

    #[test]
    fn a_value_that_would_break_the_json_is_escaped() {
        assert_eq!(pairs(&["--name", "say \"hi\"\nnow"])[0].1, "\"say \\\"hi\\\"\\nnow\"");
    }

    #[test]
    fn device_takes_an_optional_bare_action_and_call_takes_none() {
        let (m, path, body) = request_for(&strings(&["device", "calendar", "list", "--from", "today"])).unwrap();
        assert_eq!((m, path.as_str()), ("POST", "/v1/tools/device_calendar"));
        assert_eq!(body.unwrap(), r#"{"action":"list","from":"today"}"#);
        let (_, _, body) = request_for(&strings(&["device", "info"])).unwrap();
        assert_eq!(body.unwrap(), "{}");
        let (_, path, body) = request_for(&strings(&["call", "browser", "--action", "tabs"])).unwrap();
        assert_eq!(path, "/v1/tools/browser");
        assert_eq!(body.unwrap(), r#"{"action":"tabs"}"#);
        let (m, path, _) = request_for(&strings(&["files", "url", "/solos/ws/a b.png"])).unwrap();
        assert_eq!((m, path.as_str()), ("GET", "/v1/files/url?path=/solos/ws/a%20b.png"));
    }

    #[test]
    fn missing_environment_names_the_reason() {
        std::env::remove_var("SOLOS_API_URL");
        let e = run(&strings(&["tools"])).unwrap_err();
        assert_eq!(e.code, exit::NOT_AVAILABLE);
        assert!(e.message.contains("SOLOS_API_URL"), "{}", e.message);
    }
}
