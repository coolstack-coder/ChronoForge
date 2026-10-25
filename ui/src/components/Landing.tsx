import { useEffect, useState } from "react";

const LOG = [
  ["03:07:12.481", "ledger-svc", "ERROR", "invariant violated: money created: balance=50 paid=130"],
  ["03:07:12.482", "pager", "WARN", "SEV-2 opened, on-call paged"],
  ["03:41:09.003", "you", "INFO", "re-ran it 40 times locally. passes every time."],
  ["04:15:55.310", "you", "INFO", "added logging. now it never fails. great."],
];

export function Landing({ onInvestigate, onSkip }: { onInvestigate: () => void; onSkip: () => void }) {
  const [shown, setShown] = useState(0);
  useEffect(() => {
    if (shown >= LOG.length) return;
    const t = setTimeout(() => setShown((s) => s + 1), shown === 0 ? 500 : 650);
    return () => clearTimeout(t);
  }, [shown]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Enter" && onInvestigate();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onInvestigate]);

  return (
    <div className="landing">
      <div className="landing-inner">
        <div className="clock"><i /> 03:07 AM · PRODUCTION · ledger-service</div>
        <h1>It crashed once.<br />It will never crash <em>for you</em>.</h1>
        <p className="lede">
          Heisenbugs live in thread interleavings, clocks and random seeds — exactly the things that change every time you re-run.
          ChronoForge records only that nondeterminism, replays the run bit-for-bit, and lets you walk it <em>backwards</em> until the
          real cause is staring at you.
        </p>
        <div className="log-tail">
          {LOG.slice(0, shown).map(([t, who, lvl, msg]) => (
            <div key={t}>
              <span>{t}</span> <span>[{who}]</span> <span className={lvl === "ERROR" ? "e" : ""}>{lvl}</span> {msg}
            </div>
          ))}
          {shown < LOG.length && <span className="spin" />}
        </div>
        <div className="steps3">
          <div className="step3"><div className="n">01 · RECORD</div><h3>Capture the chaos</h3><p>Schedules, clocks, randomness, input. A run fits in a few hundred bytes.</p></div>
          <div className="step3"><div className="n">02 · REWIND</div><h3>Step backwards</h3><p>Checkpoints + deterministic re-execution: any instruction, any thread, any time.</p></div>
          <div className="step3"><div className="n">03 · EXPLAIN</div><h3>Blame with evidence</h3><p>Happens-before races, lock cycles and dynamic slices, ranked into a verdict.</p></div>
        </div>
        <div style={{ display: "flex", gap: 12 }}>
          <button className="btn primary" style={{ padding: "11px 18px", fontSize: 13 }} onClick={onInvestigate}>
            <span className="rec-dot" /> Investigate the 3 AM incident ↵
          </button>
          <button className="btn" style={{ padding: "11px 18px", fontSize: 13 }} onClick={onSkip}>Open the debugger</button>
        </div>
      </div>
    </div>
  );
}
