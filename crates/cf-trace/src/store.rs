//! Execution storage. The server keeps every replayed session here so step
//! ranges can be served without holding everything in memory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::event::{Recording, Step, StepEvent};
use crate::format::{self, TraceReader};
use crate::TraceError;

pub trait TraceStore: Send + Sync {
    fn put(&self, id: &str, rec: &Recording, steps: &[StepEvent]) -> Result<(), TraceError>;
    fn recording(&self, id: &str) -> Result<Recording, TraceError>;
    fn steps(&self, id: &str, from: Step, to: Step) -> Result<Vec<StepEvent>, TraceError>;
    fn ids(&self) -> Result<Vec<String>, TraceError>;
    fn backend(&self) -> &'static str;
}

/// Plain in-memory store, used by tests and `--store memory`.
#[derive(Default)]
pub struct MemoryStore {
    inner: RwLock<BTreeMap<String, (Recording, Vec<StepEvent>)>>,
}

impl TraceStore for MemoryStore {
    fn put(&self, id: &str, rec: &Recording, steps: &[StepEvent]) -> Result<(), TraceError> {
        self.inner
            .write()
            .unwrap()
            .insert(id.to_string(), (rec.clone(), steps.to_vec()));
        Ok(())
    }

    fn recording(&self, id: &str) -> Result<Recording, TraceError> {
        let g = self.inner.read().unwrap();
        g.get(id)
            .map(|(r, _)| r.clone())
            .ok_or_else(|| TraceError::NotFound(id.into()))
    }

    fn steps(&self, id: &str, from: Step, to: Step) -> Result<Vec<StepEvent>, TraceError> {
        let g = self.inner.read().unwrap();
        let (_, steps) = g.get(id).ok_or_else(|| TraceError::NotFound(id.into()))?;
        let lo = (from as usize).min(steps.len());
        let hi = (to as usize).min(steps.len());
        Ok(steps[lo..hi].to_vec())
    }

    fn ids(&self) -> Result<Vec<String>, TraceError> {
        Ok(self.inner.read().unwrap().keys().cloned().collect())
    }

    fn backend(&self) -> &'static str {
        "memory"
    }
}

/// One `.cfrec` file per session in a directory. Uses the chunk index for
/// range reads.
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, TraceError> {
        std::fs::create_dir_all(dir.as_ref())?;
        Ok(FileStore {
            dir: dir.as_ref().to_path_buf(),
        })
    }

    fn path(&self, id: &str) -> Result<PathBuf, TraceError> {
        // ids come from URLs, so don't let anyone walk out of the directory
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(TraceError::NotFound(id.into()));
        }
        Ok(self.dir.join(format!("{id}.cfrec")))
    }
}

impl TraceStore for FileStore {
    fn put(&self, id: &str, rec: &Recording, steps: &[StepEvent]) -> Result<(), TraceError> {
        format::write_recording(&self.path(id)?, rec, Some(steps))?;
        Ok(())
    }

    fn recording(&self, id: &str) -> Result<Recording, TraceError> {
        TraceReader::open(&self.path(id)?)?.recording()
    }

    fn steps(&self, id: &str, from: Step, to: Step) -> Result<Vec<StepEvent>, TraceError> {
        TraceReader::open(&self.path(id)?)?.steps(from, to)
    }

    fn ids(&self) -> Result<Vec<String>, TraceError> {
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "cfrec") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    ids.push(stem.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn backend(&self) -> &'static str {
        "file"
    }
}

#[cfg(feature = "rocksdb")]
pub use rocks::RocksStore;

#[cfg(feature = "rocksdb")]
mod rocks {
    use super::*;
    use rocksdb::{Direction, IteratorMode, Options, DB};

    /// Keys:
    ///   r/{id}              -> bincode(Recording)
    ///   s/{id}/{step:be64}  -> bincode(StepEvent)
    /// Big-endian step numbers keep each session's steps sorted, so a range
    /// read is a single forward iterator.
    pub struct RocksStore {
        db: DB,
    }

