//! The text of an HTML document from its bytes, in the encoding a browser
//! picks (HTML's "determining the character encoding", without the guessing
//! some browsers add): a byte order mark, else the `charset` of its HTTP
//! `Content-Type`, else one a `<meta>` in its first 1024 bytes declares, else
//! the default, which WebKit takes as windows-1252. UTF-8, windows-1252 (with
//! the labels the Encoding Standard maps to it, such as `iso-8859-1`) and
//! windows-1251 are decoded; any other encoding is read as UTF-8.

/// The encodings decoded here.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Encoding {
    Utf8,
    Windows1252,
    Windows1251,
}

/// Decodes a document. `content_type` is its HTTP `Content-Type`; with
/// `utf8_by_default` a document that declares nothing is read as UTF-8 (local
/// files, which have no header) instead of windows-1252.
pub fn decode_document(bytes: &[u8], content_type: Option<&str>, utf8_by_default: bool) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    // A `<meta>` past the first 1024 bytes still counts: the parser meets it in the
    // head and starts again in its encoding ("changing the encoding while parsing").
    let declared = content_type
        .and_then(charset_param)
        .and_then(encoding)
        .or_else(|| prescan(&bytes[..bytes.len().min(1024)]))
        .or_else(|| prescan(&bytes[..body_start(bytes)]));
    let fallback = if utf8_by_default { Encoding::Utf8 } else { Encoding::Windows1252 };
    match declared.unwrap_or(fallback) {
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Windows1252 => single_byte(bytes, &WINDOWS_1252),
        Encoding::Windows1251 => single_byte(bytes, &WINDOWS_1251),
    }
}

/// The `charset` parameter of a `Content-Type` value.
fn charset_param(t: &str) -> Option<&str> {
    t.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then(|| v.trim().trim_matches(|c| c == '"' || c == '\''))
    })
}

/// The encoding a label names; `None` for one decoded as nothing here.
fn encoding(label: &str) -> Option<Encoding> {
    let label = label.trim().to_ascii_lowercase();
    Some(match label.as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8" | "x-unicode20utf8" => Encoding::Utf8,
        // A document that declares UTF-16 in ASCII bytes is not UTF-16.
        "utf-16" | "utf-16le" | "utf-16be" => Encoding::Utf8,
        "ansi_x3.4-1968" | "ascii" | "cp1252" | "cp819" | "csisolatin1" | "ibm819" | "iso-8859-1" | "iso-ir-100" | "iso8859-1"
        | "iso88591" | "iso_8859-1" | "iso_8859-1:1987" | "l1" | "latin1" | "us-ascii" | "windows-1252" | "x-cp1252" => Encoding::Windows1252,
        "cp1251" | "windows-1251" | "x-cp1251" => Encoding::Windows1251,
        _ => return None,
    })
}

/// The encoding a `<meta charset>` or `<meta http-equiv=content-type
/// content="...; charset=...">` in `head` declares.
fn prescan(head: &[u8]) -> Option<Encoding> {
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("<meta") {
        let tag = &rest[at + 5..];
        let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
        if let Some(c) = tag.find("charset=") {
            let value = tag[c + 8..].trim_start_matches(['"', '\'', ' ']);
            let end = value.find(|c: char| c == '"' || c == '\'' || c == ';' || c == '/' || c.is_ascii_whitespace()).unwrap_or(value.len());
            if let Some(e) = encoding(&value[..end]) {
                return Some(e);
            }
        }
        rest = &rest[at + 5..];
    }
    None
}

/// Where the `<body` start tag begins, or the end of the document.
fn body_start(bytes: &[u8]) -> usize {
    bytes.windows(5).position(|w| w.eq_ignore_ascii_case(b"<body")).unwrap_or(bytes.len())
}

/// Decodes a single-byte encoding: bytes below 0x80 are ASCII, the others
/// the characters of `high`.
fn single_byte(bytes: &[u8], high: &[u16; 128]) -> String {
    bytes.iter().map(|&b| if b < 0x80 { b as char } else { char::from_u32(high[b as usize - 0x80] as u32).unwrap_or('\u{fffd}') }).collect()
}

/// windows-1252 above 0x7F: Latin-1 but for the printable characters of 0x80-0x9F.
const WINDOWS_1252: [u16; 128] = {
    const C1: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F,
        0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    let mut t = [0u16; 128];
    let mut i = 0;
    while i < 128 {
        t[i] = if i < 32 { C1[i] } else { 0x80 + i as u16 };
        i += 1;
    }
    t
};

/// windows-1251 above 0x7F: Cyrillic, with А-я at 0xC0-0xFF.
const WINDOWS_1251: [u16; 128] = {
    const LOW: [u16; 64] = [
        0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039, 0x040A, 0x040C, 0x040B, 0x040F,
        0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x0098, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F,
        0x00A0, 0x040E, 0x045E, 0x0408, 0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407,
        0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB, 0x0458, 0x0405, 0x0455, 0x0457,
    ];
    let mut t = [0u16; 128];
    let mut i = 0;
    while i < 128 {
        t[i] = if i < 64 { LOW[i] } else { 0x0410 + (i as u16 - 64) };
        i += 1;
    }
    t
};

#[cfg(test)]
mod tests {
    use super::decode_document;

    #[test]
    fn the_encoding_comes_from_the_bom_the_header_a_meta_or_the_default() {
        let cyrillic = b"<meta charset=windows-1251><p>\xcf\xf0\xe8\xe2\xe5\xf2</p>";
        assert_eq!(decode_document(cyrillic, None, false), "<meta charset=windows-1251><p>Привет</p>");
        let latin = b"<p>\xa9 2024 \x93q\x94</p>";
        assert_eq!(decode_document(latin, Some("text/html"), false), "<p>© 2024 “q”</p>", "an undeclared web page is windows-1252");
        assert_eq!(decode_document("<p>©</p>".as_bytes(), None, true), "<p>©</p>", "an undeclared local file stays UTF-8");
        assert_eq!(decode_document("<p>©</p>".as_bytes(), Some("text/html; charset=\"UTF-8\""), false), "<p>©</p>");
        let http_wins = b"<meta http-equiv=Content-Type content='text/html; charset=utf-8'>\xe9";
        assert_eq!(decode_document(http_wins, Some("text/html;charset=ISO-8859-1"), false).chars().last(), Some('é'));
        assert_eq!(decode_document(b"\xEF\xBB\xBF<p>\xc3\xa9</p>", Some("text/html; charset=windows-1251"), false), "<p>é</p>", "a BOM wins");
        assert_eq!(decode_document(b"<meta charset='shift_jis'>x", None, true), "<meta charset='shift_jis'>x");
        let late = format!("<head><script>{}</script><meta charset=utf-8></head><body>\u{e9}", " ".repeat(2000));
        assert_eq!(decode_document(late.as_bytes(), Some("text/html"), false), late, "a meta later in the head still sets the encoding");
        let in_body = format!("<body><p>\u{e9}</p>{}<meta charset=utf-8>", " ".repeat(2000));
        assert!(decode_document(in_body.as_bytes(), Some("text/html"), false).starts_with("<body><p>\u{c3}"), "the search stops at the body");
    }
}
