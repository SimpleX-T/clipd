//! Decide what kind of thing a clip is so the UI can render the right preview.

use clipd_proto::ClipKind;

pub fn classify(mime: &str, body: &[u8]) -> ClipKind {
    if mime.starts_with("image/") {
        return ClipKind::Image;
    }

    // Treat everything else as text and inspect the bytes.
    let Ok(s) = std::str::from_utf8(body) else {
        return ClipKind::Text;
    };
    let trimmed = s.trim();

    if is_hex_color(trimmed) {
        return ClipKind::HexColor;
    }
    if is_url(trimmed) {
        return ClipKind::Url;
    }
    if looks_like_json(trimmed) {
        return ClipKind::Json;
    }
    if looks_like_code(trimmed) {
        return ClipKind::Code;
    }
    ClipKind::Text
}

/// `#abc` or `#aabbcc` or `#aabbccff` (optional alpha).
fn is_hex_color(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('#') else { return false };
    matches!(rest.len(), 3 | 4 | 6 | 8) && rest.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_url(s: &str) -> bool {
    // Cheap heuristic — no full RFC 3986 parser. Has to start with a scheme.
    let lower = s.to_ascii_lowercase();
    (lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("file://")
        || lower.starts_with("ssh://")
        || lower.starts_with("git://"))
        && !s.contains(char::is_whitespace)
}

fn looks_like_json(s: &str) -> bool {
    let first = s.chars().next();
    let last = s.chars().last();
    matches!((first, last), (Some('{'), Some('}')) | (Some('['), Some(']')))
        && serde_json::from_str::<serde_json::Value>(s).is_ok()
}

/// Heuristic — if the snippet contains common code tokens AND newlines, call it code.
fn looks_like_code(s: &str) -> bool {
    if !s.contains('\n') {
        return false;
    }
    let needles = [
        "fn ", "def ", "function ", "class ", "import ", "const ", "let ",
        "var ", "#include", "package ", "use ", "::", "=>", "->",
        "public ", "private ", "static ", "void ", "return ",
    ];
    let lower = s.to_ascii_lowercase();
    needles.iter().filter(|n| lower.contains(*n)).count() >= 1
}

/// Short, single-line representation for the UI list. Drops control chars,
/// collapses runs of whitespace, hard-caps at `max` chars.
pub fn make_preview(kind: ClipKind, mime: &str, body: &[u8], max: usize) -> String {
    if kind == ClipKind::Image {
        return format!("Image ({}, {} KB)", mime, body.len() / 1024);
    }
    let s = String::from_utf8_lossy(body);
    let mut out = String::with_capacity(max.min(s.len()));
    let mut last_was_space = false;
    for ch in s.chars() {
        if out.chars().count() >= max {
            out.push('…');
            break;
        }
        if ch.is_control() || ch.is_whitespace() {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_url() {
        assert_eq!(classify("text/plain", b"https://example.com"), ClipKind::Url);
    }

    #[test]
    fn classify_hex() {
        assert_eq!(classify("text/plain", b"#7a3fb1"), ClipKind::HexColor);
        assert_eq!(classify("text/plain", b"#abc"), ClipKind::HexColor);
        assert_eq!(classify("text/plain", b"#abcd"), ClipKind::HexColor);
        assert_eq!(classify("text/plain", b"#aabbccff"), ClipKind::HexColor);
        assert_ne!(classify("text/plain", b"#zzz"), ClipKind::HexColor);
    }

    #[test]
    fn classify_json() {
        assert_eq!(classify("text/plain", b"{\"a\":1}"), ClipKind::Json);
        assert_ne!(classify("text/plain", b"{not json}"), ClipKind::Json);
    }

    #[test]
    fn classify_image() {
        assert_eq!(classify("image/png", &[0u8; 16]), ClipKind::Image);
    }

    #[test]
    fn preview_collapses_whitespace() {
        let p = make_preview(ClipKind::Text, "text/plain", b"hello\n\nworld\t!", 80);
        assert_eq!(p, "hello world !");
    }
}
