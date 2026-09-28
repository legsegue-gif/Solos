//! The order a model list is shown in. Endpoints report models in whatever
//! order they keep them, so finding one means reading the whole list.

use solos_api::ModelInfo;
use std::cmp::Ordering;

/// By id (what the app shows), ignoring case, with runs of digits compared
/// as numbers: `qwen3.10` comes after `qwen3.9`.
pub fn sort_models(models: &mut [ModelInfo]) {
    models.sort_by(|a, b| natural(&a.id, &b.id).then_with(|| a.id.cmp(&b.id)));
}

fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = digits(&mut a);
                let db = digits(&mut b);
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                a.next();
                b.next();
            }
        }
    }
}

fn digits(it: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut s = String::new();
    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
        s.push(c);
        it.next();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, name: Option<&str>) -> ModelInfo {
        ModelInfo { id: id.into(), display_name: name.map(Into::into), context_window: None, max_output: None }
    }

    #[test]
    fn models_are_sorted_by_id_with_numbers_as_numbers() {
        let mut list = vec![
            m("qwen3.8-max", None),
            m("qwen3.10-plus", None),
            m("Qwen3.7-max", None),
            m("claude-x", Some("Zeta")),
            m("qwen3.8", None),
            m("gpt-5", None),
        ];
        sort_models(&mut list);
        let ids: Vec<_> = list.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["claude-x", "gpt-5", "Qwen3.7-max", "qwen3.8", "qwen3.8-max", "qwen3.10-plus"]);
    }
}
