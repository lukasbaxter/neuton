//! Packet framing: length prefix, optional zlib compression, and the blocking
//! socket loop.
//!
//! Deliberately blocking rather than async. A game client has exactly one
//! connection and wants the lowest possible latency on it; a dedicated thread
//! doing blocking reads beats a runtime's wakeup path, and it keeps `tokio` and
//! its startup cost out of the binary entirely.

use crate::buf::{MAX_PACKET_LEN, Reader, Writer, varint_size};
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
}

impl<S> Framed<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            threshold: NO_COMPRESSION,
            read_buf: Vec::with_capacity(8 * 1024),
            inflate_buf: Vec::with_capacity(8 * 1024),
            write_buf: Vec::with_capacity(8 * 1024),
        }
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

/// Reads a VarInt one byte at a time straight off the stream.
///
/// Only used for the frame length, where we cannot buffer ahead without
/// knowing how much to read.
fn read_varint_from<R: Read>(r: &mut R) -> io::Result<i32> {
    let mut val: i32 = 0;
    for shift in [0u32, 7, 14, 21, 28] {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        val |= ((b[0] & 0x7F) as i32) << shift;
        if b[0] < 0x80 {
            return Ok(val);
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "varint too long"))
}

impl<S: Read> Framed<S> {
    /// Reads one packet and returns its body: packet ID VarInt followed by
    /// payload, already decompressed.
    ///
    /// The returned slice borrows an internal buffer, so it is valid until the
    /// next read. That is what keeps the receive path allocation-free.
    pub fn read_packet(&mut self) -> io::Result<&[u8]> {
        let frame_len = read_varint_from(&mut self.stream)?;
        if frame_len < 0 || frame_len as usize > MAX_PACKET_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame length {frame_len} out of range"),
            ));
        }
        let frame_len = frame_len as usize;

        self.read_buf.clear();
        self.read_buf.resize(frame_len, 0);
        self.stream.read_exact(&mut self.read_buf)?;

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

        self.stream.write_all(&self.write_buf)?;
        self.stream.flush()
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
    fn oversized_frame_length_is_rejected() {
        // VarInt 0x7fffffff as a frame length.
        let wire = vec![0xff, 0xff, 0xff, 0xff, 0x07];
        let mut inp = Framed::new(io::Cursor::new(wire));
        assert!(inp.read_packet().is_err());
    }
}
