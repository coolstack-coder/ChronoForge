import { useState } from "react";
import type { Divergence, Hunt } from "../api";
import { threadColor } from "../highlight";

interface Props {
  name: string;
  hunt: Hunt | null;
  running: boolean;
  diff: Divergence | null;
  onRun: (runs: number, chaos: number) => void;
  onOpen: (seed: number) => void;
  onClose: () => void;
}

export function HuntSheet({ name, hunt, running, diff, onRun, onOpen, onClose }: Props) {
  const [runs, setRuns] = useState(300);
  const [chaos, setChaos] = useState(0.08);
  const pct = hunt ? (hunt.failures * 100) / hunt.runs.length : 0;
  return (
    <div className="scrim" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="sheet">
        <div className="sheet-head">
          <h2>{diff ? "Where the runs part ways" : <>Chaos hunt · <span className="muted">{name}.forge</span></>}</h2>
          <span className="spacer" />
          {!diff && (
            <>
              <label className="hint">runs <input type="range" min={50} max={1000} step={50} value={runs} onChange={(e) => setRuns(+e.target.value)} /> <b className="mono">{runs}</b></label>
              <label className="hint">chaos <input type="range" min={0.01} max={0.4} step={0.01} value={chaos} onChange={(e) => setChaos(+e.target.value)} /> <b className="mono">{chaos.toFixed(2)}</b></label>
              <button className="btn primary" disabled={running} onClick={() => onRun(runs, chaos)}>
                {running ? <span className="spin" /> : <span className="rec-dot" />} {hunt ? "hunt again" : "start hunt"}
              </button>
            </>
          )}
          <button className="btn ghost" onClick={onClose}>esc</button>
        </div>
        <div className="sheet-body">
          {diff ? <DiffView d={diff} /> : !hunt ? (
            <div className="muted" style={{ fontSize: 14, lineHeight: 1.6, maxWidth: "68ch" }}>
              Every run uses a different scheduler seed and preempts threads at random instructions, the way rr's chaos mode does.
              Each one is <b className="mono">recorded</b> — only the scheduling decisions and inputs, a few hundred bytes — so any failure you see here can be
              opened and replayed exactly, as many times as you like.
            </div>
          ) : (
            <>
              <div className="row">
                <div className="big-stat">{hunt.runs.length}<small>recorded runs</small></div>
                <div className="big-stat" style={{ color: hunt.failures ? "var(--danger)" : "var(--ok)" }}>{hunt.failures}<small>failures · {pct.toFixed(1)}%</small></div>
                <div className="big-stat">{hunt.elapsed_ms.toFixed(0)}<small>ms total</small></div>
                <div className="hint" style={{ marginLeft: "auto", maxWidth: 260 }}>Click any cell to replay that exact run. Red ones failed.</div>
              </div>
              <div className="waffle">
                {hunt.runs.map((r, i) => (
                  <button
                    key={r.seed}
                    className={`cell ${r.failed ? "fail" : ""}`}
                    style={{ animationDelay: `${Math.min(i * 2, 900)}ms` }}
                    title={`seed ${r.seed} · ${r.signature} · ${r.steps} steps · ${r.switches} context switches`}
                    onClick={() => onOpen(r.seed)}
                  />
                ))}
              </div>
              <div className="sigs">
                {hunt.signatures.map(([sig, n]) => (
                  <div key={sig} className="sig">
                    <span style={{ color: sig === "pass" ? "var(--ok)" : "var(--danger)" }}>{sig}</span>
                    <span className="muted">{n} runs</span>
                    <span className="bar"><i style={{ width: `${(n * 100) / hunt.runs.length}%`, background: sig === "pass" ? "var(--ok)" : "var(--danger)" }} /></span>
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

const code = (t: string) =>
  t.split(/(`[^`]+`)/g).map((p, i) => (p.startsWith("`") ? <code key={i} className="mono">{p.slice(1, -1)}</code> : p));

function DiffView({ d }: { d: Divergence }) {
  return (
    <>
      <div className="row" style={{ marginBottom: 6 }}>
        <div className="big-stat">{d.common_prefix}<small>identical steps</small></div>
        <div className="big-stat" style={{ color: "var(--signal)" }}>{d.kind}<small>first divergence</small></div>
        {d.data && <div className="big-stat" style={{ color: "var(--amber)" }}>{d.data.name}<small>first data divergence</small></div>}
      </div>
      {d.data && (
        <div className="diff-card" style={{ borderColor: "#ffb54766" }}>
          <div className="head"><span>write #{d.data.write + 1} to {d.data.name}</span><span>— the one that matters</span></div>
          <div className="cols">
            {(["a", "b"] as const).map((k) => {
              const w = d.data![k];
              return (
                <div key={k} className="col">
                  <h5>{k === "a" ? "PASSING RUN" : "FAILING RUN"}</h5>
                  {w ? (
                    <div><b style={{ color: threadColor(w.tid) }}>T{w.tid}</b> · line {w.line} · step #{w.step} → <b style={{ color: k === "b" ? "var(--danger)" : "var(--ok)", fontSize: 18 }}>{w.value}</b></div>
                  ) : <div className="muted">never written</div>}
                </div>
              );
            })}
          </div>
          <div className="explain">{code(d.data.explanation)}</div>
        </div>
      )}
      <div className="diff-card">
        <div className="head"><span>step #{d.index}</span><span>— where the schedules first differ</span></div>
        <div className="cols">
          {(["a", "b"] as const).map((k) => {
            const s = d[k];
            return (
              <div key={k} className="col">
                <h5>{k === "a" ? "PASSING RUN" : "FAILING RUN"} · {k === "a" ? d.outcome_a : d.outcome_b}</h5>
                {s ? (
                  <>
                    <div><b style={{ color: threadColor(s.tid) }}>T{s.tid}</b> · line {s.line}</div>
                    <div style={{ margin: "6px 0", color: "var(--paper)" }}>{s.code}</div>
                    <div className="muted">{s.what}</div>
                  </>
                ) : <div className="muted">— run ended —</div>}
              </div>
            );
          })}
        </div>
        <div className="explain">{code(d.explanation)}</div>
      </div>
    </>
  );
}
