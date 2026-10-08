//! `read_image`: show the model a picture from the workspace — a chart a
//! script drew, a download, a screenshot, a photo the user attached.

use super::{object_schema, Tool, ToolContext, ToolImage, ToolOutput, ToolSpec};
use crate::sandbox::GUEST_WORKSPACE;
use async_trait::async_trait;
use serde_json::{json, Value};

/// Larger images are refused rather than sent: providers reject them.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

pub struct ReadImage;

#[async_trait]
impl Tool for ReadImage {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_image".into(),
            description: "Look at an image in the workspace: a chart or picture a script made, a download, a browser \
                screenshot, a photo the user attached. PNG, JPEG, GIF and WebP. You get the picture itself, with its size."
                .into(),
            schema: object_schema(
                json!({
                    "path": {"type": "string", "description": "The image, e.g. /solos/ws/chart.png (a solos://ws/ link or a path relative to /solos/ws also works)."}
                }),
                &["path"],
            ),
            parallel: true,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let Some(path) = input.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()) else {
            return ToolOutput::error("`path` is required.");
        };
        let workspace = ctx.sandbox.workspace_dir();
        let Some(host) = crate::files::resolve_argument(path, &workspace) else {
            let way = if path.contains("/mnt/") { "`file_copy`" } else { "`shell`" };
            return ToolOutput::error(format!(
                "{path} is not inside the workspace ({GUEST_WORKSPACE}). Copy it there with {way} first."
            ));
        };
        let rel = host.strip_prefix(&workspace).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
        let guest = format!("{GUEST_WORKSPACE}/{rel}");
        let bytes = match std::fs::read(&host) {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("Cannot read {guest}: {e}")),
        };
        let Some(mime) = media_type(&bytes) else {
            return ToolOutput::error(format!(
                "{guest} is not a PNG, JPEG, GIF or WebP image. Convert it first (for example with Python's Pillow)."
            ));
        };
        let size = bytes.len() as u64;
        if size > MAX_BYTES {
            return ToolOutput::error(format!(
                "{guest} is {} KB, more than the {} KB an image may be. Make a smaller copy first (for example with Pillow's thumbnail).",
                size / 1024,
                MAX_BYTES / 1024
            ));
        }
        let dims = dimensions(&bytes).map(|(w, h)| format!(" · {w}×{h}")).unwrap_or_default();
        ToolOutput {
            text: format!("{guest} · {mime}{dims} · {} KB", size.div_ceil(1024)),
            is_error: false,
            images: vec![ToolImage { path: guest, mime: mime.to_string(), size }],
        }
    }
}

/// Decided by the bytes, not the name.
pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Width and height, for the formats whose header says so plainly.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be = |i: usize| Some(u32::from_be_bytes(bytes.get(i..i + 4)?.try_into().ok()?));
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some((be(16)?, be(20)?));
    }
    if bytes.starts_with(b"GIF") {
        let le = |i: usize| Some(u16::from_le_bytes(bytes.get(i..i + 2)?.try_into().ok()?) as u32);
        return Some((le(6)?, le(8)?));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        // Walk the segments to the frame header, which carries the size.
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            let length = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + length;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::host::HostSandbox;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    /// 1×1 transparent PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

    fn ctx() -> (ToolContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("solos-img-{}", uuid::Uuid::new_v4()));
        let sandbox = Arc::new(HostSandbox::new(dir));
        let ws = crate::sandbox::Sandbox::workspace_dir(&*sandbox);
        std::fs::create_dir_all(&ws).unwrap();
        (ToolContext { session_id: "s".into(), sandbox, cancel: CancellationToken::new(), on_output: Arc::new(|_| {}) }, ws)
    }

    #[tokio::test]
    async fn an_image_comes_back_as_itself_with_its_size() {
        use base64::Engine as _;
        let (c, ws) = ctx();
        std::fs::write(ws.join("dot.png"), base64::engine::general_purpose::STANDARD.decode(PNG).unwrap()).unwrap();
        let out = ReadImage.call(&c, &json!({"path": "solos://ws/dot.png"})).await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.starts_with("/solos/ws/dot.png · image/png · 1×1"), "{}", out.text);
        assert_eq!(out.images, vec![ToolImage { path: "/solos/ws/dot.png".into(), mime: "image/png".into(), size: 68 }]);
    }

    #[tokio::test]
    async fn what_is_not_an_image_or_not_in_the_workspace_is_refused() {
        let (c, ws) = ctx();
        std::fs::write(ws.join("a.png"), b"not really").unwrap();
        let out = ReadImage.call(&c, &json!({"path": "a.png"})).await;
        assert!(out.is_error && out.images.is_empty(), "{}", out.text);
        assert!(ReadImage.call(&c, &json!({"path": "/etc/hosts"})).await.is_error);
    }
}
