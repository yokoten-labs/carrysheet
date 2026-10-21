//! Masking secrets in connection strings, URLs and free text.
//!
//! Every string this crate returns passes through [`clean`] or
//! [`clean_connection`] first; there is no way to get the raw text back.

mod connection_string;
mod keys;
mod nearby;
mod text;
mod url;

pub use connection_string::mask_connection_string;
pub(crate) use connection_string::pairs;
pub(crate) use keys::{is_secret_key, is_secret_word};
pub use text::mask_text as mask_secrets;

/// What a masked value is replaced with. The key before it stays.
pub const MASK: &str = "***";

/// Masks `raw` as free text and cuts it to `max_chars` characters.
pub(crate) fn clean(raw: &str, max_chars: usize) -> String {
    clip(mask_secrets(raw), max_chars)
}

pub(crate) use text::mask_text_owned;

/// Cuts `text` to `max_chars` characters and says how much was cut. Masking
/// runs before cutting, so a secret is never half shown.
pub(crate) fn clip(text: String, max_chars: usize) -> String {
    let Some((cut_at, _)) = text.char_indices().nth(max_chars) else {
        return text;
    };
    let rest = text[cut_at..].chars().count();
    let mut out = text[..cut_at].to_string();
    out.push_str(&format!(" [cut: {rest} more characters]"));
    out
}

#[cfg(test)]
mod tests {
    use super::clip;

    #[test]
    fn clip_counts_characters() {
        assert_eq!(clip("abc".into(), 3), "abc");
        assert_eq!(clip("abcdé".into(), 3), "abc [cut: 2 more characters]");
        assert_eq!(clip(String::new(), 0), "");
    }
}
