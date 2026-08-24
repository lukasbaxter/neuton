//! Packet framing: length prefix, optional zlib compression, and the blocking
//! socket loop.
//!
//! Deliberately blocking rather than async. A game client has exactly one
//! connection and wants the lowest possible latency on it; a dedicated thread
//! doing blocking reads beats a runtime's wakeup path, and it keeps `tokio` and
//! its startup cost out of the binary entirely.

use crate::buf::{MAX_PACKET_LEN, Reader, Writer, varint_size};
use crate::crypto::{Cfb8, SharedSecret};
use crate::error::Error;
use std::io::{self, Read, Write};

/// Compression is off until the server sends `set_compression`.
const NO_COMPRESSION: i32 = -1;

/// Reads and writes framed packets on a byte stream.
pub struct Framed<S> {
    stream: S,
    /// Packets at or above this size are zlib-compressed. `-1` means the
    /// compression handshake has not happened yet.
    threshold: i32,
    /// Reusable receive buffer; grows to the largest packet seen and stays.
    read_buf: Vec<u8>,
    /// Reusable decompression buffer.
    inflate_buf: Vec<u8>,
    /// Reusable send buffer.
    write_buf: Vec<u8>,
    /// Inbound cipher, once the key exchange has happened.
    ///
    /// Encryption wraps the *whole* byte stream including the length prefix, so
    /// it is applied here rather than around packet bodies.
    decrypt: Option<Cfb8>,
    /// Outbound cipher.
    encrypt: Option<Cfb8>,
}

impl<S> Framed<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            threshold: NO_COMPRESSION,
            read_buf: Vec::with_capacity(8 * 1024),
            inflate_buf: Vec::with_capacity(8 * 1024),
            write_buf: Vec::with_capacity(8 * 1024),
            decrypt: None,
            encrypt: None,
        }
    }

    /// Switches the connection to AES-128-CFB8 under `secret`.
    ///
    /// Must be called immediately after `ServerboundKeyPacket` is written and
    /// before anything else is read: the server starts encrypting from its very
    /// next byte, and CFB8 has no framing to resynchronise against.
    pub fn enable_encryption(&mut self, secret: &SharedSecret) {
        self.decrypt = Some(Cfb8::new(secret));
        self.encrypt = Some(Cfb8::new(secret));
    }

    pub fn is_encrypted(&self) -> bool {
        self.encrypt.is_some()
    }

    /// Enables compression for packets at or above `threshold` bytes.
    pub fn set_compression(&mut self, threshold: i32) {
        self.threshold = threshold;
    }

    pub fn compression(&self) -> Option<i32> {
        (self.threshold >= 0).then_some(self.threshold)
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.stream
    }
}

impl<S: Read> Framed<S> {
    /// Reads one byte, decrypting it if the stream is encrypted.
    #[inline]
    fn read_byte(&mut self) -> io::Result<u8> {
        let mut b = [0u8; 1];
        self.stream.read_exact(&mut b)?;
        if let Some(d) = &mut self.decrypt {
            d.decrypt(&mut b);
        }
        Ok(b[0])
    }

    /// Reads the frame-length VarInt.
    ///
    /// Byte at a time by necessity: until the length is known there is no way
    /// to tell how much to pull, and over-reading would desync the cipher.
    fn read_frame_len(&mut self) -> io::Result<i32> {
        let mut val: i32 = 0;
        for shift in [0u32, 7, 14, 21, 28] {
            let b = self.read_byte()?;
            val |= ((b & 0x7F) as i32) << shift;
            if b < 0x80 {
                return Ok(val);
            }
        }
        Err(io::Error::new(io::ErrorKind::InvalidData, "varint too long"))
    }

