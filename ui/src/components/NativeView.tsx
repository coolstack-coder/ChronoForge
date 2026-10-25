import { useEffect, useMemo, useState } from "react";
import { api, type NativeRecording, type NativeSummary } from "../api";

const hex = (n: number) => "0x" + n.toString(16);

// little-endian decode of a global's bytes, for 1/2/4/8-byte ints
function asInt(bytes: number[]): string {
  if (![1, 2, 4, 8].includes(bytes.length)) return "";
  let v = 0n;
  bytes.forEach((b, i) => (v |= BigInt(b) << BigInt(8 * i)));
  const bits = BigInt(bytes.length * 8);
  if (v >> (bits - 1n)) v -= 1n << bits;
  return v.toString();
}

export function NativeView() {
  const [list, setList] = useState<NativeSummary[] | null>(null);
  const [rec, setRec] = useState<NativeRecording | null>(null);
  const [sel, setSel] = useState(0);

  useEffect(() => {
    api.native().then((l) => {
      setList(l);
      if (l[0]) api.nativeGet(l[0].id).then(setRec);
    });
  }, []);

  const crashAt = useMemo(() => rec?.events.findIndex((e) => "Signal" in e.kind) ?? -1, [rec]);
  useEffect(() => {
    if (rec) setSel(crashAt >= 0 ? crashAt : rec.events.length - 1);
  }, [rec, crashAt]);

  if (!list) return <div className="empty"><span className="spin" /></div>;
  if (!rec)
    return (
      <div className="empty" style={{ maxWidth: 680 }}>
        No native recordings yet. On Linux:
        <pre className="mono" style={{ color: "var(--paper)" }}>chronoforge native record -o examples/native/recordings/flaky.json --stdin-text "GET /\nGET /health\n" -- ./flaky</pre>
      </div>
    );

  const ev = rec.events[sel];
  const globals = rec.global_history[sel] ?? [];
  const sum = list[0];
  return (
    <div className="native" style={{ height: "100%" }}>
      <div className="pane">
        <div className="pane-head"><span className="label">syscall tape</span><span className="spacer" /><span>{rec.events.length} events</span></div>
        <div className="pane-body">
          {rec.events.map((e, i) => {
            const k = e.kind;
            const isSig = "Signal" in k;
            return (
              <div key={i} className={`syscall ${i === sel ? "on" : ""} ${isSig ? "sig" : ""}`} onClick={() => setSel(i)}>
                <span className="i">{i}</span>
                {"Syscall" in k ? (
                  <span className={`pol ${k.Syscall.policy === "Emulated" ? "emu" : "pass"}`}>{k.Syscall.policy === "Emulated" ? "EMU" : "PASS"}</span>
                ) : <span className="pol emu" style={{ background: "#ff525233", color: "#ff5252" }}>{isSig ? "SIG" : "EXIT"}</span>}
                <span>{"Syscall" in k ? k.Syscall.name : "Signal" in k ? k.Signal.name : `exit ${k.Exit.code}`}</span>
                <span className="d">{"Syscall" in k ? k.Syscall.detail : "Signal" in k ? `fault @ ${hex(k.Signal.fault_addr ?? 0)}` : ""}</span>
              </div>
            );
          })}
        </div>
      </div>
      <div className="pane">
        <div className="pane-head"><span className="label">event #{sel}</span><span className="spacer" /><span>{rec.arch} · linux {rec.kernel}</span></div>
        <div className="pane-body verdict">
          <div className={`verdict-kicker ${crashAt >= 0 ? "bad" : "good"}`}>
            <span className="pill">{crashAt >= 0 ? `crash reproduced · ${sum?.signal}` : "clean exit"}</span>
            <span>{rec.program.split("/").pop()} · {sum?.emulated} syscalls emulated on replay</span>
          </div>
          <h1 style={{ fontSize: 34 }}>
            {"Syscall" in ev.kind ? <>{ev.kind.Syscall.name}<em>()</em></> : "Signal" in ev.kind ? <><em>{ev.kind.Signal.name}</em> in <span className="mono" style={{ fontSize: 28 }}>{ev.symbol}</span></> : <>exit</>}
          </h1>
          <div className="msg">{ev.symbol ?? "?"} · pc {hex(ev.regs.pc)}</div>
          {"Syscall" in ev.kind && (
            <div className="explain" style={{ marginTop: 14 }}>
              <div className="mono" style={{ fontSize: 12 }}>{ev.kind.Syscall.detail}</div>
              <p>
                {ev.kind.Syscall.policy === "Emulated"
                  ? "Nondeterministic: on replay the kernel is skipped and the recorded result (plus any bytes it wrote into the process) is injected instead."
                  : "Deterministic given the same address space: executed for real on replay, and the sequence is checked for divergence."}
              </p>
              <div className="mono faint" style={{ fontSize: 11 }}>args {ev.kind.Syscall.args.map(hex).join(" ")} → ret {ev.kind.Syscall.ret}</div>
              {ev.kind.Syscall.outputs.map((o, i) => (
                <div key={i} className="mono" style={{ fontSize: 11, marginTop: 6 }}>
                  <span className="faint">{hex(o.addr)} ←</span> {o.bytes.slice(0, 24).map((b) => b.toString(16).padStart(2, "0")).join(" ")}
                </div>
              ))}
            </div>
          )}
          <div className="section" style={{ padding: "18px 0 0" }}>
            <h4>pinned nondeterminism</h4>
            <div className="mono" style={{ fontSize: 12, lineHeight: 1.8 }}>
              <div>ASLR <span className="muted">off (personality ADDR_NO_RANDOMIZE)</span></div>
              <div>vDSO <span className="muted">patched → real syscalls: {rec.vdso_patched.join(", ") || "none"}</span></div>
              <div>AT_RANDOM <span className="muted">16 bytes recorded and restored at exec</span></div>
            </div>
          </div>
          <div className="section" style={{ padding: "14px 0 0" }}>
            <h4>stdout</h4>
            {rec.stdout.length ? (
              <pre className="mono" style={{ fontSize: 12, margin: 0 }}>{String.fromCharCode(...rec.stdout.slice(0, 2000))}</pre>
            ) : (
              <div className="muted" style={{ fontSize: 12 }}>nothing — it died on the first request, before printf ever ran</div>
            )}
          </div>
        </div>
      </div>
      <div className="pane">
        <div className="pane-head"><span className="label">process at #{sel}</span></div>
        <div className="pane-body">
          <div className="section">
            <h4>globals (from .data/.bss)</h4>
            <div className="vars">
              {rec.globals.map((g, i) => (
                <div key={g.name} className="var">
                  <span className="name">{g.name}</span>
                  <span className="val">{asInt(globals[i]?.bytes ?? [])}</span>
                  <span className="faint" style={{ fontSize: 10 }}>{(globals[i]?.bytes ?? []).slice(0, 8).map((b) => b.toString(16).padStart(2, "0")).join("")}</span>
                </div>
              ))}
            </div>
          </div>
          <div className="section">
            <h4>registers</h4>
            <div className="regs">
              {ev.regs.regs.map(([n, v]) => (
                <div key={n}><span>{n}</span>{hex(v)}</div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
