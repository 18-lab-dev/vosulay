use std::fmt::Write as _;

const MANAGER_HTML: &str = include_str!("../assets/manager.html");
const MANAGER_CSS: &str = include_str!("../assets/manager.css");
const MANAGER_JS: &str = include_str!("../assets/manager.js");
const BOOTSTRAP_MARKER: &str = "__VOSULAY_BOOTSTRAP__";

/// Builds the complete manager page without depending on files at runtime.
///
/// JavaScript and CSS are encoded as JavaScript string literals before they are
/// inserted into the document. In particular, `<` is encoded as `\u003c`, so
/// embedded `</script>` text cannot terminate the bootstrap script early.
pub fn manager_document(fps_limit: Option<u32>) -> String {
    let fps = fps_limit.map_or_else(|| "null".to_owned(), |limit| limit.to_string());
    let css = javascript_string(MANAGER_CSS);
    let javascript = javascript_string(MANAGER_JS);
    let bootstrap = format!(
        "window.__VOSULAY_FPS_LIMIT__={fps};(()=>{{const style=document.createElement(\"style\");style.textContent={css};document.head.appendChild(style);(0,eval)({javascript});}})();"
    );

    debug_assert_eq!(MANAGER_HTML.matches(BOOTSTRAP_MARKER).count(), 1);
    MANAGER_HTML.replacen(BOOTSTRAP_MARKER, &bootstrap, 1)
}

fn javascript_string(source: &str) -> String {
    let mut encoded = String::with_capacity(source.len() + 2);
    encoded.push('"');
    for character in source.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            '\u{08}' => encoded.push_str("\\b"),
            '\u{0c}' => encoded.push_str("\\f"),
            '<' => encoded.push_str("\\u003c"),
            '>' => encoded.push_str("\\u003e"),
            '&' => encoded.push_str("\\u0026"),
            '\u{2028}' => encoded.push_str("\\u2028"),
            '\u{2029}' => encoded.push_str("\\u2029"),
            control if control <= '\u{1f}' => {
                write!(encoded, "\\u{:04x}", control as u32)
                    .expect("writing to a String cannot fail");
            }
            other => encoded.push(other),
        }
    }
    encoded.push('"');
    encoded
}

#[cfg(test)]
mod tests {
    use super::{BOOTSTRAP_MARKER, javascript_string, manager_document};

    #[test]
    fn document_is_self_contained_and_has_numeric_fps_bootstrap() {
        let document = manager_document(Some(144));

        assert!(!document.contains(BOOTSTRAP_MARKER));
        assert!(document.contains("window.__VOSULAY_FPS_LIMIT__=144"));
        assert!(!document.contains("manager.css"));
        assert!(!document.contains("manager.js"));
    }

    #[test]
    fn no_limiter_is_bootstrapped_as_null() {
        assert!(manager_document(None).contains("window.__VOSULAY_FPS_LIMIT__=null"));
    }

    #[test]
    fn embedded_script_closers_are_encoded() {
        let encoded = javascript_string("before</ScRiPt><style>&after");

        assert!(!encoded.contains('<'));
        assert!(encoded.contains("\\u003c/ScRiPt\\u003e"));
    }
}
