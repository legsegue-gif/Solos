//! Just enough JSON for a CLI with no dependencies.
//!
//! Three jobs, and no more: build a flat object out of `--key value` pairs,
//! pull one top-level field out of a reply, and decode a string field for
//! printing. A full parser is not needed to do
//! either, and the guest has no `jq` to fall back on — which is exactly why
//! this has to be right rather than approximate.

/// Escape a string into a JSON string literal, quotes included.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Control characters have to be escaped or the document is
            // invalid; anything else goes through as UTF-8.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The text of a JSON string literal (quotes included), with every escape
/// expanded, `\uXXXX` and surrogate pairs too. `None` if it is not one.
pub fn unquote(literal: &str) -> Option<String> {
    let inner = literal.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let hex = |chars: &mut std::str::Chars| -> Option<u32> {
                    let s: String = chars.by_ref().take(4).collect();
                    (s.len() == 4).then(|| u32::from_str_radix(&s, 16).ok()).flatten()
                };
                let hi = hex(&mut chars)?;
                let code = if (0xD800..0xDC00).contains(&hi) {
                    // A surrogate pair: the low half must follow as `\uXXXX`.
                    if chars.next()? != '\\' || chars.next()? != 'u' {
                        return None;
                    }
                    let lo = hex(&mut chars)?;
                    0x10000 + ((hi - 0xD800) << 10) + (lo.checked_sub(0xDC00)?)
                } else {
                    hi
                };
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Build `{"k": v, ...}` from already-rendered values.
pub fn object(fields: &[(String, String)]) -> String {
    let body: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("{}:{}", quote(k), v))
        .collect();
    format!("{{{}}}", body.join(","))
}

/// The raw text of one top-level field, or `None` if the document has no such
/// key at depth 1. Walks the document tracking strings, escapes and nesting,
/// so a `"data"` appearing *inside* a nested object or a string is not
/// mistaken for the field being asked for.
pub fn top_level_field<'a>(doc: &'a str, want: &str) -> Option<&'a str> {
    let b = doc.as_bytes();
    let mut i = 0;
    // Step into the outermost object.
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= b.len() || b[i] != b'{' {
        return None;
    }
    i += 1;

    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() || b[i] == b'}' {
            return None;
        }
        if b[i] != b'"' {
            return None;
        }
        let (key, next) = read_string(doc, i)?;
        i = next;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b':' {
            return None;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        i = skip_value(doc, i)?;
        if key == want {
            return Some(doc[start..i].trim());
        }
    }
}

/// Reads a JSON string starting at `at` (which must be the opening quote).
/// Returns its decoded-enough-for-keys contents and the index just past the
/// closing quote. Keys in this protocol are plain ASCII, so escapes are only
/// skipped, not expanded.
fn read_string(doc: &str, at: usize) -> Option<(String, usize)> {
    let b = doc.as_bytes();
    let mut i = at + 1;
    let mut out = String::new();
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
            }
            b'"' => return Some((out, i + 1)),
            _ => {
                let start = i;
                while i < b.len() && b[i] != b'"' && b[i] != b'\\' {
                    i += 1;
                }
                out.push_str(&doc[start..i]);
            }
        }
    }
    None
}

/// Index just past the value starting at `at`.
fn skip_value(doc: &str, at: usize) -> Option<usize> {
    let b = doc.as_bytes();
    if at >= b.len() {
        return None;
    }
    match b[at] {
        b'"' => read_string(doc, at).map(|(_, n)| n),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = at;
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = read_string(doc, i)?.1;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let mut i = at;
            while i < b.len() && !matches!(b[i], b',' | b'}' | b']') && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            Some(i)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_the_things_a_title_actually_contains() {
        assert_eq!(quote("hi"), "\"hi\"");
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote("a\\b"), "\"a\\\\b\"");
        assert_eq!(quote("line\nbreak"), "\"line\\nbreak\"");
        assert_eq!(quote("蟋蟀"), "\"蟋蟀\"", "UTF-8 passes through");
        assert_eq!(quote("\u{7}"), "\"\\u0007\"");
    }

    #[test]
    fn a_nested_key_of_the_same_name_is_not_mistaken_for_the_top_level_one() {
        let doc = r#"{"ok":true,"tool":"device_calendar","data":{"ok":false,"data":"inner"},"n":3}"#;
        assert_eq!(top_level_field(doc, "ok"), Some("true"));
        assert_eq!(top_level_field(doc, "tool"), Some("\"device_calendar\""));
        assert_eq!(top_level_field(doc, "data"), Some(r#"{"ok":false,"data":"inner"}"#));
        assert_eq!(top_level_field(doc, "n"), Some("3"));
        assert_eq!(top_level_field(doc, "missing"), None);
    }

    #[test]
    fn a_key_name_hiding_inside_a_string_is_not_a_key() {
        let doc = r#"{"note":"the word \"data\": is in here","data":1}"#;
        assert_eq!(top_level_field(doc, "data"), Some("1"));
    }

    #[test]
    fn arrays_and_escapes_do_not_derail_the_scan() {
        let doc = r#"{"a":[1,{"b":2},"]"],"c":"tail\\","d":null}"#;
        assert_eq!(top_level_field(doc, "c"), Some(r#""tail\\""#));
        assert_eq!(top_level_field(doc, "d"), Some("null"));
    }

    #[test]
    fn a_string_decodes_every_escape_json_has() {
        assert_eq!(unquote(r#""a\"b\\c\/d\ne\tf""#).as_deref(), Some("a\"b\\c/d\ne\tf"));
        assert_eq!(unquote(r#""\u4e2d\u6587""#).as_deref(), Some("中文"));
        assert_eq!(unquote(r#""\ud83d\ude00""#).as_deref(), Some("😀"));
        assert_eq!(unquote("\"直接的中文\"").as_deref(), Some("直接的中文"));
        assert_eq!(unquote("not a string"), None);
        assert_eq!(unquote(r#""\x""#), None);
    }

    #[test]
    fn objects_render_with_escaped_keys_and_given_values() {
        let fields = vec![
            ("action".to_string(), quote("list")),
            ("all_day".to_string(), "true".to_string()),
        ];
        assert_eq!(object(&fields), r#"{"action":"list","all_day":true}"#);
    }
}
