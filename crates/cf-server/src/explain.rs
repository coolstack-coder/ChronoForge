//! Plain-English write-up of a failure.
//!
//! The deterministic analysis does the actual reasoning; this just narrates
//! it. If the optional Python helper (`analysis/chronoforge_ai`) is set up
//! with an API key we ask an LLM to write the narrative from the evidence
//! bundle, and reject any answer that cites steps not in the evidence.
//! Otherwise we write the narrative ourselves from the top hypothesis.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::{json, Value};

use crate::session::Session;

#[derive(Serialize)]
pub struct Explanation {
    pub mode: String,
    pub model: Option<String>,
    pub narrative: String,
    pub cited_steps: Vec<u64>,
    pub grounded: bool,
    pub note: Option<String>,
}

pub fn evidence(s: &Session) -> Value {
    let numbered: Vec<String> = s
        .program
        .source
        .lines()
        .enumerate()
        .map(|(i, l)| format!("{:>3} | {l}", i + 1))
        .collect();
    let r = &s.report;
    let slice: Vec<Value> = r
        .slice
        .iter()
        .flat_map(|sl| sl.nodes.iter())
        .rev()
        .take(40)
        .map(|n| json!({"step": n.step, "tid": n.tid, "line": n.line, "what": n.what}))
        .collect();
    json!({
        "program": numbered.join("\n"),
        "outcome": r.fault.as_ref().map(|f| json!({
            "kind": f.kind.label(), "message": f.message, "step": f.step, "tid": f.tid, "line": f.line
        })),
        "hypotheses": r.hypotheses.iter().take(4).collect::<Vec<_>>(),
        "races": r.races.iter().take(6).collect::<Vec<_>>(),
        "lock_cycles": r.locks.cycles,
        "slice": slice,
        "stats": r.stats,
    })
}

/// Every step number the analysis is willing to stand behind.
fn known_steps(ev: &Value) -> std::collections::BTreeSet<u64> {
    fn walk(v: &Value, out: &mut std::collections::BTreeSet<u64>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    if k == "step" {
                        if let Some(n) = v.as_u64() {
                            out.insert(n);
                        }
                    }
                    walk(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Default::default();
    walk(ev, &mut out);
    out
}

fn helper_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CHRONOFORGE_AI_DIR") {
        return Some(PathBuf::from(dir));
    }
    let local = PathBuf::from("analysis");
    local.join("chronoforge_ai").is_dir().then_some(local)
}

fn try_llm(ev: &Value) -> Result<Explanation, String> {
    if std::env::var("ANTHROPIC_API_KEY")
        .map(|k| k.is_empty())
        .unwrap_or(true)
    {
        return Err("ANTHROPIC_API_KEY not set".into());
    }
    let dir = helper_dir().ok_or("analysis/chronoforge_ai not found")?;
    let python = std::env::var("CHRONOFORGE_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut child = Command::new(python)
        .args(["-m", "chronoforge_ai", "explain"])
        .env("PYTHONPATH", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start python: {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(ev.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .last()
            .unwrap_or("helper failed")
            .to_string());
    }
    let v: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("bad helper output: {e}"))?;
    let cited: Vec<u64> = v["cited_steps"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    let known = known_steps(ev);
    let grounded = cited.iter().all(|s| known.contains(s));
    Ok(Explanation {
        mode: "llm".into(),
        model: v["model"].as_str().map(String::from),
        narrative: v["narrative"].as_str().unwrap_or_default().to_string(),
        cited_steps: cited,
        grounded,
        note: (!grounded).then(|| {
            "The model cited steps that aren't in the evidence; treat with suspicion.".into()
        }),
    })
}

fn offline(s: &Session, why: Option<String>) -> Explanation {
    let r = &s.report;
    let mut text = String::new();
    let mut cited = Vec::new();
    match (&r.fault, r.hypotheses.first()) {
        (Some(f), Some(h)) => {
            text.push_str(&format!(
                "**What happened.** {} at line {} on thread T{} (step {}): {}\n\n",
                capitalise(f.kind.label()),
                f.line,
                f.tid,
                f.step,
                f.message
            ));
            text.push_str(&format!(
                "**Most likely cause — {}** ({:.0}% confidence). {}\n\n",
                h.title,
                h.confidence * 100.0,
                h.explanation
            ));
            if let Some(fix) = &h.fix {
                text.push_str(&format!("**Suggested fix.** {fix}\n\n"));
            }
            if r.hypotheses.len() > 1 {
                text.push_str("**Also considered:** ");
                let rest: Vec<String> = r.hypotheses[1..]
                    .iter()
                    .take(3)
                    .map(|h| format!("{} ({:.0}%)", h.title, h.confidence * 100.0))
                    .collect();
                text.push_str(&rest.join(", "));
                text.push('.');
            }
            cited.extend(h.evidence.iter().map(|e| e.step));
        }
        (None, Some(h)) => {
            text.push_str(&format!(
                "This run completed, but there's a latent problem: **{}**. {}",
                h.title, h.explanation
            ));
            cited.extend(h.evidence.iter().map(|e| e.step));
        }
        _ => text
            .push_str("The run completed and no races, lock-order problems or faults were found."),
    }
    Explanation {
        mode: "offline".into(),
        model: None,
        narrative: text,
        cited_steps: cited,
        grounded: true,
        note: why,
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub fn explain(s: &Session, allow_llm: bool) -> Explanation {
    if !allow_llm {
        return offline(s, None);
    }
    let ev = evidence(s);
    match try_llm(&ev) {
        Ok(e) => e,
        Err(why) => offline(
            s,
            Some(format!(
                "LLM narrative unavailable ({why}); showing the deterministic analysis."
            )),
        ),
    }
}
