//! How attached files reach a model, whatever the protocol.
//!
//! Images are sent as images — the most recent [`IMAGE_BUDGET`] of them;
//! older ones are named with their path so the model can look again. Other
//! files are listed with their path, URL and size for the model to open with
//! its tools. The shapes follow the reference app.

use base64::Engine as _;
use solos_api::{Message, Part};
use std::collections::HashSet;
use std::path::Path;

/// Images sent as images per request, newest first.
pub const IMAGE_BUDGET: usize = 20;

/// Paths of the attached images that are sent as images.
pub fn inline_set(messages: &[Message]) -> HashSet<String> {
    messages
        .iter()
        .rev()
        .flat_map(|m| m.parts.iter().rev())
        .filter(|p| p.is_image())
        .filter_map(|p| match p {
            Part::Attachment { path, .. } => Some(path.clone()),
            _ => None,
        })
        .take(IMAGE_BUDGET)
        .collect()
}

/// An image file's bytes as base64, read from the workspace.
pub fn image_base64(workspace: &Path, guest_path: &str) -> Option<String> {
    let host = crate::files::resolve(guest_path, workspace)?;
    let bytes = std::fs::read(host).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// An image file as a `data:` URL, read from the workspace.
pub fn image_data_url(workspace: &Path, guest_path: &str, mime: &str) -> Option<String> {
    Some(format!("data:{mime};base64,{}", image_base64(workspace, guest_path)?))
}

/// What the model is told about an image that is not sent.
pub fn omitted_note(p: &Part) -> String {
    match p {
        Part::Attachment { path, size, .. } => {
            format!("[attached image not included to save context — {} — {path}]", human_size(*size))
        }
        _ => String::new(),
    }
}

/// The files that are not images: where each is and how big, for the model to open with its tools.
pub fn files_block(parts: &[Part]) -> Option<String> {
    let lines: Vec<String> = parts
        .iter()
        .filter(|p| !p.is_image())
        .filter_map(|p| match p {
            Part::Attachment { path, size, .. } => {
                let url = path.replacen(crate::sandbox::GUEST_WORKSPACE, "solos://ws", 1);
                Some(format!("  <file path=\"{path}\" url=\"{url}\" size=\"{size}\" />"))
            }
            _ => None,
        })
        .collect();
    (!lines.is_empty()).then(|| format!("<attachments>\n{}\n</attachments>", lines.join("\n")))
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::openai::convert_messages;
    use serde_json::json;
    use solos_api::Role;

    fn image(path: &str) -> Part {
        Part::Attachment { path: path.into(), name: "x.png".into(), mime: "image/png".into(), size: 3 }
    }

    fn user(parts: Vec<Part>) -> Message {
        Message { id: "u".into(), role: Role::User, parts, created_at: 0 }
    }

    #[test]
    fn an_image_is_sent_as_an_image_with_its_path_and_a_file_is_listed() {
        let ws = std::env::temp_dir().join(format!("solos-att-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(ws.join("attachments")).unwrap();
        std::fs::write(ws.join("attachments/a.png"), [1u8, 2, 3]).unwrap();
        let msg = user(vec![
            Part::Text { text: "what is this?".into() },
            image("/solos/ws/attachments/a.png"),
            Part::Attachment { path: "/solos/ws/attachments/r.pdf".into(), name: "r.pdf".into(), mime: "application/pdf".into(), size: 10 },
        ]);
        let out = convert_messages(&[msg], false, Some(&ws));
        let content = &out[0]["content"];
        assert_eq!(content[0], json!({"type": "text", "text": "what is this?"}));
        assert_eq!(content[1]["text"], "[attached image: /solos/ws/attachments/a.png]");
        assert_eq!(content[2]["image_url"]["url"], "data:image/png;base64,AQID");
        let block = content[3]["text"].as_str().unwrap();
        assert!(block.contains(r#"<file path="/solos/ws/attachments/r.pdf" url="solos://ws/attachments/r.pdf" size="10" />"#), "{block}");
    }

    #[test]
    fn files_alone_keep_the_content_a_string() {
        let msg = user(vec![Part::Attachment { path: "/solos/ws/attachments/n.txt".into(), name: "n.txt".into(), mime: "text/plain".into(), size: 1 }]);
        let out = convert_messages(&[msg], false, None);
        assert!(out[0]["content"].as_str().unwrap().starts_with("<attachments>"));
    }

    #[test]
    fn only_the_newest_images_are_sent() {
        let msgs: Vec<Message> = (0..IMAGE_BUDGET + 2).map(|i| user(vec![image(&format!("/solos/ws/attachments/{i}.png"))])).collect();
        let set = inline_set(&msgs);
        assert_eq!(set.len(), IMAGE_BUDGET);
        assert!(!set.contains("/solos/ws/attachments/0.png") && !set.contains("/solos/ws/attachments/1.png"));
        assert!(set.contains(&format!("/solos/ws/attachments/{}.png", IMAGE_BUDGET + 1)));
        // An image that is not sent, or cannot be read, is named instead.
        let out = convert_messages(&msgs[..1], false, None);
        assert!(out[0]["content"].as_str().unwrap().contains("not included"));
    }
}
