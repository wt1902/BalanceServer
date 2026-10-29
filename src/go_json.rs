//! Compatibility with encoding/json's replacement of malformed Unicode in
//! decoded JSON strings. Syntax errors remain errors for the JSON decoder.
//!
//! Apply this only after enforcing limits on the original request bytes.
//! RawMessage/RawValue fields must retain their original input separately:
//! Go does not replace malformed Unicode inside a RawMessage.

/// Replace malformed UTF-8 and unpaired UTF-16 escapes inside JSON strings.
/// Valid bytes, whitespace and escape spellings are preserved exactly.
pub fn normalize(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    let mut in_string = false;
    while index < input.len() {
        let byte = input[index];
        if !in_string {
            output.push(byte);
            in_string = byte == b'"';
            index += 1;
            continue;
        }
        match byte {
            b'"' => {
                output.push(byte);
                in_string = false;
                index += 1;
            }
            b'\\' => {
                if let Some(code) = unicode_escape(&input[index..]) {
                    if (0xD800..=0xDBFF).contains(&code) {
                        if unicode_escape(&input[index + 6..])
                            .is_some_and(|low| (0xDC00..=0xDFFF).contains(&low))
                        {
                            output.extend_from_slice(&input[index..index + 12]);
                            index += 12;
                            continue;
                        }
                        output.extend_from_slice(b"\\uFFFD");
                    } else if (0xDC00..=0xDFFF).contains(&code) {
                        output.extend_from_slice(b"\\uFFFD");
                    } else {
                        output.extend_from_slice(&input[index..index + 6]);
                    }
                    index += 6;
                } else {
                    // Skip an escaped quote/backslash as one unit; invalid
                    // escape syntax is left intact for serde to reject.
                    let end = (index + 2).min(input.len());
                    output.extend_from_slice(&input[index..end]);
                    index = end;
                }
            }
            0..=0x7f => {
                output.push(byte);
                index += 1;
            }
            _ => {
                let length = match byte {
                    0xC2..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF4 => 4,
                    _ => 0,
                };
                if length != 0
                    && index + length <= input.len()
                    && std::str::from_utf8(&input[index..index + length]).is_ok()
                {
                    output.extend_from_slice(&input[index..index + length]);
                    index += length;
                } else {
                    // Go's utf8.DecodeRune consumes one byte for every error,
                    // including each byte of an incomplete sequence.
                    output.extend_from_slice("\u{FFFD}".as_bytes());
                    index += 1;
                }
            }
        }
    }
    output
}

fn unicode_escape(input: &[u8]) -> Option<u16> {
    if input.len() < 6 || &input[..2] != b"\\u" {
        return None;
    }
    input[2..6].iter().try_fold(0u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        Some(value * 16 + u16::from(digit))
    })
}

/// Replace named raw fields with numbered JSON strings before scalar decoding.
/// The returned raw values retain the original byte spelling and whitespace.
pub fn extract_raw_fields(input: &[u8], field: &str) -> Result<(Vec<u8>, Vec<Vec<u8>>), ()> {
    let start = skip_space(input, 0);
    let end = value_end(input, start)?;
    let mut output = Vec::with_capacity(input.len());
    let mut raw = Vec::new();
    let mut copied = 0;
    let mut index = start;
    while index < end {
        if input[index] != b'"' {
            index += 1;
            continue;
        }
        let string_end = quoted_end(input, index)?;
        let colon = skip_space(input, string_end);
        if input.get(colon) == Some(&b':') {
            let key: String =
                serde_json::from_slice(&normalize(&input[index..string_end])).map_err(|_| ())?;
            if key.to_ascii_lowercase().replace('ſ', "s") == field {
                let value_start = skip_space(input, colon + 1);
                let value_end = value_end(input, value_start)?;
                let bytes = &input[value_start..value_end];
                // Validate raw syntax without converting numbers or strings.
                serde_json::from_slice::<Box<serde_json::value::RawValue>>(&normalize(bytes))
                    .map_err(|_| ())?;
                output.extend_from_slice(&input[copied..value_start]);
                output.extend_from_slice(format!("\"{}\"", raw.len()).as_bytes());
                raw.push(bytes.to_vec());
                copied = value_end;
                index = value_end;
                continue;
            }
        }
        index = string_end;
    }
    output.extend_from_slice(&input[copied..]);
    Ok((output, raw))
}

fn skip_space(input: &[u8], mut index: usize) -> usize {
    while input
        .get(index)
        .is_some_and(|b| matches!(b, b' ' | b'\r' | b'\n' | b'\t'))
    {
        index += 1;
    }
    index
}

fn quoted_end(input: &[u8], mut index: usize) -> Result<usize, ()> {
    index += 1;
    while index < input.len() {
        match input[index] {
            b'"' => return Ok(index + 1),
            b'\\' => index += 2,
            _ => index += 1,
        }
    }
    Err(())
}

