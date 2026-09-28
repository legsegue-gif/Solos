//! Server-sent events: split a byte stream into `data:` payloads.

/// Accumulates bytes and yields complete `data:` payloads. Comments
/// (`: keepalive`) and other fields are dropped; multi-line data is joined
/// with `\n` as the spec says.
#[derive(Default)]
pub struct SseParser {
    buf: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
        }
        out
    }

    /// Whatever is left when the stream ends without a final blank line.
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        if let Some(d) = rest.trim_end().strip_prefix("data:") {
            self.data.push(d.trim_start().to_string());
        }
        (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunk_boundaries_and_skips_comments() {
        let mut p = SseParser::default();
        assert!(p.push(b": keepalive\n\ndata: {\"a\"").is_empty());
        assert_eq!(p.push(b":1}\n\ndata: [DONE]\n\n"), vec!["{\"a\":1}", "[DONE]"]);
    }

    #[test]
    fn a_stream_cut_without_a_blank_line_still_yields_its_last_event() {
        let mut p = SseParser::default();
        assert!(p.push(b"data: last").is_empty());
        assert_eq!(p.finish().as_deref(), Some("last"));
    }
}