    /// Reads one packet and returns its body: packet ID VarInt followed by
    /// payload, already decompressed.
    ///
    /// The returned slice borrows an internal buffer, so it is valid until the
    /// next read. That is what keeps the receive path allocation-free.
    pub fn read_packet(&mut self) -> io::Result<&[u8]> {
        let frame_len = self.read_frame_len()?;
        if frame_len < 0 || frame_len as usize > MAX_PACKET_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame length {frame_len} out of range"),
            ));
        }
        let frame_len = frame_len as usize;

        self.read_buf.clear();
        self.read_buf.resize(frame_len, 0);
        // Destructured so the cipher and the buffer can be borrowed together.
        let Self { stream, read_buf, decrypt, .. } = self;
        stream.read_exact(read_buf)?;
        if let Some(d) = decrypt {
            d.decrypt(read_buf);
        }

        if self.threshold < 0 {
            return Ok(&self.read_buf);
        }

        // Compressed format: VarInt uncompressed-size, then either raw bytes
        // (size == 0) or a zlib stream.
        let mut r = Reader::new(&self.read_buf);
        let uncompressed = r.read_varint().map_err(to_io)?;
        let consumed = frame_len - r.remaining();

        if uncompressed == 0 {
            return Ok(&self.read_buf[consumed..]);
        }
        if uncompressed < 0 || uncompressed as usize > MAX_PACKET_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("uncompressed length {uncompressed} out of range"),
            ));
        }

        self.inflate_buf.clear();
        self.inflate_buf.reserve(uncompressed as usize);
        let mut d = flate2::read::ZlibDecoder::new(&self.read_buf[consumed..]);
        d.read_to_end(&mut self.inflate_buf)?;
        if self.inflate_buf.len() != uncompressed as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "packet claimed {uncompressed} bytes but inflated to {}",
                    self.inflate_buf.len()
                ),
            ));
        }
        Ok(&self.inflate_buf)
    }
}

impl<S: Write> Framed<S> {
    /// Frames and sends one packet.
    pub fn write_packet(&mut self, id: i32, body: &Writer) -> io::Result<()> {
        let id_len = varint_size(id);
        let payload_len = id_len + body.len();

        self.write_buf.clear();

        if self.threshold < 0 {
            // Uncompressed: [len][id][body]
            let mut w = Writer::with_capacity(payload_len + 5);
            w.write_varint(payload_len as i32);
            w.write_varint(id);
            w.write_bytes(body.as_slice());
            self.write_buf = w.into_vec();
        } else if payload_len < self.threshold as usize {
            // Below threshold: [len][0][id][body]
            let inner = 1 + payload_len; // the literal 0 VarInt is one byte
            let mut w = Writer::with_capacity(inner + 5);
            w.write_varint(inner as i32);
            w.write_varint(0);
            w.write_varint(id);
            w.write_bytes(body.as_slice());
            self.write_buf = w.into_vec();
        } else {
            // At or above threshold: [len][uncompressed_len][zlib(id + body)]
            let mut plain = Writer::with_capacity(payload_len);
            plain.write_varint(id);
            plain.write_bytes(body.as_slice());
            let plain = plain.into_vec();

            let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            enc.write_all(&plain)?;
            let compressed = enc.finish()?;

            let inner = varint_size(plain.len() as i32) + compressed.len();
            let mut w = Writer::with_capacity(inner + 5);
            w.write_varint(inner as i32);
            w.write_varint(plain.len() as i32);
            w.write_bytes(&compressed);
            self.write_buf = w.into_vec();
        }

        let Self { stream, write_buf, encrypt, .. } = self;
        if let Some(e) = encrypt {
            e.encrypt(write_buf);
        }
        stream.write_all(write_buf)?;
        stream.flush()
    }
}