fn value_end(input: &[u8], start: usize) -> Result<usize, ()> {
    match input.get(start).ok_or(())? {
        b'"' => quoted_end(input, start),
        b'[' | b'{' => {
            let mut depth = 1usize;
            let mut index = start + 1;
            while index < input.len() {
                match input[index] {
                    b'"' => {
                        index = quoted_end(input, index)?;
                        continue;
                    }
                    b'[' | b'{' => {
                        depth += 1;
                        if depth > 10_000 {
                            return Err(());
                        }
                    }
                    b']' | b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(index + 1);
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            Err(())
        }
        _ => {
            let length = input[start..]
                .iter()
                .position(|b| matches!(b, b' ' | b'\r' | b'\n' | b'\t' | b',' | b']' | b'}'))
                .unwrap_or(input.len() - start);
            if length == 0 {
                Err(())
            } else {
                Ok(start + length)
            }
        }
    }
}

/// Go's encoding/json compacts RawMessage and escapes HTML-sensitive bytes,
/// preserving duplicate keys, number spellings and malformed Unicode bytes.
pub fn compact_raw(input: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0;
    while index < input.len() {
        let byte = input[index];
        if !in_string && matches!(byte, b' ' | b'\n' | b'\r' | b'\t') {
            index += 1;
            continue;
        }
        if in_string && !escaped {
            let replacement: Option<&[u8]> = match byte {
                b'<' => Some(b"\\u003c"),
                b'>' => Some(b"\\u003e"),
                b'&' => Some(b"\\u0026"),
                _ => None,
            };
            if let Some(value) = replacement {
                result.extend_from_slice(value);
                index += 1;
                continue;
            }
            if input[index..].starts_with(&[0xE2, 0x80, 0xA8])
                || input[index..].starts_with(&[0xE2, 0x80, 0xA9])
            {
                result.extend_from_slice(if input[index + 2] == 0xA8 {
                    b"\\u2028"
                } else {
                    b"\\u2029"
                });
                index += 3;
                continue;
            }
        }
        result.push(byte);
        if escaped {
            escaped = false;
        } else if in_string && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            in_string = !in_string;
        }
        index += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_valid_json_and_literal_backslashes() {
        let input =
            br#" {"escaped":"\\ud800", "pair":"\ud83d\ude00", "quote":"\"", "slash":"\\"} "#;
        assert_eq!(normalize(input), input);
        serde_json::from_slice::<serde_json::Value>(&normalize(input)).unwrap();
        let unicode = "{\"text\":\"Привет 🌍\"}".as_bytes();
        assert_eq!(normalize(unicode), unicode);
    }

    #[test]
    fn replaces_unpaired_surrogates_without_consuming_following_escapes() {
        let input = br#"["\ud800", "\udc00", "\ud800\u0061", "\ud800\ud800\udc00"]"#;
        let value: Vec<String> = serde_json::from_slice(&normalize(input)).unwrap();
        assert_eq!(value, ["�", "�", "�a", "�𐀀"]);
    }

    #[test]
    fn replaces_each_malformed_utf8_byte_as_go_does() {
        for (input, expected) in [
            (b"\"\xff\"".as_slice(), "�"),
            (b"\"\xe1\x80\"".as_slice(), "��"),
            (b"\"\xed\xa0\x80\"".as_slice(), "���"),
            (b"\"\xf4\x90\x80\x80\"".as_slice(), "����"),
        ] {
            let decoded: String = serde_json::from_slice(&normalize(input)).unwrap();
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn leaves_invalid_json_syntax_rejected() {
        for input in [
            br#""\uZZZZ""#.as_slice(),
            br#""\x""#.as_slice(),
            b"\"raw\nnewline\"".as_slice(),
            b"{\xff:1}".as_slice(),
        ] {
            assert!(serde_json::from_slice::<serde_json::Value>(&normalize(input)).is_err());
        }
    }

    #[test]
    fn raw_extraction_preserves_bytes_and_ignores_trailing_json() {
        let input = b"{\"payload\": { \"x\":\"\\ud800\", \"x\":\"\xff\" },\"PAYLOAD\":null} {\"payload\":not-json}";
        let (masked, values) = extract_raw_fields(input, "payload").unwrap();
        assert_eq!(
            values,
            [
                b"{ \"x\":\"\\ud800\", \"x\":\"\xff\" }".to_vec(),
                b"null".to_vec()
            ]
        );
        assert_eq!(
            masked,
            b"{\"payload\": \"0\",\"PAYLOAD\":\"1\"} {\"payload\":not-json}"
        );
        assert!(extract_raw_fields(br#"{"payload":{"bad":}}"#, "payload").is_err());
        assert!(extract_raw_fields(br#"{"payload":[1,]}"#, "payload").is_err());
    }

    #[test]
    fn raw_compaction_matches_go_html_escaping_without_decoding_strings() {
        let input = "{ \"raw\":\"<&>\u{2028}\u{2029}\", \"escaped\":\"\\u003c\", \"dup\":1, \"dup\":2, \"n\":1.00e+999 }";
        assert_eq!(
            compact_raw(input.as_bytes()),
            br#"{"raw":"\u003c\u0026\u003e\u2028\u2029","escaped":"\u003c","dup":1,"dup":2,"n":1.00e+999}"#
        );
    }
}
