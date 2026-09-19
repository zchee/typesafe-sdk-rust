// The check every test of a rendering of text this SDK did not write shares.
// Each test module that needs it pastes it in with `include!`: one `mod` per
// includer would load this file as a module several times, and a single
// shared module would need a declaration in library code.

/// No byte a terminal or a log reader would act on, and none of the
/// characters that hide or reorder text.
fn assert_printable(rendered: &str) {
    assert!(
        !rendered.bytes().any(|byte| byte < 0x20 || byte == 0x7f),
        "a control byte reached the text: {rendered:?}"
    );
    for hidden in ['\u{202e}', '\u{2066}', '\u{200b}', '\u{feff}', '\u{2028}', '\u{85}'] {
        assert!(!rendered.contains(hidden), "{hidden:?} reached the text: {rendered:?}");
    }
}