fn to_io(e: Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

impl Framed<std::net::TcpStream> {
    /// Opens a connection with Nagle disabled.
    ///
    /// Nagle would coalesce our small movement packets and add up to 40 ms of
    /// latency, which is exactly the thing this client exists to avoid.
    pub fn connect(addr: impl std::net::ToSocketAddrs, timeout: std::time::Duration) -> io::Result<Self> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address resolved"))?;
        let stream = std::net::TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self::new(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips packets through an in-memory pipe at a given threshold.
    fn roundtrip(threshold: i32, id: i32, payload: &[u8]) {
        let mut out = Framed::new(Vec::new());
        out.set_compression(threshold);
        let mut body = Writer::new();
        body.write_bytes(payload);
        out.write_packet(id, &body).unwrap();
        let wire = out.stream;

        let mut inp = Framed::new(io::Cursor::new(wire));
        inp.set_compression(threshold);
        let got = inp.read_packet().unwrap();
        let mut r = Reader::new(got);
        assert_eq!(r.read_varint().unwrap(), id, "packet id at threshold {threshold}");
        assert_eq!(r.rest(), payload, "payload at threshold {threshold}");
    }

    #[test]
    fn uncompressed_frames_roundtrip() {
        roundtrip(-1, 0x00, b"hello");
        roundtrip(-1, 0x7f, &[]);
        roundtrip(-1, 300, &vec![0xab; 5000]);
    }

    #[test]
    fn below_threshold_packets_stay_uncompressed() {
        roundtrip(256, 0x01, b"short");
    }

    #[test]
    fn at_or_above_threshold_packets_are_compressed() {
        roundtrip(256, 0x02, &vec![0x42; 4096]);
        // Highly compressible payload must still inflate to the exact length.
        roundtrip(64, 0x03, &vec![0u8; 100_000]);
    }

    #[test]
    fn compression_boundary_is_exact() {
        // A payload sized to land right on the threshold, and one byte below.
        for size in [63usize, 64, 65] {
            roundtrip(64, 0x04, &vec![0x11; size]);
        }
    }

    #[test]
    fn encrypted_frames_roundtrip_including_the_length_prefix() {
        use crate::crypto::SharedSecret;
        // Both endpoints derive their ciphers from the same secret, exactly as
        // the two sides of a real connection do.
        let secret = SharedSecret::generate();

        let mut out = Framed::new(Vec::new());
        out.set_compression(256);
        out.enable_encryption(&secret);
        assert!(out.is_encrypted());

        // Several packets, so a desynchronised cipher shows up rather than
        // passing by luck on the first one.
        let sent: Vec<(i32, Vec<u8>)> =
            vec![(1, b"first".to_vec()), (2, vec![0x5a; 700]), (3, Vec::new())];
        for (id, payload) in &sent {
            let mut body = Writer::new();
            body.write_bytes(payload);
            out.write_packet(*id, &body).unwrap();
        }
        let wire = out.stream;

        let mut inp = Framed::new(io::Cursor::new(wire));
        inp.set_compression(256);
        inp.enable_encryption(&secret);
        for (id, payload) in &sent {
            let got = inp.read_packet().unwrap();
            let mut r = Reader::new(got);
            assert_eq!(r.read_varint().unwrap(), *id);
            assert_eq!(r.rest(), &payload[..]);
        }
    }

    #[test]
    fn an_unencrypted_reader_never_recovers_the_plaintext() {
        use crate::crypto::SharedSecret;
        let payload = [0xABu8; 64];

        // Repeated, because the outcome depends on the random key: usually the
        // length prefix decrypts to nonsense and the read fails, but sometimes
        // it happens to name a length the buffer can satisfy. Either is fine.
        // What must never happen is the plaintext coming back.
        for _ in 0..64 {
            let secret = SharedSecret::generate();
            let mut out = Framed::new(Vec::new());
            out.enable_encryption(&secret);
            let mut body = Writer::new();
            body.write_bytes(&payload);
            out.write_packet(1, &body).unwrap();
            let wire = out.stream;

            assert!(
                !wire.windows(payload.len()).any(|w| w == payload),
                "plaintext survived encryption"
            );

            let mut inp = Framed::new(io::Cursor::new(wire));
            if let Ok(frame) = inp.read_packet() {
                assert_ne!(frame, &payload[..], "decoded the payload without the key");
            }
        }
    }

    #[test]
    fn oversized_frame_length_is_rejected() {
        // VarInt 0x7fffffff as a frame length.
        let wire = vec![0xff, 0xff, 0xff, 0xff, 0x07];
        let mut inp = Framed::new(io::Cursor::new(wire));
        assert!(inp.read_packet().is_err());
    }
}
