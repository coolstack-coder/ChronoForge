//! `.cfrec` — the on-disk trace format.
//!
//! ```text
//! "CFORGE01"
//! frame*      [len:u32][crc32:u32][kind:u8][payload: lz4(bincode)]
//! index       a Index frame listing every other frame
//! footer      [index_offset:u64]["CFEND\0\0\0"]
//! ```
//!
//! Step events are chunked so a reader can jump to step N without
//! decompressing the whole file; the index maps chunk -> first step.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::event::{Recording, Step, StepEvent};
use crate::TraceError;

const MAGIC: &[u8; 8] = b"CFORGE01";
const FOOTER: &[u8; 8] = b"CFEND\0\0\0";
pub const STEP_CHUNK: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
enum FrameKind {
    Recording = 1,
    Steps = 2,
    Index = 3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct IndexEntry {
    kind: FrameKind,
    offset: u64,
    first_step: Step,
    count: u32,
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, TraceError> {
    let raw = bincode::serialize(value)?;
    Ok(lz4_flex::compress_prepend_size(&raw))
}

fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, TraceError> {
    let raw = lz4_flex::decompress_size_prepended(payload)
        .map_err(|e| TraceError::Corrupt(format!("lz4: {e}")))?;
    Ok(bincode::deserialize(&raw)?)
}

struct FrameWriter<W: Write + Seek> {
    out: W,
    index: Vec<IndexEntry>,
}

impl<W: Write + Seek> FrameWriter<W> {
    fn frame(
        &mut self,
        kind: FrameKind,
        payload: &[u8],
        first_step: Step,
        count: u32,
    ) -> io::Result<()> {
        let offset = self.out.stream_position()?;
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&[kind as u8]);
        hasher.update(payload);
        self.out.write_all(&(payload.len() as u32).to_le_bytes())?;
        self.out.write_all(&hasher.finalize().to_le_bytes())?;
        self.out.write_all(&[kind as u8])?;
        self.out.write_all(payload)?;
        if kind != FrameKind::Index {
            self.index.push(IndexEntry {
                kind,
                offset,
                first_step,
                count,
            });
        }
        Ok(())
    }
}

/// Writes a recording, optionally with the fully instrumented step stream.
/// Without steps a file is usually a few KB; replay regenerates them.
pub fn write_recording(
    path: &Path,
    rec: &Recording,
    steps: Option<&[StepEvent]>,
) -> Result<u64, TraceError> {
    let file = File::create(path)?;
    let mut w = FrameWriter {
        out: BufWriter::new(file),
        index: Vec::new(),
    };
    w.out.write_all(MAGIC)?;
    w.frame(FrameKind::Recording, &encode(rec)?, 0, 0)?;
    if let Some(steps) = steps {
        for chunk in steps.chunks(STEP_CHUNK) {
            let first = chunk.first().map(|s| s.step).unwrap_or(0);
            w.frame(
                FrameKind::Steps,
                &encode(&chunk.to_vec())?,
                first,
                chunk.len() as u32,
            )?;
        }
    }
    let index_offset = w.out.stream_position()?;
    let index = encode(&w.index)?;
    w.frame(FrameKind::Index, &index, 0, 0)?;
    w.out.write_all(&index_offset.to_le_bytes())?;
    w.out.write_all(FOOTER)?;
    w.out.flush()?;
    Ok(w.out.stream_position()?)
}

pub struct TraceReader {
    file: File,
    index: Vec<IndexEntry>,
}

impl TraceReader {
    pub fn open(path: &Path) -> Result<Self, TraceError> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(TraceError::Corrupt(
                "not a ChronoForge recording (bad magic)".into(),
            ));
        }
        file.seek(SeekFrom::End(-16))?;
        let mut tail = [0u8; 16];
        file.read_exact(&mut tail)?;
        if &tail[8..] != FOOTER {
            return Err(TraceError::Corrupt(
                "truncated recording (no footer)".into(),
            ));
        }
        let index_offset = u64::from_le_bytes(tail[..8].try_into().unwrap());
        let mut reader = TraceReader {
            file,
            index: Vec::new(),
        };
        let (kind, payload) = reader.read_frame(index_offset)?;
        if kind != FrameKind::Index as u8 {
            return Err(TraceError::Corrupt(
                "footer does not point at an index".into(),
            ));
        }
        reader.index = decode(&payload)?;
        Ok(reader)
    }

    fn read_frame(&mut self, offset: u64) -> Result<(u8, Vec<u8>), TraceError> {
        self.file.seek(SeekFrom::Start(offset))?;
        let mut head = [0u8; 9];
        self.file.read_exact(&mut head)?;
        let len = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(head[4..8].try_into().unwrap());
        let kind = head[8];
        let mut payload = vec![0u8; len];
        self.file.read_exact(&mut payload)?;
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&[kind]);
        hasher.update(&payload);
        if hasher.finalize() != crc {
            return Err(TraceError::Corrupt(format!(
                "checksum mismatch in frame at {offset}"
            )));
        }
        Ok((kind, payload))
    }

    pub fn recording(&mut self) -> Result<Recording, TraceError> {
        let entry = self
            .index
            .iter()
            .find(|e| e.kind == FrameKind::Recording)
            .ok_or_else(|| TraceError::Corrupt("no recording frame".into()))?
            .offset;
        let (_, payload) = self.read_frame(entry)?;
        decode(&payload)
    }

    pub fn has_steps(&self) -> bool {
        self.index.iter().any(|e| e.kind == FrameKind::Steps)
    }

    /// Steps in `[from, to)`, touching only the chunks that overlap.
    pub fn steps(&mut self, from: Step, to: Step) -> Result<Vec<StepEvent>, TraceError> {
        let chunks: Vec<u64> = self
            .index
            .iter()
            .filter(|e| e.kind == FrameKind::Steps)
            .filter(|e| e.first_step < to && e.first_step + e.count as u64 > from)
            .map(|e| e.offset)
            .collect();
        let mut out = Vec::new();
        for offset in chunks {
            let (_, payload) = self.read_frame(offset)?;
            let chunk: Vec<StepEvent> = decode(&payload)?;
            out.extend(chunk.into_iter().filter(|s| s.step >= from && s.step < to));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::*;
    use crate::value::{Loc, Val};

    fn sample(n: u64) -> (Recording, Vec<StepEvent>) {
        let rec = Recording {
            meta: TraceMeta {
                name: "t".into(),
                source: "print(1);".into(),
                ..Default::default()
            },
            nondet: vec![NondetEvent::Rand { step: 3, value: 42 }],
            outcome: Outcome::Completed,
            total_steps: n,
            output: vec![],
        };
        let steps = (0..n)
            .map(|i| StepEvent {
                step: i,
                tid: (i % 2) as u32,
                func: 0,
                pc: i as u32,
                line: 1,
                op: OpTag::Store,
                reads: vec![],
                writes: vec![crate::event::Write {
                    loc: Loc::Global(0),
                    old: Val::Int(i as i64),
                    new: Val::Int(i as i64 + 1),
                }],
                deps: vec![],
                ctrl: None,
                sync: None,
                nondet: None,
                result: None,
            })
            .collect();
        (rec, steps)
    }

    #[test]
    fn roundtrip_with_random_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.cfrec");
        let (rec, steps) = sample(5000);
        write_recording(&path, &rec, Some(&steps)).unwrap();

        let mut r = TraceReader::open(&path).unwrap();
        assert_eq!(r.recording().unwrap(), rec);
        let window = r.steps(2040, 2060).unwrap();
        assert_eq!(window.len(), 20);
        assert_eq!(window[0], steps[2040]);
        assert_eq!(window.last().unwrap(), &steps[2059]);
    }

    #[test]
    fn detects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.cfrec");
        let (rec, steps) = sample(100);
        write_recording(&path, &rec, Some(&steps)).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[30] ^= 0xff; // somewhere inside the first frame's payload
        std::fs::write(&path, &bytes).unwrap();
        let err = TraceReader::open(&path)
            .and_then(|mut r| r.recording())
            .unwrap_err();
        assert!(matches!(err, TraceError::Corrupt(_)), "{err}");
    }
}
