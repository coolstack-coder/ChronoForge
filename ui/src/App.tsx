import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api, ApiError, type Divergence, type Example, type HistoryPoint, type Hunt, type Report,
  type StateView, type Summary, type TimelineData,
} from "./api";
import { Landing } from "./components/Landing";
import { SourceView } from "./components/SourceView";
import { Timeline } from "./components/Timeline";
import { Verdict } from "./components/Verdict";
import { Inspector } from "./components/Inspector";
import { HuntSheet } from "./components/HuntSheet";
import { NativeView } from "./components/NativeView";

interface Loaded {
  summary: Summary;
  timeline: TimelineData;
  report: Report;
}

const fmtBytes = (b: number) => (b < 1024 ? `${b} B` : `${(b / 1024).toFixed(1)} KB`);

export default function App() {
  const [landing, setLanding] = useState(() => !new URLSearchParams(location.search).has("skip"));
  const [view, setView] = useState<"debug" | "native">("debug");
  const [examples, setExamples] = useState<Example[]>([]);
  const [active, setActive] = useState("bank");
  const [source, setSource] = useState("");
  const [editing, setEditing] = useState(false);
  const [compileErr, setCompileErr] = useState<string | null>(null);

  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [cursor, setCursor] = useState(0);
  const [state, setState] = useState<StateView | null>(null);
  const [breakpoints, setBreakpoints] = useState<Set<number>>(new Set());
  const [hyp, setHyp] = useState(0);
  const [selectedLoc, setSelectedLoc] = useState<string | null>(null);
  const [history, setHistory] = useState<{ name: string; points: HistoryPoint[] } | null>(null);
  const [playing, setPlaying] = useState(false);

  const [huntOpen, setHuntOpen] = useState(false);
  const [hunt, setHunt] = useState<Hunt | null>(null);
  const [hunting, setHunting] = useState(false);
  const [diff, setDiff] = useState<Divergence | null>(null);
  const [toast, setToast] = useState<React.ReactNode | null>(null);
  const [busy, setBusy] = useState(false);

  const cache = useRef(new Map<string, StateView>());
  const say = useCallback((t: React.ReactNode) => {
    setToast(t);
    window.setTimeout(() => setToast(null), 2600);
  }, []);

  useEffect(() => {
    api.examples().then((ex) => {
      setExamples(ex);
      const first = ex.find((e) => e.name === "bank") ?? ex[0];
      if (first) {
        setActive(first.name);
        setSource(first.source);
      }
    });
  }, []);

  const total = loaded?.summary.total_steps ?? 0;
  const tl = loaded?.timeline;

  const seek = useCallback(
    (s: number) => setCursor(Math.max(0, Math.min(Math.max(0, total - 1), Math.round(s)))),
    [total],
  );

  const load = useCallback(async (summary: Summary) => {
    const [timeline, report] = await Promise.all([api.timeline(summary.id), api.analysis(summary.id)]);
    cache.current.clear();
    setLoaded({ summary, timeline, report });
    setHyp(0);
    setHistory(null);
    setSelectedLoc(null);
    setState(null);
    // land just before the failure: that's where you want to start rewinding from
    const f = summary.outcome.step;
    setCursor(f !== null ? Math.max(0, f) : Math.max(0, summary.total_steps - 1));
  }, []);

  // fetch machine state for the playhead; debounced + cached so scrubbing stays smooth
  useEffect(() => {
    if (!loaded) return;
    const key = `${loaded.summary.id}:${cursor}`;
    const hit = cache.current.get(key);
    if (hit) {
      setState(hit);
      return;
    }
    const ctrl = new AbortController();
    const t = window.setTimeout(() => {
      api.state(loaded.summary.id, cursor, ctrl.signal).then((s) => {
        cache.current.set(key, s);
        if (cache.current.size > 400) cache.current.delete(cache.current.keys().next().value!);
        setState(s);
      }).catch(() => {});
    }, playing ? 0 : 35);
    return () => {
      clearTimeout(t);
      ctrl.abort();
    };
  }, [loaded, cursor, playing]);

  useEffect(() => {
    if (!loaded || !selectedLoc) {
      setHistory(null);
      return;
    }
    api.history(loaded.summary.id, selectedLoc).then((h) => setHistory({ name: h.name, points: h.points }));
  }, [loaded, selectedLoc]);

  // ── navigation over the client-side timeline arrays ──
  const isBreak = useCallback(
    (i: number) => !!tl && breakpoints.has(tl.line[i]) && (i === 0 || tl.line[i - 1] !== tl.line[i] || tl.tid[i - 1] !== tl.tid[i]),
    [tl, breakpoints],
  );
  const cont = useCallback(
    (dir: 1 | -1) => {
      if (!tl) return;
      for (let i = cursor + dir; i >= 0 && i < total; i += dir) {
        if (isBreak(i)) {
          seek(i);
          say(<>{dir < 0 ? "reverse-continue" : "continue"} → breakpoint at line <b>{tl.line[i]}</b>, step #{i}</>);
          return;
        }
      }
      seek(dir < 0 ? 0 : total - 1);
      say(breakpoints.size ? "no breakpoint hit — ran to the end" : "tip: click a line number to set a breakpoint");
    },
    [tl, cursor, total, isBreak, seek, say, breakpoints],
  );
  const onThread = useCallback(
    (dir: 1 | -1) => {
      if (!tl) return;
      const tid = tl.tid[cursor];
      for (let i = cursor + dir; i >= 0 && i < total; i += dir) {
        if (tl.tid[i] === tid && tl.line[i] !== tl.line[cursor]) return seek(i);
      }
    },
    [tl, cursor, total, seek],
  );

  useEffect(() => {
    if (!playing) return;
    if (cursor >= total - 1) {
      setPlaying(false);
      return;
    }
    const t = window.setTimeout(() => seek(cursor + 1), 45);
    return () => clearTimeout(t);
  }, [playing, cursor, total, seek]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (editing || huntOpen || landing || (e.target as HTMLElement).tagName === "INPUT") return;
      const n = e.shiftKey ? 10 : 1;
      if (e.key === "ArrowLeft") seek(cursor - n);
      else if (e.key === "ArrowRight") seek(cursor + n);
      else if (e.key === "Home") seek(0);
      else if (e.key === "End") seek(total - 1);
      else if (e.key === "[") onThread(-1);
      else if (e.key === "]") onThread(1);
      else if (e.key === "r") cont(-1);
      else if (e.key === "c") cont(1);
      else if (e.key === " ") setPlaying((p) => !p);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [cursor, total, seek, onThread, cont, editing, huntOpen, landing]);

  // ── actions ──
  const pick = (ex: Example) => {
    setActive(ex.name);
    setSource(ex.source);
    setLoaded(null);
    setHunt(null);
    setBreakpoints(new Set());
    setCompileErr(null);
    setEditing(false);
  };

  const record = async (seed = Math.floor(Math.random() * 1e6)) => {
    setBusy(true);
    setCompileErr(null);
    try {
      const s = await api.record(active, source, seed, 0.08);
      await load(s);
      say(<>recorded <b>{s.total_steps}</b> steps into <b>{fmtBytes(s.recording.bytes)}</b> · seed {seed}</>);
      setEditing(false);
    } catch (e) {
      if (e instanceof ApiError) setCompileErr(e.line ? `line ${e.line}:${e.col}  ${e.message}` : e.message);
    } finally {
      setBusy(false);
    }
  };

  const runHunt = async (runs: number, chaos: number) => {
    setHunting(true);
    setDiff(null);
    try {
      setHunt(await api.hunt(active, source, runs, chaos));
    } catch (e) {
      if (e instanceof ApiError) setCompileErr(e.message);
    } finally {
      setHunting(false);
    }
  };

  const openRun = async (seed: number) => {
    if (!hunt) return;
    const s = await api.openRun(hunt.id, seed);
    setHuntOpen(false);
    await load(s);
    say(<>replayed seed <b>{seed}</b> from its recording — {s.outcome.status === "fault" ? "failure reproduced" : "passing run"}</>);
  };

  const compare = async () => {
    if (!hunt || !loaded) return;
    const pass = hunt.runs.find((r) => !r.failed);
    if (!pass) return;
    const good = await api.openRun(hunt.id, pass.seed);
    const d = await api.diff(good.id, loaded.summary.id);
    setDiff(d);
    setHuntOpen(true);
  };

  const investigate = async () => {
    setLanding(false);
    const bank = examples.find((e) => e.name === "bank");
    if (bank) pick(bank);
    setHuntOpen(true);
  };

  const h = loaded?.report.hypotheses[hyp];
  const evidence = useMemo(() => h?.evidence.map((e) => e.step) ?? [], [h]);
  const cur = state?.event;
  const ex = examples.find((e) => e.name === active);

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand" onClick={() => setLanding(true)}>
          <div className="brand-mark" />
          <div className="brand-name">Chrono<em>Forge</em></div>
        </div>
        <div className="tabs">
          {examples.map((e) => (
            <button key={e.name} className={`tab ${view === "debug" && e.name === active ? "on" : ""}`} onClick={() => { setView("debug"); pick(e); }}>
              <span className="dot" />{e.name}
            </button>
          ))}
          <button className={`tab ${view === "native" ? "on" : ""}`} onClick={() => setView("native")}>
            <span className="dot" />native · ptrace
          </button>
        </div>
        <span className="spacer" />
        {loaded && view === "debug" && (
          <div className="meters">
            <div className="meter"><small>steps</small><b>{loaded.summary.total_steps.toLocaleString()}</b></div>
            <div className="meter"><small>threads</small><b>{loaded.summary.threads.length}</b></div>
            <div className="meter"><small>recording</small><b>{fmtBytes(loaded.summary.recording.bytes)}</b></div>
            <div className="meter"><small>replay</small><b>{loaded.summary.timings.replay_ms.toFixed(1)} ms</b></div>
          </div>
        )}
        {view === "debug" && (
          <>
            <button className="btn" onClick={() => { setDiff(null); setHuntOpen(true); }} data-testid="hunt">⚡ hunt</button>
            <button className="btn primary" onClick={() => record()} disabled={busy} data-testid="record">
              {busy ? <span className="spin" /> : <span className="rec-dot" />} record
            </button>
          </>
        )}
      </header>

      {view === "native" ? (
        <div style={{ minHeight: 0, gridRow: "2 / 4", display: "grid" }}><NativeView /></div>
      ) : (
        <>
          <main className="work">
            <section className="pane">
              <div className="pane-head">
                <span className="label">{ex?.file ?? "scratch.forge"}</span>
                <span className="spacer" />
                <button className="btn ghost" style={{ padding: "3px 8px" }} onClick={() => setEditing((e) => !e)}>{editing ? "done" : "edit"}</button>
              </div>
              {compileErr && <div className="compile-err">✕ {compileErr}</div>}
              <div className="pane-body">
                {editing ? (
                  <textarea className="editor" spellCheck={false} value={source} onChange={(e) => setSource(e.target.value)} />
                ) : (
                  <SourceView
                    source={loaded?.summary.source ?? source}
                    summary={loaded?.summary ?? null}
                    report={loaded?.report ?? null}
                    currentLine={cur?.line ?? null}
                    currentTid={cur?.tid ?? null}
                    breakpoints={breakpoints}
                    onToggleBreakpoint={(n) => setBreakpoints((b) => {
                      const nb = new Set(b);
                      if (nb.has(n)) nb.delete(n); else nb.add(n);
                      return nb;
                    })}
                  />
                )}
              </div>
            </section>

            <section className="pane">
              <div className="pane-head"><span className="label">verdict</span></div>
              <div className="pane-body">
                {loaded ? (
                  <Verdict
                    summary={loaded.summary}
                    report={loaded.report}
                    selected={hyp}
                    onSelect={setHyp}
                    cursor={cursor}
                    onSeek={seek}
                    onExplain={() => api.explain(loaded.summary.id, true)}
                    onCompare={hunt && hunt.runs.some((r) => !r.failed) && loaded.summary.outcome.status === "fault" ? compare : null}
                  />
                ) : (
                  <div className="verdict">
                    <div className="verdict-kicker"><span className="pill">no recording yet</span></div>
                    <h1>{ex?.title || "Write some Forge."}</h1>
                    <div className="explain" style={{ marginTop: 10 }}>{ex?.description}</div>
                    <div style={{ display: "flex", gap: 10, marginTop: 22 }}>
                      <button className="btn primary" onClick={() => record()}><span className="rec-dot" /> record one run</button>
                      <button className="btn" onClick={() => setHuntOpen(true)}>⚡ hunt across hundreds of schedules</button>
                    </div>
                  </div>
                )}
              </div>
            </section>

            <section className="pane">
              <Inspector state={state} report={loaded?.report ?? null} cursor={cursor} selectedLoc={selectedLoc} onSelectLoc={setSelectedLoc} onSeek={seek} />
            </section>
          </main>

          <footer className="deck">
            <div className="transport">
              <button className="tbtn" onClick={() => seek(0)} title="start (Home)">⏮</button>
              <button className="tbtn" onClick={() => cont(-1)} title="reverse-continue to previous breakpoint (r)">◀◀<span className="k">r</span></button>
              <button className="tbtn" onClick={() => seek(cursor - 1)} title="step back (←)">◀<span className="k">←</span></button>
              <button className="tbtn big" onClick={() => setPlaying((p) => !p)} title="play (space)">{playing ? "❚❚" : "▶"}</button>
              <button className="tbtn" onClick={() => seek(cursor + 1)} title="step (→)">▶<span className="k">→</span></button>
              <button className="tbtn" onClick={() => cont(1)} title="continue to next breakpoint (c)">▶▶<span className="k">c</span></button>
              <button className="tbtn" onClick={() => seek(total - 1)} title="end (End)">⏭</button>
              <div className="counter"><b>{loaded ? cursor : "—"}</b><span>/ {total ? total - 1 : 0}</span></div>
              <span className="spacer" />
              <span className="hint">
                <kbd>←</kbd><kbd>→</kbd> step · <kbd>[</kbd><kbd>]</kbd> same thread · <kbd>r</kbd> reverse-continue · click a name to plot it · <kbd>⟲</kbd> who wrote it
              </span>
              {loaded && <a className="btn ghost" href={api.exportUrl(loaded.summary.id)} style={{ textDecoration: "none" }}>⤓ .cfrec</a>}
            </div>
            {loaded ? (
              <Timeline
                data={loaded.timeline}
                summary={loaded.summary}
                races={loaded.report.races}
                cursor={cursor}
                onSeek={seek}
                history={history}
                evidence={evidence}
              />
            ) : (
              <div className="tape" style={{ display: "grid", placeItems: "center", cursor: "default" }}>
                <span className="hint">no tape loaded — <kbd>record</kbd> a run or <kbd>hunt</kbd> for a failing one</span>
              </div>
            )}
          </footer>
        </>
      )}

      {huntOpen && (
        <HuntSheet
          name={active}
          hunt={hunt}
          running={hunting}
          diff={diff}
          onRun={runHunt}
          onOpen={openRun}
          onClose={() => { setHuntOpen(false); setDiff(null); }}
        />
      )}
      {landing && <Landing onInvestigate={investigate} onSkip={() => setLanding(false)} />}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
