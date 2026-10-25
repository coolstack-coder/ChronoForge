use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A runtime value as the VM sees it. Lives in cf-trace because traces
/// carry values around and we don't want two parallel definitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Val {
    Nil,
    Int(i64),
    Bool(bool),
    Str(Arc<str>),
    /// Reference into the VM heap (arrays are the only heap object for now).
    Array(u32),
    Func(u32),
    Mutex(u32),
    Thread(u32),
}

impl Val {
    pub fn type_name(&self) -> &'static str {
        match self {
            Val::Nil => "nil",
            Val::Int(_) => "int",
            Val::Bool(_) => "bool",
            Val::Str(_) => "str",
            Val::Array(_) => "array",
            Val::Func(_) => "fn",
            Val::Mutex(_) => "mutex",
            Val::Thread(_) => "thread",
        }
    }

    pub fn truthy(&self) -> bool {
        !matches!(self, Val::Nil | Val::Bool(false))
    }

    pub fn str(s: &str) -> Val {
        Val::Str(Arc::from(s))
    }
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Val::Nil => write!(f, "nil"),
            Val::Int(i) => write!(f, "{i}"),
            Val::Bool(b) => write!(f, "{b}"),
            Val::Str(s) => write!(f, "{s:?}"),
            Val::Array(id) => write!(f, "array#{id}"),
            Val::Func(id) => write!(f, "fn#{id}"),
            Val::Mutex(id) => write!(f, "mutex#{id}"),
            Val::Thread(id) => write!(f, "T{id}"),
        }
    }
}

/// A memory location the instrumentation can observe.
///
/// Locals are keyed by a unique frame id (not by depth) so two calls of the
/// same function never alias each other in the trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Loc {
    Global(u32),
    Local { frame: u64, slot: u32 },
    Heap { obj: u32, idx: u32 },
}

impl Loc {
    /// Locals can't escape a frame (no closures), so only globals and heap
    /// cells can ever be shared between threads.
    pub fn is_shared(&self) -> bool {
        !matches!(self, Loc::Local { .. })
    }
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Loc::Global(g) => write!(f, "g:{g}"),
            Loc::Local { frame, slot } => write!(f, "l:{frame}:{slot}"),
            Loc::Heap { obj, idx } => write!(f, "h:{obj}:{idx}"),
        }
    }
}

impl std::str::FromStr for Loc {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        let num = |p: &str| p.parse::<u64>().map_err(|_| format!("bad location `{s}`"));
        match parts.as_slice() {
            ["g", g] => Ok(Loc::Global(num(g)? as u32)),
            ["l", fr, sl] => Ok(Loc::Local {
                frame: num(fr)?,
                slot: num(sl)? as u32,
            }),
            ["h", o, i] => Ok(Loc::Heap {
                obj: num(o)? as u32,
                idx: num(i)? as u32,
            }),
            _ => Err(format!("bad location `{s}`")),
        }
    }
}

// Strings for JSON (the UI wants "g:3"), a compact tuple for bincode.
impl Serialize for Loc {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.collect_str(self)
        } else {
            let (tag, a, b) = match *self {
                Loc::Global(g) => (0u8, g as u64, 0u32),
                Loc::Local { frame, slot } => (1, frame, slot),
                Loc::Heap { obj, idx } => (2, obj as u64, idx),
            };
            (tag, a, b).serialize(s)
        }
    }
}

impl<'de> Deserialize<'de> for Loc {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if d.is_human_readable() {
            let s = String::deserialize(d)?;
            s.parse().map_err(serde::de::Error::custom)
        } else {
            let (tag, a, b) = <(u8, u64, u32)>::deserialize(d)?;
            match tag {
                0 => Ok(Loc::Global(a as u32)),
                1 => Ok(Loc::Local { frame: a, slot: b }),
                2 => Ok(Loc::Heap {
                    obj: a as u32,
                    idx: b,
                }),
                _ => Err(serde::de::Error::custom("unknown location tag")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loc_roundtrips_through_both_encodings() {
        let locs = [
            Loc::Global(7),
            Loc::Local { frame: 99, slot: 2 },
            Loc::Heap { obj: 3, idx: 41 },
        ];
        for loc in locs {
            let json = serde_json::to_string(&loc).unwrap();
            assert_eq!(serde_json::from_str::<Loc>(&json).unwrap(), loc);
            let bin = bincode::serialize(&loc).unwrap();
            assert_eq!(bincode::deserialize::<Loc>(&bin).unwrap(), loc);
        }
        assert_eq!(serde_json::to_string(&Loc::Global(7)).unwrap(), "\"g:7\"");
    }
}
