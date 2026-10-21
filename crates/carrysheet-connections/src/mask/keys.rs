//! Which key names hold a secret.
//!
//! The rule errs on the side of masking: hiding a value that was not secret
//! costs a little readability, while showing one that was is a leak.

/// Words that make a key secret wherever they appear in it (after the key is
/// lower-cased and everything but letters and digits is removed), so
/// `User Password`, `Jet OLEDB:Database Password`, `ProxyPassword` and
/// `client_secret` all match.
const SECRET_PARTS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "passcode",
    "pwd",
    "secret",
    "token",
    "apikey",
    "accountkey",
    "accesskey",
    "privatekey",
    "signature",
    "credential",
    "assertion",
    "cookie",
    "session",
    "jwt",
];

/// Keys that are secret only when they are the whole key: short words that
/// would match too much as parts (`sig` in `signal`, `code` in `codepage`).
const SECRET_WHOLE: &[&str] = &[
    "pass",
    "pw",
    "sig",
    "code",
    "auth",
    "authorization",
    "bearer",
    "otp",
    "pin",
];

/// Endings that name a secret without holding one: `SharedAccessKeyName`,
/// `AccessKeyId`, `TokenType`.
const NOT_SECRET_ENDINGS: &[&str] = &["keyname", "keyid", "tokentype", "secretname"];

/// Lower-cases `key` and keeps only ASCII letters and digits.
fn normalize(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// True when the value of a key with this name must be masked.
///
/// Any key whose normalized name ends in `key` counts (`AccountKey`,
/// `SharedAccessKey`, `Ocp-Apim-Subscription-Key`, `key` itself), and so
/// does any that ends in `pass` (`dbPass`, `SmtpPass`).
pub(crate) fn is_secret_key(key: &str) -> bool {
    let key = strip_key_quotes(key.trim());
    let norm = normalize(key);
    if norm.is_empty() || NOT_SECRET_ENDINGS.iter().any(|end| norm.ends_with(end)) {
        return false;
    }
    SECRET_WHOLE.contains(&norm.as_str())
        || norm.ends_with("key")
        || norm.ends_with("pass")
        || SECRET_PARTS.iter().any(|part| norm.contains(part))
}

/// Key endings that name a real secret, where most words ending in `key`
/// (`CustomerKey`, `ProductKey` as a surrogate key) are plain columns.
const SECRET_KEY_ENDINGS: &[&str] = &[
    "apikey",
    "accountkey",
    "accesskey",
    "privatekey",
    "secretkey",
    "subscriptionkey",
    "sharedaccesskey",
    "masterkey",
    "encryptionkey",
    "signingkey",
    "functionskey",
];

/// Short words that name a secret in a query only as a whole word or a
/// whole `_` piece of one: `pin` and `user_pin`, never `Spinner`.
const SQL_WHOLE: &[&str] = &["pass", "pw", "pin", "otp", "bearer", "authorization"];

/// Parts that name a secret in a key but are ordinary columns in a query:
/// `SessionID`, `CookieConsent`.
const SQL_PLAIN_PARTS: &[&str] = &["session", "cookie"];

/// True when a word in a query names a secret. Stricter than
/// [`is_secret_key`] about short and generic words, since query words are
/// column and table names: `key`, `code`, `sig`, `auth` and `session` alone
/// do not count, nor does a word that ends in `key` unless it names a kind
/// of secret key (`api_key`, `AccessKey`), so `CustomerKey` does not.
pub(crate) fn is_secret_word(word: &str) -> bool {
    let word = strip_key_quotes(word.trim());
    let norm = normalize(word);
    if norm.is_empty() || NOT_SECRET_ENDINGS.iter().any(|end| norm.ends_with(end)) {
        return false;
    }
    let whole_piece = word
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|piece| SQL_WHOLE.contains(&piece.to_ascii_lowercase().as_str()));
    whole_piece
        || norm.ends_with("pass")
        || SECRET_PARTS
            .iter()
            .filter(|part| !SQL_PLAIN_PARTS.contains(part))
            .any(|part| norm.contains(part))
        || SECRET_KEY_ENDINGS.iter().any(|end| norm.ends_with(end))
}

/// Removes the wrapping of a quoted key: `"pwd"`, `'pwd'`, `#"api-key"`,
/// `[Password]`.
fn strip_key_quotes(key: &str) -> &str {
    let key = key.strip_prefix('#').unwrap_or(key);
    for (open, close) in [('"', '"'), ('\'', '\''), ('[', ']')] {
        if let Some(inner) = key.strip_prefix(open).and_then(|k| k.strip_suffix(close)) {
            return inner;
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::{is_secret_key, is_secret_word};

    #[test]
    fn secret_words_in_queries() {
        for word in [
            "password",
            "pwd",
            "api_key",
            "AccessKey",
            "secret_key",
            "access_key",
            "client_secret",
            "token",
            "passcode",
            "pin",
            "user_pin",
            "vw_PasswordResets",
        ] {
            assert!(is_secret_word(word), "{word}");
        }
        for word in [
            "CustomerKey",
            "ProductKey",
            "DateKey",
            "region",
            "key",
            "code",
            "SessionID",
            "Spinner",
            "Shipping",
        ] {
            assert!(!is_secret_word(word), "{word}");
        }
    }

    #[test]
    fn secret_and_plain_keys() {
        for key in [
            "Password",
            "PWD",
            "pwd",
            "User Password",
            "Jet OLEDB:Database Password",
            "Jet OLEDB:New Database Password",
            "AccessToken",
            "access_token",
            "Token",
            "SharedAccessKey",
            "AccountKey",
            "Secret",
            "ApiKey",
            "api-key",
            "#\"x-api-key\"",
            "Client Secret",
            "sig",
            "Authorization",
            "[Password]",
            "key",
            "Ocp-Apim-Subscription-Key",
            "dbPass",
            "v_password",
            "Passphrase",
        ] {
            assert!(is_secret_key(key), "{key} should be secret");
        }
        for key in [
            "Server",
            "Data Source",
            "Initial Catalog",
            "User ID",
            "UID",
            "Provider",
            "Persist Security Info",
            "signal",
            "codepage",
            "SharedAccessKeyName",
            "AccessKeyId",
            "se",
            "sv",
            "",
        ] {
            assert!(!is_secret_key(key), "{key} should not be secret");
        }
    }
}
