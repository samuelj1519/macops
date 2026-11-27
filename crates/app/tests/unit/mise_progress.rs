#![cfg(test)]
use super::*;

#[test]
fn redraw_stays_in_owned_rows_and_ignores_other_terminal_commands() {
    let mut renderer = Renderer {
        color: true,
        ..Renderer::default()
    };
    assert_eq!(renderer.push(b"\x1b[1Aone\n\x1b[1"), "one\n");
    assert_eq!(
        renderer.push(b"A\x1b[2Ktwo\x1b]52;abc"),
        "\x1b[1A\x1b[2Ktwo"
    );
    assert_eq!(
        renderer.push(b"xyz\x1b\\\x1b[?25l\x1b[999A\x1b[2J\x1b[32mend"),
        "\x1b[32mend"
    );
}

#[test]
fn redraw_preserves_partial_utf8_and_drops_unsafe_escapes_in_plain_mode() {
    let mut renderer = Renderer::default();
    let glyph = "汉".as_bytes();
    assert_eq!(renderer.push(&glyph[..2]), "");
    assert_eq!(renderer.push(&glyph[2..]), "汉");
    assert_eq!(renderer.push(b"\x1b[31mplain\x1b]skip\x07"), "plain");
    assert_eq!(renderer.push(b"\x1b[1A\x1bQtail"), "tail");
    assert_eq!(
        renderer.push("start\u{202e}\u{2066}end\t\r".as_bytes()),
        "startend\r"
    );
    assert_eq!(
        renderer.push(b"\x1b[2?K\x1b[11111111111111111111111111111111111mend"),
        "end"
    );
}
