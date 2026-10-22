use base64::Engine as _;
use std::fmt::Write as _;

const MANAGER_HTML: &str = include_str!("../assets/manager.html");
const MANAGER_CSS: &str = include_str!("../assets/manager.css");
const MANAGER_JS: &str = include_str!("../assets/manager.js");
const BOOTSTRAP_MARKER: &str = "__VOSULAY_BOOTSTRAP__";
const FONTS: &[(&str, &[u8])] = &[
    (
        "barlow-latin-400-normal.woff2",
        include_bytes!("../assets/fonts/barlow-latin-400-normal.woff2"),
    ),
    (
        "barlow-latin-600-normal.woff2",
        include_bytes!("../assets/fonts/barlow-latin-600-normal.woff2"),
    ),
    (
        "barlow-condensed-latin-600-normal.woff2",
        include_bytes!("../assets/fonts/barlow-condensed-latin-600-normal.woff2"),
    ),
    (
        "ibm-plex-mono-latin-400-normal.woff2",
        include_bytes!("../assets/fonts/ibm-plex-mono-latin-400-normal.woff2"),
    ),
];

fn manager_styles() -> String {
    let mut css = String::with_capacity(
        MANAGER_CSS.len()
            + FONTS
                .iter()
                .map(|(_, bytes)| bytes.len().div_ceil(3) * 4)
                .sum::<usize>(),
    );
    let mut rest = MANAGER_CSS;
    while let Some((before, after)) = rest.split_once("vosulay-font:") {
        css.push_str(before);
        let (name, after) = after.split_once('"').expect("font URL closes");
        let bytes = FONTS
            .iter()
            .find(|(file, _)| *file == name)
            .expect("bundled editor font")
            .1;
        css.push_str("data:font/woff2;base64,");
        base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut css);
        css.push('"');
        rest = after;
    }
    css.push_str(rest);
    css
}

/// Builds the complete manager page without depending on files at runtime.
///
/// JavaScript and CSS are encoded as JavaScript string literals before they are
/// inserted into the document. In particular, `<` is encoded as `\u003c`, so
/// embedded `</script>` text cannot terminate the bootstrap script early.
pub fn manager_document() -> String {
    manager_document_with("")
}

/// The manager page with trusted `prelude` JavaScript run before the manager
/// script, as the manage page uses to install its HTTP host bridge.
pub(crate) fn manager_document_with(prelude: &str) -> String {
    let css = javascript_string(&manager_styles());
    let javascript = javascript_string(MANAGER_JS);
    let bootstrap = format!(
        "(()=>{{{prelude}const style=document.createElement(\"style\");style.textContent={css};document.head.appendChild(style);(0,eval)({javascript});}})();"
    );

    debug_assert_eq!(MANAGER_HTML.matches(BOOTSTRAP_MARKER).count(), 1);
    MANAGER_HTML.replacen(BOOTSTRAP_MARKER, &bootstrap, 1)
}

/// JSON for an inline `<script>`: the same escapes as [`javascript_string`],
/// so embedded values cannot close the script element.
pub(crate) fn script_json(value: &serde_json::Value) -> String {
    let mut encoded = String::new();
    for character in value.to_string().chars() {
        match character {
            '<' => encoded.push_str("\\u003c"),
            '>' => encoded.push_str("\\u003e"),
            '&' => encoded.push_str("\\u0026"),
            '\u{2028}' => encoded.push_str("\\u2028"),
            '\u{2029}' => encoded.push_str("\\u2029"),
            other => encoded.push(other),
        }
    }
    encoded
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
