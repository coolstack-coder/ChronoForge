import { useEffect, useRef, useState } from "react";
import type { Report, StateView, Var } from "../api";
import { threadColor, threadHex } from "../highlight";

interface Props {
  state: StateView | null;
  report: Report | null;
  cursor: number;
  selectedLoc: string | null;
  onSelectLoc: (loc: string | null) => void;
  onSeek: (step: number) => void;
}

type Tab = "state" | "concurrency" | "slice" | "output";

export function Inspector({ state, report, cursor, selectedLoc, onSelectLoc, onSeek }: Props) {
  const [tab, setTab] = useState<Tab>("state");
  const races = report?.races.length ?? 0;
  const cycles = report?.locks.cycles.length ?? 0;
  return (
    <>
      <div className="insp-tabs">
        {(["state", "concurrency", "slice", "output"] as Tab[]).map((t) => (
          <button key={t} className={`insp-tab ${tab === t ? "on" : ""}`} onClick={() => setTab(t)}>
            {t}
            {t === "concurrency" && races + cycles > 0 && <span className="n">{races + cycles}</span>}
            {t === "slice" && report?.slice && <span className="n">{report.slice.nodes.length}</span>}
          </button>
        ))}
      </div>
      <div className="pane-body">
        {tab === "state" && <StateTab state={state} selectedLoc={selectedLoc} onSelectLoc={onSelectLoc} onSeek={onSeek} />}
        {tab === "concurrency" && <ConcurrencyTab report={report} state={state} onSeek={onSeek} />}
        {tab === "slice" && <SliceTab report={report} cursor={cursor} onSeek={onSeek} />}
        {tab === "output" && <OutputTab state={state} />}
      </div>
    </>
  );
}

function VarRow({ v, selected, onSelect, onSeek }: { v: Var; selected: boolean; onSelect: () => void; onSeek: (s: number) => void }) {
  // flash the value when it changes under the playhead
  const prev = useRef(v.value);
  const [changed, setChanged] = useState(false);
  useEffect(() => {
    if (prev.current !== v.value) {
      setChanged(true);
      const t = setTimeout(() => setChanged(false), 900);
      prev.current = v.value;
      return () => clearTimeout(t);
    }
  }, [v.value]);
  return (
    <div className={`var ${selected ? "sel" : ""}`}>
      <span className="name" onClick={onSelect} title="show value history on the timeline">{v.name}</span>
      <span className={`val ${changed ? "changed" : ""}`} title={v.value}>{v.value}</span>
      {v.writer !== null ? (
        <button className="who" onClick={() => onSeek(v.writer!)} title="reverse-jump to the step that wrote this value">
          ⟲ #{v.writer}
        </button>
      ) : <span />}
    </div>
  );
}

