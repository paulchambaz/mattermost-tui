//! Copying text to the system clipboard through the terminal.

const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let [a, b, c] = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(a) << 16 | u32::from(b) << 8 | u32::from(c);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            let used = match i {
                0 | 1 => true,
                2 => chunk.len() > 1,
                _ => chunk.len() > 2,
            };
            out.push(if used {
                DIGITS[(n >> shift & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// The sequence that asks a terminal to copy `text` to the system clipboard.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_follows_the_rfc_vectors() {
        for (text, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(text.as_bytes()), expected, "{text}");
        }
    }

    #[test]
    fn non_ascii_text_and_the_sequence() {
        assert_eq!(base64("héllo 🎉".as_bytes()), "aMOpbGxvIPCfjok=");
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }
}
