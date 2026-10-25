import { useEffect, useMemo, useRef } from "react";
import { highlight, threadColor } from "../highlight";
import type { Report, Summary } from "../api";

interface Props {
  source: string;
  summary: Summary | null;
  report: Report | null;
  currentLine: number | null;
  currentTid: number | null;
  breakpoints: Set<number>;
  onToggleBreakpoint: (line: number) => void;
}

export function SourceView({ source, summary, report, currentLine, currentTid, breakpoints, onToggleBreakpoint }: Props) {
  const lines = useMemo(() => source.split("\n"), [source]);
  const heat = useMemo(() => new Map(summary?.line_heat ?? []), [summary]);
  const maxHeat = useMemo(() => Math.max(1, ...heat.values()), [heat]);
  const slice = useMemo(() => new Set(report?.slice?.lines ?? []), [report]);
  const raceLines = useMemo(() => {
    const m = new Map<number, string>();
    report?.races.forEach((r) => {
      m.set(r.first.line, r.name);
      m.set(r.second.line, r.name);
    });
    return m;
  }, [report]);
  const faultLine = summary?.outcome.line ?? null;
  const box = useRef<HTMLDivElement>(null);

  // keep the current line in view while scrubbing
  useEffect(() => {
    if (!currentLine || !box.current) return;
    const el = box.current.querySelector<HTMLElement>(`[data-line="${currentLine}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [currentLine]);

  return (
    <div className="source" ref={box}>
      {lines.map((text, i) => {
        const n = i + 1;
        const h = heat.get(n) ?? 0;
        const cls = ["src-line"];
        if (breakpoints.has(n)) cls.push("has-bp");
        if (slice.has(n)) cls.push("slice");
        if (n === faultLine) cls.push("fault");
        if (n === currentLine) cls.push("cur");
        return (
          <div
            key={n}
            data-line={n}
            className={cls.join(" ")}
            style={n === currentLine && currentTid !== null ? ({ "--cur": threadColor(currentTid) } as React.CSSProperties) : undefined}
            onClick={() => onToggleBreakpoint(n)}
            title={h ? `executed ${h}×  ·  click to toggle breakpoint` : "click to toggle breakpoint"}
          >
            <span className="bp" />
            <span className="ln">
              {n}
              {h > 0 && <span className="heat" style={{ opacity: 0.15 + 0.85 * (h / maxHeat) }} />}
            </span>
            <span className="code">{highlight(text)}</span>
            {n === faultLine ? (
              <span className="tag fault">✕ {summary?.outcome.kind}</span>
            ) : raceLines.has(n) ? (
              <span className="tag race">⚡ race · {raceLines.get(n)}</span>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}
