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
