import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { F, type HistoryPoint, type Race, type Summary, type TimelineData } from "../api";
import { threadHex } from "../highlight";

interface Props {
  data: TimelineData;
  summary: Summary;
  races: Race[];
  cursor: number;
  onSeek: (step: number) => void;
  history: { name: string; points: HistoryPoint[] } | null;
  evidence: number[];
}

const GUTTER = 96;
const WAVE = 44;
const RULER = 18;
const MINIMAP = 16;

export function Timeline({ data, summary, races, cursor, onSeek, history, evidence }: Props) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState({ w: 800, h: 210 });
  const total = data.tid.length;
  const [view, setView] = useState<[number, number]>([0, Math.max(total, 1)]);
  const [hover, setHover] = useState<{ x: number; y: number; step: number } | null>(null);
  const drag = useRef<null | "seek" | "mini">(null);

  // new session → show everything
  useEffect(() => setView([0, Math.max(total, 1)]), [total, summary.id]);

  useEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setSize({ w: e.contentRect.width, h: e.contentRect.height }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // keep the playhead visible when stepping outside the zoomed window
  useEffect(() => {
    setView(([a, b]) => {
      if (cursor >= a && cursor < b) return [a, b];
      const span = b - a;
      const start = Math.max(0, Math.min(total - span, cursor - span / 2));
      return [start, start + span];
    });
  }, [cursor, total]);

  const lanes = useMemo(() => summary.threads.map((t) => t.tid), [summary]);
  const laneTop = history ? WAVE + 6 : 6;
  const laneArea = size.h - laneTop - RULER - MINIMAP;
  const laneH = Math.max(14, Math.min(46, laneArea / Math.max(1, lanes.length)));
  const laneY = (tid: number) => laneTop + lanes.indexOf(tid) * laneH;

  const plotW = size.w - GUTTER - 12;
  const [v0, v1] = view;
  const span = Math.max(1, v1 - v0);
  const xOf = useCallback((step: number) => GUTTER + ((step - v0) / span) * plotW, [v0, span, plotW]);
  const stepAt = useCallback(
    (x: number) => Math.max(0, Math.min(total - 1, Math.floor(v0 + ((x - GUTTER) / plotW) * span))),
    [v0, span, plotW, total],
  );

  // density of the whole trace for the minimap, computed once per session
  const density = useMemo(() => {
    const cols = 600;
    const d = new Float32Array(cols);
    const bad = new Uint8Array(cols);
    for (let i = 0; i < total; i++) {
      const c = Math.floor((i / total) * cols);
      d[c] += 1;
      if (data.flags[i] & (F.RACE | F.FAULT)) bad[c] = 1;
    }
    const max = Math.max(1, ...d);
    return { cols, d: Array.from(d, (x) => x / max), bad };
  }, [data, total]);

  useEffect(() => {
    const c = canvas.current;
    if (!c) return;
    const dpr = window.devicePixelRatio || 1;
    c.width = size.w * dpr;
    c.height = size.h * dpr;
    const g = c.getContext("2d")!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, size.w, size.h);
    g.font = "11px 'JetBrains Mono', monospace";
    g.textBaseline = "middle";

    // lane backgrounds + labels
    lanes.forEach((tid, i) => {
      const y = laneY(tid);
      g.fillStyle = i % 2 ? "#0f0f12" : "#0c0c0e";
      g.fillRect(GUTTER, y, plotW, laneH - 1);
      g.fillStyle = threadHex(tid);
      g.fillRect(10, y + laneH / 2 - 4, 8, 8);
      g.fillStyle = "#ece9e2";
      const name = summary.threads.find((t) => t.tid === tid)?.name ?? "";
      g.fillText(`T${tid}`, 24, y + laneH / 2);
      g.fillStyle = "#5c5a55";
      g.fillText(name.length > 9 ? name.slice(0, 8) + "…" : name, 48, y + laneH / 2);
    });

    // steps. When zoomed out several steps share a pixel column; draw each
    // column once per lane, letting "interesting" steps win.
    const pxPer = plotW / span;
    const w = Math.max(1, pxPer - (pxPer > 4 ? 1 : 0));
    const lo = Math.max(0, Math.floor(v0));
    const hi = Math.min(total, Math.ceil(v1));
    const lastCol = new Map<number, number>();
    for (let i = lo; i < hi; i++) {
      const tid = data.tid[i];
      const f = data.flags[i];
      const x = xOf(i);
      const col = Math.floor(x);
      const interesting = f & (F.SHARED_WRITE | F.RACE | F.FAULT | F.SYNC | F.NONDET | F.SLICE);
      if (lastCol.get(tid) === col && !interesting) continue;
      lastCol.set(tid, col);
      const y = laneY(tid);
      const mid = y + laneH / 2;
      g.globalAlpha = 0.5;
      g.fillStyle = threadHex(tid);
      g.fillRect(x, mid - laneH * 0.18, w, laneH * 0.36);
      g.globalAlpha = 1;
      if (f & F.SHARED_WRITE) {
        g.fillRect(x, y + 3, w, laneH - 7);
      } else if (f & F.SHARED_READ) {
        g.globalAlpha = 0.8;
        g.fillRect(x, mid - laneH * 0.3, w, laneH * 0.6);
        g.globalAlpha = 1;
      }
      if (f & F.SYNC) {
        g.fillStyle = "#4fd1c5";
        g.fillRect(x, y + 1, Math.max(2, w), 3);
      }
      if (f & F.NONDET) {
        g.fillStyle = "#f6d860";
        g.beginPath();
        g.moveTo(x + w / 2, y + 2);
        g.lineTo(x + w / 2 + 4, y + 7);
        g.lineTo(x + w / 2, y + 12);
        g.lineTo(x + w / 2 - 4, y + 7);
        g.fill();
      }
      if (f & F.SLICE) {
        g.fillStyle = "#ff6b1a";
        g.fillRect(x, y + laneH - 4, Math.max(2, w), 3);
      }
      if (f & F.FAULT) {
        g.fillStyle = "#ff5252";
        g.fillRect(x - 1, laneTop, 2, laneArea);
        g.fillText("✕", x + 5, y + 8);
      }
    }

    // spawn/join wires between lanes
    g.lineWidth = 1;
    for (const s of data.sync) {
      if (s.step < lo || s.step >= hi) continue;
      if ((s.kind === "spawn" || s.kind === "join") && s.target !== null && lanes.includes(s.target)) {
        const x = xOf(s.step) + w / 2;
        const ya = laneY(s.tid) + laneH / 2;
        const yb = laneY(s.target) + laneH / 2;
        g.strokeStyle = s.kind === "spawn" ? "#4fd1c5aa" : "#7ee2a8aa";
        g.setLineDash(s.kind === "join" ? [3, 3] : []);
        g.beginPath();
        g.moveTo(x, ya);
        g.lineTo(x, yb);
        g.stroke();
        g.setLineDash([]);
      }
      if (s.kind === "block") {
        g.fillStyle = "#ff5252";
        const x = xOf(s.step);
        g.fillRect(x, laneY(s.tid) + laneH - 6, Math.max(3, w), 4);
      }
    }

    // race arcs: the two conflicting accesses, joined over the lanes
    for (const r of races.slice(0, 12)) {
      const a = r.first, b = r.second;
      if (b.step < lo || a.step >= hi) continue;
      const xa = xOf(a.step) + w / 2, xb = xOf(b.step) + w / 2;
      const ya = laneY(a.tid) + 4, yb = laneY(b.tid) + 4;
      const lift = Math.min(40, 14 + Math.abs(xb - xa) * 0.2);
      g.strokeStyle = "#ffb547";
      g.lineWidth = 1.5;
      g.setLineDash([4, 3]);
      g.beginPath();
      g.moveTo(xa, ya);
      g.bezierCurveTo(xa, Math.min(ya, yb) - lift, xb, Math.min(ya, yb) - lift, xb, yb);
      g.stroke();
      g.setLineDash([]);
      g.fillStyle = "#ffb547";
      g.fillText("⚡", (xa + xb) / 2 - 4, Math.min(ya, yb) - lift * 0.75);
    }

    // evidence pins from the selected hypothesis
    evidence.forEach((s, i) => {
      if (s < lo || s >= hi || s >= total) return;
      const x = xOf(s) + w / 2;
      const y = laneY(data.tid[s]);
      g.fillStyle = "#ff6b1a";
      g.beginPath();
      g.arc(x, y + laneH / 2, 7, 0, Math.PI * 2);
      g.fill();
      g.fillStyle = "#0b0b0c";
      g.font = "bold 9px 'JetBrains Mono', monospace";
      g.fillText(String(i + 1), x - 3, y + laneH / 2 + 0.5);
      g.font = "11px 'JetBrains Mono', monospace";
    });

    // the value waveform: an oscilloscope trace of the selected variable
    if (history && history.points.length) {
      const pts = history.points.filter((p) => p.int !== null);
      const top = 6, h = WAVE - 10;
      g.fillStyle = "#0e0e10";
      g.fillRect(GUTTER, top, plotW, h + 4);
      g.fillStyle = "#ff6b1a";
      g.fillText(history.name, 12, top + h / 2);
      if (pts.length) {
        const vals = pts.map((p) => p.int!);
        const mn = Math.min(0, ...vals), mx = Math.max(1, ...vals);
        const yOf = (v: number) => top + h - ((v - mn) / (mx - mn || 1)) * h + 2;
        g.strokeStyle = "#ff6b1a";
        g.lineWidth = 1.5;
        g.shadowColor = "#ff6b1a";
        g.shadowBlur = 6;
        g.beginPath();
        let prev = yOf(0);
        g.moveTo(GUTTER, prev);
        for (const p of pts) {
          const x = Math.max(GUTTER, Math.min(GUTTER + plotW, xOf(p.step)));
          g.lineTo(x, prev);
          prev = yOf(p.int!);
          g.lineTo(x, prev);
        }
        g.lineTo(GUTTER + plotW, prev);
        g.stroke();
        g.shadowBlur = 0;
        g.fillStyle = "#5c5a55";
        g.fillText(String(mx), GUTTER - 30, top + 4);
        g.fillText(String(mn), GUTTER - 30, top + h);
      }
    }

    // ruler
    const ry = size.h - RULER - MINIMAP;
    g.fillStyle = "#5c5a55";
    g.font = "10px 'JetBrains Mono', monospace";
    const tick = niceTick(span / 8);
    for (let s = Math.ceil(v0 / tick) * tick; s < v1; s += tick) {
      const x = xOf(s);
      g.fillRect(x, ry, 1, 4);
      g.fillText(String(s), x + 3, ry + 10);
    }

    // minimap
    const my = size.h - MINIMAP + 2;
    for (let c = 0; c < density.cols; c++) {
      const x = GUTTER + (c / density.cols) * plotW;
      g.fillStyle = density.bad[c] ? "#ffb547" : "#3a3a44";
      const hh = 2 + density.d[c] * (MINIMAP - 6);
      g.fillRect(x, my + (MINIMAP - 4 - hh), Math.max(1, plotW / density.cols), hh);
    }
    g.strokeStyle = "#ff6b1a88";
    g.strokeRect(GUTTER + (v0 / total) * plotW, my - 1, Math.max(3, (span / total) * plotW), MINIMAP - 3);

    // playhead
    if (cursor >= lo && cursor <= hi) {
      const x = xOf(cursor) + w / 2;
      g.fillStyle = "#ff6b1a";
      g.fillRect(x - 0.75, 0, 1.5, size.h - MINIMAP);
      g.beginPath();
      g.moveTo(x - 6, 0);
      g.lineTo(x + 6, 0);
      g.lineTo(x, 8);
      g.fill();
      const tid = data.tid[cursor];
      if (tid !== undefined) {
        g.strokeStyle = "#ff6b1a";
        g.lineWidth = 2;
        g.strokeRect(x - w / 2 - 2, laneY(tid) + 1, w + 4, laneH - 3);
      }
    }
  }, [data, summary, races, cursor, size, view, history, evidence, lanes, laneH, laneTop, laneArea, plotW, span, v0, v1, xOf, total, density]);

  const local = (e: React.MouseEvent) => {
    const r = canvas.current!.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };

  const onDown = (e: React.MouseEvent) => {
    const { x, y } = local(e);
    if (x < GUTTER) return;
    if (y > size.h - MINIMAP) {
      drag.current = "mini";
      panTo(x);
    } else {
      drag.current = "seek";
      onSeek(stepAt(x));
    }
  };

  const panTo = (x: number) => {
    const center = ((x - GUTTER) / plotW) * total;
    const start = Math.max(0, Math.min(total - span, center - span / 2));
    setView([start, start + span]);
  };

  const onMove = (e: React.MouseEvent) => {
    const { x, y } = local(e);
    if (drag.current === "seek") onSeek(stepAt(x));
    if (drag.current === "mini") panTo(x);
    setHover(x > GUTTER && y < size.h - MINIMAP ? { x, y, step: stepAt(x) } : null);
  };

  const onWheel = (e: React.WheelEvent) => {
    const { x } = local(e);
    if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      const d = (e.deltaX / plotW) * span;
      const start = Math.max(0, Math.min(total - span, v0 + d));
      setView([start, start + span]);
      return;
    }
    const anchor = v0 + ((x - GUTTER) / plotW) * span;
    const factor = Math.exp(e.deltaY * 0.0015);
    const ns = Math.max(24, Math.min(total, span * factor));
    let start = anchor - ((x - GUTTER) / plotW) * ns;
    start = Math.max(0, Math.min(total - ns, start));
    setView([start, start + ns]);
  };

  const hv = hover ? hover.step : null;
  return (
    <div
      className="tape"
      ref={wrap}
      onMouseDown={onDown}
      onMouseMove={onMove}
      onMouseUp={() => (drag.current = null)}
      onMouseLeave={() => {
        drag.current = null;
        setHover(null);
      }}
      onWheel={onWheel}
    >
      <canvas ref={canvas} />
      {hover && hv !== null && (
        <div className="tooltip" style={{ left: hover.x, top: hover.y - 10 }}>
          <span style={{ color: "#ff6b1a" }}>#{hv}</span>{" "}
          <span style={{ color: threadHex(data.tid[hv]) }}>T{data.tid[hv]}</span> · line {data.line[hv]} · {data.ops[data.op[hv]]}
          {data.flags[hv] & F.RACE ? " · ⚡race" : ""}
          {data.flags[hv] & F.SWITCH ? " · ⇄ switch" : ""}
          {data.flags[hv] & F.NONDET ? " · ◆ nondet" : ""}
        </div>
      )}
    </div>
  );
}

function niceTick(raw: number) {
  const p = Math.pow(10, Math.floor(Math.log10(Math.max(1, raw))));
  for (const m of [1, 2, 5, 10]) if (m * p >= raw) return m * p;
  return 10 * p;
}