    impl RocksStore {
        pub fn open(path: impl AsRef<Path>) -> Result<Self, TraceError> {
            let mut opts = Options::default();
            opts.create_if_missing(true);
            opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
            let db = DB::open(&opts, path).map_err(|e| TraceError::Backend(e.to_string()))?;
            Ok(RocksStore { db })
        }

        fn step_key(id: &str, step: Step) -> Vec<u8> {
            let mut k = format!("s/{id}/").into_bytes();
            k.extend_from_slice(&step.to_be_bytes());
            k
        }
    }

    impl TraceStore for RocksStore {
        fn put(&self, id: &str, rec: &Recording, steps: &[StepEvent]) -> Result<(), TraceError> {
            let mut batch = rocksdb::WriteBatch::default();
            batch.put(format!("r/{id}"), bincode::serialize(rec)?);
            for s in steps {
                batch.put(Self::step_key(id, s.step), bincode::serialize(s)?);
            }
            self.db
                .write(batch)
                .map_err(|e| TraceError::Backend(e.to_string()))
        }

        fn recording(&self, id: &str) -> Result<Recording, TraceError> {
            let raw = self
                .db
                .get(format!("r/{id}"))
                .map_err(|e| TraceError::Backend(e.to_string()))?
                .ok_or_else(|| TraceError::NotFound(id.into()))?;
            Ok(bincode::deserialize(&raw)?)
        }

        fn steps(&self, id: &str, from: Step, to: Step) -> Result<Vec<StepEvent>, TraceError> {
            let start = Self::step_key(id, from);
            let end = Self::step_key(id, to);
            let mut out = Vec::new();
            for item in self
                .db
                .iterator(IteratorMode::From(&start, Direction::Forward))
            {
                let (k, v) = item.map_err(|e| TraceError::Backend(e.to_string()))?;
                if k.as_ref() >= end.as_slice() {
                    break;
                }
                out.push(bincode::deserialize(&v)?);
            }
            Ok(out)
        }

        fn ids(&self) -> Result<Vec<String>, TraceError> {
            let mut ids = Vec::new();
            for item in self.db.prefix_iterator(b"r/") {
                let (k, _) = item.map_err(|e| TraceError::Backend(e.to_string()))?;
                if !k.starts_with(b"r/") {
                    break;
                }
                ids.push(String::from_utf8_lossy(&k[2..]).into_owned());
            }
            Ok(ids)
        }

        fn backend(&self) -> &'static str {
            "rocksdb"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::*;

    fn rec() -> Recording {
        Recording {
            meta: TraceMeta::default(),
            nondet: vec![],
            outcome: Outcome::Completed,
            total_steps: 3,
            output: vec![],
        }
    }

    fn steps() -> Vec<StepEvent> {
        (0..3)
            .map(|i| StepEvent {
                step: i,
                tid: 0,
                func: 0,
                pc: 0,
                line: 1,
                op: OpTag::Const,
                reads: vec![],
                writes: vec![],
                deps: vec![],
                sync: None,
                nondet: None,
                result: None,
            })
            .collect()
    }

    fn exercise(store: &dyn TraceStore) {
        store.put("abc", &rec(), &steps()).unwrap();
        assert_eq!(store.recording("abc").unwrap(), rec());
        assert_eq!(store.steps("abc", 1, 3).unwrap(), steps()[1..3].to_vec());
        assert_eq!(store.ids().unwrap(), vec!["abc".to_string()]);
        assert!(store.recording("nope").is_err());
    }

    #[test]
    fn memory_store() {
        exercise(&MemoryStore::default());
    }

    #[test]
    fn file_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::open(dir.path()).unwrap();
        exercise(&store);
        assert!(store.recording("../etc/passwd").is_err());
    }

    #[cfg(feature = "rocksdb")]
    #[test]
    fn rocks_store() {
        let dir = tempfile::tempdir().unwrap();
        exercise(&RocksStore::open(dir.path()).unwrap());
    }
}
