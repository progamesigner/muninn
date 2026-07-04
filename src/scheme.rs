//! The VFS scheme (`MUNINN_VFS_SCHEME`).
//!
//! A scheme is a dotted sequence of literal and `<ident>` placeholder segments.
//! Each distinct placeholder ident becomes a required scope parameter on every
//! tool call. At resolve time the scheme renders to a single dotted string that
//! is used both as the per-scope directory segment under the agents folder and as
//! the suffix appended to a file stem.
//!
//! Grammar (design.md D4):
//! ```text
//! scheme     := segment ( '.' segment )*
//! segment      := placeholder | literal
//! placeholder  := '<' ident '>'
//! literal      := [A-Za-z0-9_-]+
//! ident        := [A-Za-z_][A-Za-z0-9_]*
//! ```
//! The empty string is a valid scheme that disables suffixing entirely.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// A single scheme segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// A fixed literal segment, emitted verbatim.
    Literal(String),
    /// A `<ident>` placeholder, replaced by the caller-supplied scope value.
    Placeholder(String),
}

/// A parsed VFS suffix scheme.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scheme {
    segments: Vec<Segment>,
}

/// An error parsing a scheme string. Carries enough context for a startup
/// message that names the offending character or placeholder.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemeError {
    #[error("empty segment (a '.' with nothing on one side)")]
    EmptySegment,
    #[error("unclosed placeholder in segment '{segment}' (expected a closing '>')")]
    UnclosedPlaceholder { segment: String },
    #[error("stray '<' or '>' in literal segment '{segment}'")]
    StrayBracket { segment: String },
    #[error("invalid character '{ch}' in literal segment '{segment}'")]
    InvalidLiteralChar { segment: String, ch: char },
    #[error("invalid placeholder name '<{ident}>' (must match [A-Za-z_][A-Za-z0-9_]*)")]
    InvalidPlaceholderIdent { ident: String },
}

/// An error rendering a scheme against a set of scope arguments.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderError {
    #[error("missing required scope key '{0}'")]
    MissingKey(String),
    #[error("unexpected scope key '{0}'")]
    UnexpectedKey(String),
}

impl Scheme {
    /// Parse a scheme string per the grammar above.
    pub fn parse(s: &str) -> std::result::Result<Scheme, SchemeError> {
        if s.is_empty() {
            return Ok(Scheme::default());
        }

        let mut segments = Vec::new();
        for part in s.split('.') {
            if part.is_empty() {
                return Err(SchemeError::EmptySegment);
            }

            if let Some(after) = part.strip_prefix('<') {
                let ident =
                    after
                        .strip_suffix('>')
                        .ok_or_else(|| SchemeError::UnclosedPlaceholder {
                            segment: part.to_string(),
                        })?;
                validate_ident(ident)?;
                segments.push(Segment::Placeholder(ident.to_string()));
            } else if part.contains('<') || part.contains('>') {
                return Err(SchemeError::StrayBracket {
                    segment: part.to_string(),
                });
            } else {
                validate_literal(part)?;
                segments.push(Segment::Literal(part.to_string()));
            }
        }

        Ok(Scheme { segments })
    }

    /// `true` when the scheme has no segments (suffixing disabled).
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// The ordered, de-duplicated list of placeholder idents — the required scope
    /// parameter names, in first-occurrence order.
    pub fn placeholders(&self) -> Vec<&str> {
        let mut seen = Vec::new();
        for seg in &self.segments {
            if let Segment::Placeholder(ident) = seg
                && !seen.contains(&ident.as_str())
            {
                seen.push(ident.as_str());
            }
        }
        seen
    }

    /// Render the scheme into a single dotted string.
    ///
    /// Validates that `scope` contains exactly the placeholder idents — no missing
    /// keys, no extra keys. A repeated placeholder repeats its value.
    pub fn render(
        &self,
        scope: &BTreeMap<String, String>,
    ) -> std::result::Result<String, RenderError> {
        let placeholders = self.placeholders();

        for key in &placeholders {
            if !scope.contains_key(*key) {
                return Err(RenderError::MissingKey((*key).to_string()));
            }
        }
        for key in scope.keys() {
            if !placeholders.contains(&key.as_str()) {
                return Err(RenderError::UnexpectedKey(key.clone()));
            }
        }

        let rendered = self
            .segments
            .iter()
            .map(|seg| match seg {
                Segment::Literal(literal) => literal.as_str(),
                // Safe: every placeholder was checked present above.
                Segment::Placeholder(ident) => scope[ident].as_str(),
            })
            .collect::<Vec<_>>()
            .join(".");

        Ok(rendered)
    }

    /// The JSON-Schema fragment contributed by the scheme's scope fields.
    ///
    /// Returns `{ "properties": { <ident>: {"type":"string", ...}, ... },
    /// "required": [<ident>, ...] }`. Both are empty for an empty scheme.
    pub fn to_json_schema(&self) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for ident in self.placeholders() {
            properties.insert(
                ident.to_string(),
                json!({
                    "type": "string",
                    "description": format!("Scope key '{ident}' identifying the caller."),
                    "minLength": 1,
                }),
            );
            required.push(Value::String(ident.to_string()));
        }
        json!({ "properties": Value::Object(properties), "required": required })
    }
}

