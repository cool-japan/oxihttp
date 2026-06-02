//! URL-encoded form body builder.

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

/// Builder for URL-encoded form bodies (`application/x-www-form-urlencoded`).
#[derive(Debug, Clone, Default)]
pub struct FormBody {
    fields: Vec<(String, String)>,
}

impl FormBody {
    /// Create a new empty form builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a field to the form.
    pub fn field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push((name.into(), value.into()));
        self
    }

    /// Build the form body as raw bytes.
    pub fn build(self) -> Bytes {
        let encoded: String = self
            .fields
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    utf8_percent_encode(k, NON_ALPHANUMERIC),
                    utf8_percent_encode(v, NON_ALPHANUMERIC)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        Bytes::from(encoded)
    }

    /// Returns the number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns `true` if no fields have been added.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_form() {
        let body = FormBody::new().build();
        assert!(body.is_empty());
    }

    #[test]
    fn test_single_field() {
        let body = FormBody::new().field("key", "value").build();
        assert_eq!(&body[..], b"key=value");
    }

    #[test]
    fn test_multiple_fields() {
        let body = FormBody::new().field("a", "1").field("b", "2").build();
        assert_eq!(&body[..], b"a=1&b=2");
    }

    #[test]
    fn test_special_characters_encoded() {
        let body = FormBody::new()
            .field("query", "hello world")
            .field("path", "/foo/bar")
            .build();
        let s = std::str::from_utf8(&body).expect("valid utf-8");
        assert!(s.contains("hello%20world"));
        assert!(s.contains("%2Ffoo%2Fbar"));
    }

    #[test]
    fn test_len() {
        let form = FormBody::new().field("a", "1").field("b", "2");
        assert_eq!(form.len(), 2);
        assert!(!form.is_empty());
    }
}
