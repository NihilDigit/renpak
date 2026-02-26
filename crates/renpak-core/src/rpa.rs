//! RPA-3.0 archive reader and writer.
//!
//! Format:
//! - 40-byte header: `RPA-3.0 {index_offset:016x} {key:08x}\n` + padding
//! - Data region: file contents laid out sequentially
//! - Index region: zlib(pickle(dict[str, list[tuple[int, int, bytes]]]))

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use serde_pickle::DeOptions;

/// A single entry in an RPA archive.
#[derive(Debug, Clone)]
pub struct RpaEntry {
    pub name: String,
    pub offset: u64,
    pub length: u64,
    pub prefix: Vec<u8>,
}

/// Reader for RPA-3.0 archives. Supports concurrent pread access.
pub struct RpaReader {
    file: File,
    key: u64,
    index_offset: u64,
}

impl RpaReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let mut header = [0u8; 40];
        file.read_exact(&mut header)?;

        let header_str = std::str::from_utf8(&header)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if !header_str.starts_with("RPA-3.0 ") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Not an RPA-3.0 archive",
            ));
        }

        let index_offset =
            u64::from_str_radix(header_str[8..24].trim(), 16).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("bad offset: {e}"))
            })?;
        let key = u64::from_str_radix(header_str[25..33].trim(), 16).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("bad key: {e}"))
        })?;

        Ok(Self { file, key, index_offset })
    }

    /// Parse the RPA index. Returns a map of filename → RpaEntry.
    pub fn read_index(&mut self) -> io::Result<HashMap<String, RpaEntry>> {
        self.file.seek(SeekFrom::Start(self.index_offset))?;
        let mut compressed = Vec::new();
        self.file.read_to_end(&mut compressed)?;

        let mut decompressed = Vec::new();
        ZlibDecoder::new(&compressed[..]).read_to_end(&mut decompressed)?;

        // Parse pickle: dict[str, list[tuple[int, int] | tuple[int, int, bytes]]]
        let raw: HashMap<String, Vec<Vec<serde_pickle::Value>>> =
            serde_pickle::from_slice(&decompressed, DeOptions::default()).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("pickle: {e}"))
            })?;

        let mut entries = HashMap::with_capacity(raw.len());
        for (name, tuples) in raw {
            if tuples.is_empty() {
                continue;
            }
            let t = &tuples[0];
            let (offset_raw, length_raw, prefix) = match t.len() {
                2 => (
                    pickle_to_u64(&t[0])?,
                    pickle_to_u64(&t[1])?,
                    Vec::new(),
                ),
                3 => (
                    pickle_to_u64(&t[0])?,
                    pickle_to_u64(&t[1])?,
                    pickle_to_bytes(&t[2]),
                ),
                _ => continue,
            };
            let offset = offset_raw ^ self.key;
            let length = length_raw ^ self.key;
            entries.insert(
                name.clone(),
                RpaEntry { name, offset, length, prefix },
            );
        }
        Ok(entries)
    }

    /// Get a reference to the underlying file (for pread sharing).
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Get the XOR key.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// Read file data at the given offset+length using pread (thread-safe).
    pub fn read_file_at(&self, entry: &RpaEntry) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; entry.length as usize];
        self.file.read_exact_at(&mut buf, entry.offset)?;
        if !entry.prefix.is_empty() {
            let mut full = Vec::with_capacity(entry.prefix.len() + buf.len());
            full.extend_from_slice(&entry.prefix);
            full.extend_from_slice(&buf);
            Ok(full)
        } else {
            Ok(buf)
        }
    }
}

fn pickle_to_u64(val: &serde_pickle::Value) -> io::Result<u64> {
    match val {
        serde_pickle::Value::I64(n) => Ok(*n as u64),
        serde_pickle::Value::Int(n) => {
            // BigInt — try to convert
            use std::convert::TryFrom;
            let v = i64::try_from(n.clone()).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("int too large: {e}"))
            })?;
            Ok(v as u64)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected int, got {:?}", val),
        )),
    }
}

fn pickle_to_bytes(val: &serde_pickle::Value) -> Vec<u8> {
    match val {
        serde_pickle::Value::Bytes(b) => b.clone(),
        serde_pickle::Value::None => Vec::new(),
        serde_pickle::Value::String(s) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    }
}
/// Writer for RPA-3.0 archives.
pub struct RpaWriter {
    file: BufWriter<File>,
    key: u64,
    entries: Vec<(String, u64, u64)>, // (name, offset, length)
}

impl RpaWriter {
    pub fn create(path: &Path, key: u64) -> io::Result<Self> {
        let file = BufWriter::new(File::create(path)?);
        let mut w = Self {
            file,
            key,
            entries: Vec::new(),
        };
        // Reserve 40 bytes for header
        w.file.write_all(&[0u8; 40])?;
        Ok(w)
    }

    pub fn add_file(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let offset = self.file.stream_position()?;
        self.file.write_all(data)?;
        self.entries.push((name.to_string(), offset, data.len() as u64));
        Ok(())
    }

    /// Copy raw bytes from a source file at the given offset+length.
    pub fn add_file_from(
        &mut self,
        name: &str,
        src: &File,
        src_offset: u64,
        src_length: u64,
        prefix: &[u8],
    ) -> io::Result<()> {
        let offset = self.file.stream_position()?;
        if !prefix.is_empty() {
            self.file.write_all(prefix)?;
        }
        // Read from source using pread and write in chunks
        let mut remaining = src_length as usize;
        let mut src_pos = src_offset;
        let mut chunk = vec![0u8; 256 * 1024]; // 256KB chunks
        while remaining > 0 {
            let to_read = remaining.min(chunk.len());
            src.read_exact_at(&mut chunk[..to_read], src_pos)?;
            self.file.write_all(&chunk[..to_read])?;
            src_pos += to_read as u64;
            remaining -= to_read;
        }
        let total_len = prefix.len() as u64 + src_length;
        self.entries.push((name.to_string(), offset, total_len));
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let index_offset = self.file.stream_position()?;

        // Build pickle-compatible index
        let mut index: HashMap<String, Vec<(i64, i64, Vec<u8>)>> = HashMap::new();
        for (name, offset, length) in &self.entries {
            index.insert(
                name.clone(),
                vec![((offset ^ self.key) as i64, (length ^ self.key) as i64, vec![])],
            );
        }

        let pickled = serde_pickle::to_vec(&index, Default::default())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("pickle: {e}")))?;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&pickled)?;
        let compressed = encoder.finish()?;
        self.file.write_all(&compressed)?;

        // Write header
        let header = format!("RPA-3.0 {:016x} {:08x}\n", index_offset, self.key);
        let mut header_bytes = [0u8; 40];
        let hb = header.as_bytes();
        header_bytes[..hb.len()].copy_from_slice(hb);

        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header_bytes)?;
        self.file.flush()?;
        Ok(())
    }
}