function StateTab({ state, selectedLoc, onSelectLoc, onSeek }: Omit<Props, "report" | "cursor">) {
  if (!state) return <div className="empty"><span className="spin" /> rewinding…</div>;
  const { snapshot: s, event: e } = state;
  return (
    <>
      {e && (
        <div className="now">
          <div className="meta">
            <span className="t" style={{ color: threadColor(e.tid) }}>T{e.tid}</span>
            <span>#{e.step}</span>
            <span>{e.func}()</span>
            <span>line {e.line}</span>
            <span style={{ marginLeft: "auto" }}>{state.seek_ms.toFixed(2)} ms seek</span>
          </div>
          <div className="what">{e.what}</div>
          <div className="code">{e.code}</div>
          <div style={{ display: "flex", gap: 6, marginTop: 8, flexWrap: "wrap" }}>
            <span className="badge">{e.op}</span>
            {e.writes.some((w) => w.shared) && <span className="badge write">shared write</span>}
            {e.sync && <span className="badge sync">{e.sync}</span>}
            {e.nondet && <span className="badge nondet">◆ {e.nondet} (from recording)</span>}
            {e.deps.length > 0 && (
              <span className="badge" style={{ cursor: "pointer" }} onClick={() => onSeek(Math.max(...e.deps))} title="jump to the step that produced this step's input">
                ← depends on #{e.deps.join(", #")}
              </span>
            )}
          </div>
        </div>
      )}

      <div className="section">
        <h4>threads</h4>
        {s.threads.map((t) => (
          <div key={t.tid} className={`thread ${t.tid === s.current ? "cur" : ""}`} style={{ "--tc": threadHex(t.tid) } as React.CSSProperties}>
            <div className="thread-head">
              <b style={{ color: threadColor(t.tid) }}>T{t.tid}</b>
              <span>{t.name}</span>
              <span className="faint">L{t.line}</span>
              <span className={`st ${t.state.startsWith("blocked") || t.state.startsWith("joining") ? "blocked" : t.state === "done" ? "done" : ""}`}>{t.state}</span>
            </div>
            {t.state !== "done" &&
              t.frames.slice(0, 2).map((f, i) => (
                <div key={i} className="frame">
                  <div className="frame-name">{f.func}() · line {f.line}</div>
                  <div className="vars">
                    {f.locals.map((v) => (
                      <VarRow key={v.loc} v={v} selected={selectedLoc === v.loc} onSelect={() => onSelectLoc(v.loc)} onSeek={onSeek} />
                    ))}
                  </div>
                </div>
              ))}
          </div>
        ))}
      </div>

      <div className="section">
        <h4>globals</h4>
        <div className="vars">
          {s.globals.map((v) => (
            <VarRow key={v.loc} v={v} selected={selectedLoc === v.loc} onSelect={() => onSelectLoc(selectedLoc === v.loc ? null : v.loc)} onSeek={onSeek} />
          ))}
        </div>
      </div>

      {s.heap.length > 0 && (
        <div className="section">
          <h4>heap</h4>
          {s.heap.slice(0, 6).map((h) => (
            <div key={h.id} style={{ marginBottom: 6 }}>
              <div className="frame-name">array#{h.id}</div>
              <div className="vars">
                {h.items.slice(0, 12).map((v) => (
                  <VarRow key={v.loc} v={v} selected={selectedLoc === v.loc} onSelect={() => onSelectLoc(v.loc)} onSeek={onSeek} />
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

function LockGraph({ report }: { report: Report }) {
  const nodes = report.locks.nodes;
  if (!nodes.length) return null;
  const W = 320, H = 190, cx = W / 2, cy = H / 2, R = Math.min(70, 30 + nodes.length * 10);
  const pos = new Map(nodes.map(([id], i) => {
    const a = (i / nodes.length) * Math.PI * 2 - Math.PI / 2;
    return [id, { x: cx + R * Math.cos(a), y: cy + R * Math.sin(a) }];
  }));
  const inCycle = new Set(report.locks.cycles.flatMap((c) => c.edges.map((e) => `${e.from}-${e.to}`)));
  return (
    <svg className="lockgraph" viewBox={`0 0 ${W} ${H}`}>
      <defs>
        <marker id="arr" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
          <path d="M0,0 L10,5 L0,10 z" fill="context-stroke" />
        </marker>
      </defs>
      {report.locks.edges.map((e, i) => {
        const a = pos.get(e.from)!, b = pos.get(e.to)!;
        const dx = b.x - a.x, dy = b.y - a.y, len = Math.hypot(dx, dy) || 1;
        const k = 20 / len;
        // bow edges so A→B and B→A don't sit on top of each other
        const mx = (a.x + b.x) / 2 - dy * 0.18, my = (a.y + b.y) / 2 + dx * 0.18;
        const bad = inCycle.has(`${e.from}-${e.to}`);
        return (
          <path key={i} d={`M${a.x + dx * k},${a.y + dy * k} Q${mx},${my} ${b.x - dx * k},${b.y - dy * k}`}
            fill="none" stroke={bad ? "#ff5252" : threadHex(e.tid)} strokeWidth={bad ? 2 : 1.2} markerEnd="url(#arr)" opacity={0.9} />
        );
      })}
      {nodes.map(([id, name]) => {
        const p = pos.get(id)!;
        return (
          <g key={id}>
            <circle cx={p.x} cy={p.y} r={17} fill="#16161a" stroke="#2f2f38" />
            <text x={p.x} y={p.y + 4} textAnchor="middle" fontSize="10" fontFamily="JetBrains Mono" fill="#ece9e2">{name.replace("forks", "f")}</text>
          </g>
        );
      })}
    </svg>
  );
}

function ConcurrencyTab({ report, state, onSeek }: { report: Report | null; state: StateView | null; onSeek: (s: number) => void }) {
  if (!report) return null;
  return (
    <>
      <div className="section">
        <h4>data races · happens-before</h4>
        {report.races.length === 0 && <div className="muted" style={{ fontSize: 12, padding: "4px 0 10px" }}>No unordered conflicting accesses. Every shared access is ordered by a lock, spawn or join.</div>}
        {report.races.map((r, i) => (
          <div key={i} className="race-card">
            <div className="title">⚡ {r.name} · {r.kind}{r.occurrences > 1 ? ` · ×${r.occurrences}` : ""}</div>
            <div className="pair">
              <div className="acc" onClick={() => onSeek(r.first.step)}>
                <b style={{ color: threadColor(r.first.tid) }}>T{r.first.tid}</b> {r.first.kind} <span className="faint">#{r.first.step}</span>
                <div className="faint">line {r.first.line} = {r.first.value}</div>
              </div>
              <div className="x">⟷</div>
              <div className="acc" onClick={() => onSeek(r.second.step)}>
                <b style={{ color: threadColor(r.second.tid) }}>T{r.second.tid}</b> {r.second.kind} <span className="faint">#{r.second.step}</span>
                <div className="faint">line {r.second.line} = {r.second.value}</div>
              </div>
            </div>
          </div>
        ))}
      </div>
      <div className="section">
        <h4>lock-order graph</h4>
        {report.locks.nodes.length === 0 ? <div className="muted" style={{ fontSize: 12 }}>No nested locking.</div> : <LockGraph report={report} />}
        {report.locks.cycles.map((c, i) => (
          <div key={i} className="lock-card" style={{ borderColor: "#ff525266" }}>
            <div style={{ color: "#ff5252", marginBottom: 4 }}>cycle: {c.mutexes.join(" → ")} → {c.mutexes[0]}</div>
            {c.edges.map((e, j) => (
              <div key={j} className="faint" style={{ cursor: "pointer" }} onClick={() => onSeek(e.step)}>
                T{e.tid} holds {report.locks.nodes.find((n) => n[0] === e.from)?.[1]} → wants {report.locks.nodes.find((n) => n[0] === e.to)?.[1]} · line {e.line} · #{e.step}
              </div>
            ))}
          </div>
        ))}
      </div>
      {state && state.snapshot.mutexes.length > 0 && (
        <div className="section">
          <h4>mutexes at playhead</h4>
          <div className="vars">
            {state.snapshot.mutexes.map((m) => (
              <div key={m.id} className="var">
                <span className="name">{report.locks.nodes.find((n) => n[0] === m.id)?.[1] ?? `mutex#${m.id}`}</span>
                <span className="val">{m.owner === null ? <span className="faint">free</span> : <span style={{ color: threadColor(m.owner) }}>held by T{m.owner}</span>}</span>
                <span className="faint">{m.waiters.length ? `waiting: ${m.waiters.map((w) => "T" + w).join(" ")}` : ""}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </>
  );
}

function SliceTab({ report, cursor, onSeek }: { report: Report | null; cursor: number; onSeek: (s: number) => void }) {
  const slice = report?.slice;
  if (!slice) return <div className="empty">The run didn't fail, so there's nothing to slice backwards from.</div>;
  return (
    <div className="section">
      <h4>backward slice from step #{slice.criterion} · {slice.nodes.length} steps{slice.truncated ? " (truncated)" : ""}</h4>
      <div className="vars">
        {[...slice.nodes].reverse().map((n) => (
          <div key={n.step} className={`var ${n.step === cursor ? "sel" : ""}`} style={{ cursor: "pointer", gridTemplateColumns: "54px 26px 1fr" }} onClick={() => onSeek(n.step)}>
            <span className="faint">#{n.step}</span>
            <b style={{ color: threadColor(n.tid) }}>T{n.tid}</b>
            <span className="val" style={{ paddingLeft: Math.min(n.depth, 8) * 6 }}>
              <span className="faint">L{n.line} </span>{n.what}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function OutputTab({ state }: { state: StateView | null }) {
  if (!state) return null;
  return (
    <div style={{ padding: "10px 0" }}>
      {state.snapshot.output.length === 0 && <div className="empty">Nothing printed yet at this point in time.</div>}
      {state.snapshot.output.map((o, i) => (
        <div key={i} className="output-line">
          <span className="t" style={{ color: threadColor(o.tid) }}>T{o.tid}</span>
          <span className="faint">#{o.step} </span>
          {o.text}
        </div>
      ))}
    </div>
  );
}
