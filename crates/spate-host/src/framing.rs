//! Chrome native messaging framing: a 4-byte little-endian length followed by UTF-8 JSON.

use std::io::{self, ErrorKind, Read, Write};

/// Chrome refuses host responses larger than 1 MB.
pub const MAX_RESPONSE: usize = 1024 * 1024;
/// Chrome allows up to 64 MiB extension → host; anything bigger is a protocol error.
pub const MAX_REQUEST: usize = 64 * 1024 * 1024;

/// Reads one message. `Ok(None)` means stdin closed cleanly between messages.
pub fn read_message<R: Read>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    // Distinguish clean EOF (0 bytes) from a truncated length prefix.
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "truncated length prefix",
                ))
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_REQUEST {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("message too large: {len} bytes"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Writes one message and flushes. Fails (writing nothing) if it exceeds `MAX_RESPONSE`.
pub fn write_message<W: Write>(w: &mut W, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_RESPONSE {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "response exceeds 1 MB",
        ));
    }
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(body)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn roundtrip() {
        let mut out = Vec::new();
        write_message(&mut out, br#"{"type":"ping"}"#).unwrap();
        assert_eq!(&out[..4], &[15, 0, 0, 0]);
        let msg = read_message(&mut Cursor::new(out)).unwrap().unwrap();
        assert_eq!(msg, br#"{"type":"ping"}"#);
    }

    #[test]
    fn reads_several_then_eof() {
        let mut input = frame(b"{}");
        input.extend(frame(br#"{"a":1}"#));
        let mut c = Cursor::new(input);
        assert_eq!(read_message(&mut c).unwrap().unwrap(), b"{}");
        assert_eq!(read_message(&mut c).unwrap().unwrap(), br#"{"a":1}"#);
        assert!(read_message(&mut c).unwrap().is_none());
    }

    #[test]
    fn empty_input_is_clean_eof() {
        assert!(read_message(&mut Cursor::new(Vec::<u8>::new()))
            .unwrap()
            .is_none());
    }

    #[test]
    fn truncated_prefix_and_body_are_errors() {
        assert!(read_message(&mut Cursor::new(vec![5u8, 0])).is_err());
        assert!(read_message(&mut Cursor::new(vec![5u8, 0, 0, 0, b'{'])).is_err());
    }

    #[test]
    fn handles_short_reads() {
        // A reader that returns one byte at a time, like a slow pipe.
        struct OneByte(Cursor<Vec<u8>>);
        impl Read for OneByte {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = buf.len().min(1);
                self.0.read(&mut buf[..n])
            }
        }
        let mut r = OneByte(Cursor::new(frame(br#"{"type":"ping"}"#)));
        assert_eq!(
            read_message(&mut r).unwrap().unwrap(),
            br#"{"type":"ping"}"#
        );
    }

    #[test]
    fn rejects_oversized() {
        let huge = ((MAX_REQUEST + 1) as u32).to_le_bytes().to_vec();
        assert!(read_message(&mut Cursor::new(huge)).is_err());
        let mut out = Vec::new();
        assert!(write_message(&mut out, &vec![b' '; MAX_RESPONSE + 1]).is_err());
        assert!(out.is_empty());
        write_message(&mut out, &vec![b' '; MAX_RESPONSE]).unwrap();
    }
}
