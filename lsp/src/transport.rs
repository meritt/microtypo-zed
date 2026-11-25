use std::io::{self, BufRead, Read, Write};

const MAX_BODY: u64 = 32 * 1024 * 1024;
const MAX_HEADERS: u64 = 4096;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn parse_length(value: &[u8]) -> io::Result<u64> {
    let value = value.trim_ascii();
    if !value.iter().all(u8::is_ascii_digit) {
        return Err(invalid("malformed content length"));
    }
    let length = std::str::from_utf8(value)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .ok_or_else(|| invalid("malformed content length"))?;
    if length > MAX_BODY {
        return Err(invalid("frame too large"));
    }
    Ok(length)
}

pub fn read(input: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut seen = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = input.by_ref().take(MAX_HEADERS - seen).read_until(b'\n', &mut line)? as u64;
        if count == 0 {
            return if seen == 0 { Ok(None) } else { Err(invalid("truncated headers")) };
        }
        seen += count;
        let header = line.strip_suffix(b"\r\n").ok_or_else(|| invalid("malformed header"))?;
        if header.is_empty() {
            break;
        }
        let colon = header.iter().position(|&byte| byte == b':').ok_or_else(|| invalid("malformed header"))?;
        if header[..colon].eq_ignore_ascii_case(b"content-length") {
            if length.is_some() {
                return Err(invalid("duplicate content length"));
            }
            length = Some(parse_length(&header[colon + 1..])?);
        }
    }
    let length = length.ok_or_else(|| invalid("missing content length"))?;
    let mut body = Vec::new();
    let received = input.by_ref().take(length).read_to_end(&mut body)? as u64;
    if received != length {
        return Err(invalid("truncated body"));
    }
    Ok(Some(body))
}

pub fn write(output: &mut impl Write, body: &str) -> io::Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(body.as_bytes())?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn reads_consecutive_frames_then_eof() {
        let mut bytes = frame("{\"a\":1}");
        bytes.extend(frame("[]"));
        let mut input = bytes.as_slice();
        assert_eq!(read(&mut input).unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(read(&mut input).unwrap().unwrap(), b"[]");
        assert_eq!(read(&mut input).unwrap(), None);
    }

    #[test]
    fn accepts_other_headers_in_any_case() {
        let bytes = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(read(&mut bytes.as_slice()).unwrap().unwrap(), b"{}");
    }

    #[test]
    fn rejects_malformed_frames() {
        let cases: [&[u8]; 8] = [
            b"Content-Length: 2\n\n{}",
            b"Content-Length: +2\r\n\r\n{}",
            b"Content-Length: -2\r\n\r\n{}",
            b"Content-Length:\r\n\r\n{}",
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            b"X-Other: 1\r\n\r\n{}",
            b"no colon here\r\n\r\n",
            b"Content-Length: 5\r\n\r\n{}",
        ];
        for bytes in cases {
            assert!(read(&mut &*bytes).is_err(), "{}", String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn rejects_length_above_ceiling_without_reading_it() {
        let bytes = format!("Content-Length: {}\r\n\r\n", MAX_BODY + 1);
        assert!(read(&mut bytes.as_bytes()).is_err());
    }

    #[test]
    fn rejects_unbounded_headers() {
        let bytes = format!("X: {}\r\n\r\n", "a".repeat(usize::try_from(MAX_HEADERS).unwrap()));
        assert!(read(&mut bytes.as_bytes()).is_err());
    }

    #[test]
    fn writes_byte_length_of_utf8_body() {
        let mut output = Vec::new();
        write(&mut output, "\"Амбер\"").unwrap();
        let body = "\"Амбер\"";
        assert_eq!(output, frame(body));
    }
}
