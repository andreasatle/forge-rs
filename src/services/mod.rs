//! Stateless services.
//!
//! Services transform input data into output data without owning long-lived
//! machine state.
//!
//! Examples:
//! - prompt rendering
//! - config loading
//! - response parsing
//! - plan validation
//! - graph validation
//!
//! If a component has durable state and transitions over time, it belongs under
//! `machines/`, not `services/`.

pub mod time;

/// Extract the first balanced JSON object from `s`.
///
/// Returns a slice of `s` from the opening `{` to the matching `}`, ignoring
/// any leading non-`{` characters or trailing content (including whitespace and
/// provider artifacts) that appear after the closing brace.
///
/// Returns `None` if no complete top-level JSON object is found.
pub fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => {
                i += 2;
                continue;
            }
            b'"' => {
                in_string = !in_string;
            }
            b'{' if !in_string => {
                depth += 1;
            }
            b'}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_bare_object() {
        assert_eq!(extract_json_object(r#"{"a":1}"#), Some(r#"{"a":1}"#));
    }

    #[test]
    fn extract_ignores_trailing_content() {
        let cases = [
            ("newline", "{\"a\":1}\n"),
            ("spaces and tabs", "{\"a\":1}  \t  "),
            ("provider artifact", "{\"a\":1}\nsome trailing text"),
        ];

        for (name, input) in cases {
            assert_eq!(extract_json_object(input), Some("{\"a\":1}"), "{name}");
        }
    }

    #[test]
    fn extract_handles_nested_objects() {
        assert_eq!(
            extract_json_object(r#"{"a":{"b":2}}"#),
            Some(r#"{"a":{"b":2}}"#)
        );
    }

    #[test]
    fn extract_handles_braces_inside_strings() {
        assert_eq!(
            extract_json_object(r#"{"a":"use {} here"}"#),
            Some(r#"{"a":"use {} here"}"#)
        );
    }

    #[test]
    fn extract_handles_escaped_quote_inside_string() {
        assert_eq!(
            extract_json_object(r#"{"a":"say \"hi\""}"#),
            Some(r#"{"a":"say \"hi\""}"#)
        );
    }

    #[test]
    fn extract_returns_none_for_no_object() {
        assert_eq!(extract_json_object("no braces here"), None);
    }

    #[test]
    fn extract_returns_none_for_unclosed_object() {
        assert_eq!(extract_json_object(r#"{"a":1"#), None);
    }
}