fn validate_ident(ident: &str) -> std::result::Result<(), SchemeError> {
    let mut chars = ident.chars();
    let ok = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(SchemeError::InvalidPlaceholderIdent {
            ident: ident.to_string(),
        })
    }
}

fn validate_literal(literal: &str) -> std::result::Result<(), SchemeError> {
    for ch in literal.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
            return Err(SchemeError::InvalidLiteralChar {
                segment: literal.to_string(),
                ch,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn default_scheme_requires_agent_and_user() {
        let t = Scheme::parse("<agent>.<user>").unwrap();
        assert_eq!(t.placeholders(), vec!["agent", "user"]);
        assert_eq!(
            t.render(&scope(&[("agent", "jarvis"), ("user", "tony")]))
                .unwrap(),
            "jarvis.tony"
        );
    }

    #[test]
    fn single_key_scheme() {
        let t = Scheme::parse("<agent>").unwrap();
        assert_eq!(t.placeholders(), vec!["agent"]);
        assert_eq!(t.render(&scope(&[("agent", "jarvis")])).unwrap(), "jarvis");
    }

    #[test]
    fn empty_scheme_disables_suffixing() {
        let t = Scheme::parse("").unwrap();
        assert!(t.is_empty());
        assert!(t.placeholders().is_empty());
        assert_eq!(t.render(&scope(&[])).unwrap(), "");
    }

    #[test]
    fn multi_key_scheme_renders_in_order() {
        let t = Scheme::parse("<team>.<agent>.<env>.<user>").unwrap();
        assert_eq!(t.placeholders(), vec!["team", "agent", "env", "user"]);
        assert_eq!(
            t.render(&scope(&[
                ("team", "platform"),
                ("agent", "jarvis"),
                ("env", "prod"),
                ("user", "tony"),
            ]))
            .unwrap(),
            "platform.jarvis.prod.tony"
        );
    }

    #[test]
    fn literal_segment_is_emitted_verbatim() {
        let t = Scheme::parse("v1.<agent>.<user>").unwrap();
        assert_eq!(t.placeholders(), vec!["agent", "user"]);
        assert_eq!(
            t.render(&scope(&[("agent", "jarvis"), ("user", "tony")]))
                .unwrap(),
            "v1.jarvis.tony"
        );
    }

    #[test]
    fn repeated_placeholder_collapses_to_one_param_but_repeats_value() {
        let t = Scheme::parse("<agent>.<agent>").unwrap();
        assert_eq!(t.placeholders(), vec!["agent"]);
        assert_eq!(t.render(&scope(&[("agent", "x")])).unwrap(), "x.x");
    }

    #[test]
    fn unclosed_bracket_is_rejected() {
        assert!(matches!(
            Scheme::parse("<agent"),
            Err(SchemeError::UnclosedPlaceholder { .. })
        ));
    }

    #[test]
    fn invalid_placeholder_idents_are_rejected() {
        assert!(matches!(
            Scheme::parse("<1bad>"),
            Err(SchemeError::InvalidPlaceholderIdent { .. })
        ));
        assert!(matches!(
            Scheme::parse("<a-b>"),
            Err(SchemeError::InvalidPlaceholderIdent { .. })
        ));
    }

    #[test]
    fn empty_segment_is_rejected() {
        assert_eq!(Scheme::parse("a..b"), Err(SchemeError::EmptySegment));
        assert_eq!(Scheme::parse(".a"), Err(SchemeError::EmptySegment));
        assert_eq!(Scheme::parse("a."), Err(SchemeError::EmptySegment));
    }

    #[test]
    fn invalid_literal_char_is_rejected() {
        assert!(matches!(
            Scheme::parse("v$1.<agent>"),
            Err(SchemeError::InvalidLiteralChar { ch: '$', .. })
        ));
    }

    #[test]
    fn render_rejects_missing_key() {
        let t = Scheme::parse("<agent>.<user>").unwrap();
        assert_eq!(
            t.render(&scope(&[("agent", "jarvis")])),
            Err(RenderError::MissingKey("user".to_string()))
        );
    }

    #[test]
    fn render_rejects_extra_key() {
        let t = Scheme::parse("<agent>").unwrap();
        assert_eq!(
            t.render(&scope(&[("agent", "jarvis"), ("user", "tony")])),
            Err(RenderError::UnexpectedKey("user".to_string()))
        );
    }

    #[test]
    fn json_schema_lists_required_scope_fields() {
        let t = Scheme::parse("<agent>.<user>").unwrap();
        let schema = t.to_json_schema();
        assert_eq!(schema["required"], json!(["agent", "user"]));
        assert_eq!(schema["properties"]["agent"]["type"], "string");
        assert_eq!(schema["properties"]["user"]["type"], "string");
    }

    #[test]
    fn json_schema_is_empty_for_empty_scheme() {
        let t = Scheme::parse("").unwrap();
        let schema = t.to_json_schema();
        assert_eq!(schema["required"], json!([]));
        assert_eq!(schema["properties"], json!({}));
    }
}
