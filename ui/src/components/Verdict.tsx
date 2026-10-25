import { useState } from "react";
import type { Explanation, Hypothesis, Report, Summary } from "../api";
import { threadColor } from "../highlight";

interface Props {
  summary: Summary;
  report: Report;
  selected: number;
  onSelect: (i: number) => void;
  cursor: number;
  onSeek: (step: number) => void;
  onExplain: () => Promise<Explanation>;
  onCompare: (() => void) | null;
}

// `code` spans in explanations come back as backticks; render them as mono
function rich(text: string) {
  return text.split(/(`[^`]+`)/g).map((part, i) =>
    part.startsWith("`") ? <code key={i}>{part.slice(1, -1)}</code> : <span key={i}>{part}</span>,
  );
}

function markdown(text: string) {
  return text.split(/\n\n+/).map((para, i) => (
    <p key={i}>
      {para.split(/(\*\*[^*]+\*\*)/g).map((p, j) =>
        p.startsWith("**") ? <strong key={j}>{p.slice(2, -2)}</strong> : <span key={j}>{rich(p)}</span>,
      )}
    </p>
  ));
}

function headline(h: Hypothesis | undefined, summary: Summary) {
  if (!h) return summary.outcome.status === "completed" ? <>Clean run. <em>Nothing to blame.</em></> : <>{summary.outcome.kind}</>;
  const m = h.title.match(/^(.*?)`([^`]+)`(.*)$/);
  return m ? <>{m[1]}<em>{m[2]}</em>{m[3]}</> : <>{h.title}</>;
}

export function Verdict({ summary, report, selected, onSelect, cursor, onSeek, onExplain, onCompare }: Props) {
  const [story, setStory] = useState<Explanation | null>(null);
  const [busy, setBusy] = useState(false);
  const h = report.hypotheses[selected];
  const failed = summary.outcome.status !== "completed";

  return (
    <div>
      <div className="verdict">
        <div className={`verdict-kicker ${failed ? "bad" : "good"}`}>
          <span className="pill">{failed ? "failure reproduced" : "run passed"}</span>
          <span>
            {failed
              ? `${summary.outcome.kind} · line ${summary.outcome.line} · T${summary.outcome.tid} · step ${summary.outcome.step}`
              : `${summary.total_steps} steps · ${report.races.length} latent race${report.races.length === 1 ? "" : "s"}`}
          </span>
        </div>
        <h1>{headline(h, summary)}</h1>
        {failed && <div className="msg">“{summary.outcome.message}”</div>}

        {h && (
          <>
            <div className="confidence">
              <span>confidence</span>
              <span className="bar"><i style={{ width: `${h.confidence * 100}%` }} /></span>
              <span>{Math.round(h.confidence * 100)}%</span>
              <span className="faint">· {h.kind}</span>
            </div>
            <div className="explain">{rich(h.explanation)}</div>
            <div className="evidence">
              {h.evidence.map((e, i) => (
                <div key={i} className={`ev ${e.step === cursor ? "on" : ""}`} onClick={() => onSeek(e.step)} title="jump here">
                  <span className="role">{i + 1} · {e.role}</span>
                  <span className="tid" style={{ color: threadColor(e.tid) }}>T{e.tid}</span>
                  <span className="faint">#{e.step}</span>
                  <span className="detail">line {e.line} — <b>{e.detail}</b></span>
                </div>
              ))}
            </div>
            {h.fix && <div className="fix"><b>FIX</b>{rich(h.fix)}</div>}
          </>
        )}

        <div className="others">
          {report.hypotheses.map((x, i) => (
            <button key={i} className={`chip ${i === selected ? "on" : ""}`} onClick={() => onSelect(i)}>
              {x.title.replace(/`/g, "")}<i>{Math.round(x.confidence * 100)}%</i>
            </button>
          ))}
          <button
            className="chip"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              try {
                setStory(await onExplain());
              } finally {
                setBusy(false);
              }
            }}
          >
            {busy ? <span className="spin" /> : "✦ write it up"}
          </button>
          {onCompare && <button className="chip" onClick={onCompare}>⇄ diff against a passing run</button>}
        </div>
      </div>
      {story && (
        <div className="narrative">
          <div className="via">{story.mode === "llm" ? `narrated by ${story.model} · grounded: ${story.grounded}` : "deterministic write-up · no LLM"}</div>
          {markdown(story.narrative)}
          {story.note && <div className="faint mono" style={{ fontSize: 11 }}>{story.note}</div>}
        </div>
      )}
    </div>
  );
}
