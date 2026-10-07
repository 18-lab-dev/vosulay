use cef::CefStringUserfreeUtf16;

pub(crate) fn units(value: &CefStringUserfreeUtf16) -> &[u16] {
    let raw: Option<&cef::sys::_cef_string_utf16_t> = value.into();
    let Some(raw) = raw else { return &[] };
    if raw.str_.is_null() || raw.length == 0 {
        return &[];
    }
    // CEF owns this userfree string until value is dropped; the returned slice
    // cannot outlive the borrowed owner.
    unsafe { std::slice::from_raw_parts(raw.str_, raw.length) }
}

pub(crate) fn equals(value: &CefStringUserfreeUtf16, expected: &str) -> bool {
    units(value).iter().copied().eq(expected.encode_utf16())
}

pub(crate) fn decode(value: &CefStringUserfreeUtf16, max_bytes: usize) -> Option<String> {
    let units = units(value);
    if units.len() > max_bytes {
        return None;
    }
    let text = String::from_utf16_lossy(units);
    (text.len() <= max_bytes).then_some(text)
}
