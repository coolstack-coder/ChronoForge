// Types mirror the Rust server's JSON. Keep them in sync with crates/cf-server.

export type Step = number;

export interface ThreadInfo { tid: number; name: string; first_step: Step; last_step: Step; steps: number }
export interface OutcomeView {
  status: "completed" | "fault" | "step_limit";
  kind: string | null; message: string | null; step: Step | null; tid: number | null; line: number | null;
}
export interface Summary {
  id: string; name: string; origin: "record" | "hunt" | "import"; source: string; seed: number; chaos: number;
  total_steps: number; threads: ThreadInfo[]; outcome: OutcomeView;
  recording: { events: number; bytes: number; schedule: number; rand: number; clock: number; input: number };
  timings: { record_ms: number; replay_ms: number; analysis_ms: number };
  checkpoints: number;
  plan: { instructions: number; access_probes: number; shared_probes: number; sync_probes: number; nondet_probes: number };
  line_heat: [number, number][];
  output: [Step, number, string][];
  engine: string;
}

export interface SyncMark { step: Step; tid: number; kind: string; target: number | null }
export interface TimelineData { tid: number[]; line: number[]; op: number[]; flags: number[]; sync: SyncMark[]; ops: string[] }

export const F = {
  SHARED_READ: 1, SHARED_WRITE: 2, SYNC: 4, NONDET: 8, SWITCH: 16, SLICE: 32, RACE: 64, FAULT: 128, OUTPUT: 256,
} as const;

export interface Var { name: string; loc: string; value: string; kind: string; writer: Step | null }
export interface FrameSnap { func: string; line: number; locals: Var[] }
export interface ThreadSnap {
  tid: number; name: string; state: string; line: number; frames: FrameSnap[]; stack: string[];
  spawned_at: Step; finished_at: Step | null;
}
export interface Snapshot {
  position: number; current: number; threads: ThreadSnap[]; globals: Var[];
  heap: { id: number; items: Var[] }[]; mutexes: { id: number; owner: number | null; waiters: number[] }[];
  output: { step: Step; tid: number; text: string }[];
}
export interface AccessView { loc: string; name: string; value: string; old: string | null; writer: Step | null; shared: boolean }
export interface StepView {
  step: Step; tid: number; line: number; func: string; op: string; what: string; code: string;
  reads: AccessView[]; writes: AccessView[]; deps: Step[]; ctrl: Step | null; sync: string | null; nondet: string | null;
}
export interface StateView { snapshot: Snapshot; event: StepView | null; seek_ms: number }

export interface Access { step: Step; tid: number; line: number; kind: "read" | "write"; value: string }
export interface Race { loc: string; name: string; kind: string; first: Access; second: Access; occurrences: number }
export interface LockEdge { from: number; to: number; tid: number; step: Step; line: number }
export interface Evidence { step: Step; tid: number; line: number; role: string; detail: string }
export interface Hypothesis { kind: string; title: string; explanation: string; confidence: number; evidence: Evidence[]; fix: string | null }
export interface SliceNode { step: Step; tid: number; line: number; op: string; depth: number; what: string; deps: Step[] }
export interface Report {
  fault: { kind: unknown; message: string; step: Step; tid: number; line: number } | null;
  stats: { steps: number; threads: number; shared_accesses: number; sync_events: number; nondet_events: number; recording_events: number };
  races: Race[];
  locks: { nodes: [number, string][]; edges: LockEdge[]; cycles: { mutexes: string[]; edges: LockEdge[]; description: string }[] };
  slice: { criterion: Step; nodes: SliceNode[]; lines: number[]; truncated: boolean } | null;
  hypotheses: Hypothesis[];
}

export interface HuntRun { seed: number; failed: boolean; kind: string | null; message: string | null; line: number | null; steps: number; switches: number; signature: string }
export interface Hunt { id: string; name: string; runs: HuntRun[]; failures: number; signatures: [string, number][]; elapsed_ms: number }

export interface Brief { step: Step; tid: number; line: number; code: string; what: string }
export interface Divergence {
  kind: string; index: number; common_prefix: number; a: Brief | null; b: Brief | null;
  explanation: string; outcome_a: string; outcome_b: string;
  data: {
    name: string; write: number; explanation: string;
    a: { step: Step; tid: number; line: number; value: string } | null;
    b: { step: Step; tid: number; line: number; value: string } | null;
  } | null;
}
export interface Explanation { mode: "llm" | "offline"; model: string | null; narrative: string; cited_steps: Step[]; grounded: boolean; note: string | null }
export interface HistoryPoint { step: Step; tid: number; line: number; old: string; new: string; int: number | null }
export interface Example { name: string; file: string; title: string; description: string; source: string }

export interface NativeSummary {
  id: string; program: string; arch: string; kernel: string; events: number; emulated: number;
  signal: string | null; syscalls: Record<string, number>; vdso_patched: string[];
}
export interface MemBlob { addr: number; bytes: number[] }
export type NativeKind =
  | { Syscall: { nr: number; name: string; args: number[]; ret: number; policy: "Passthrough" | "Emulated"; outputs: MemBlob[]; detail: string } }
  | { Signal: { signo: number; name: string; fault_addr: number | null } }
  | { Exit: { code: number } };
export interface NativeEvent { index: number; kind: NativeKind; regs: { pc: number; sp: number; regs: [string, number][] }; symbol: string | null }
export interface NativeRecording {
  program: string; args: string[]; arch: string; kernel: string; load_base: number; globals: { name: string; addr: number; size: number }[];
  vdso_patched: string[]; stdout: number[]; events: NativeEvent[]; global_history: MemBlob[][];
}

export class ApiError extends Error {
  constructor(message: string, public line?: number, public col?: number) { super(message); }
}

async function req<T>(method: string, url: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: body !== undefined ? { "content-type": "application/json" } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
    signal,
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new ApiError(data.message ?? data.error ?? res.statusText, data.line, data.col);
  return data as T;
}

export const api = {
  health: () => req<{ engine: string; store: string; native: boolean; os: string; ai: boolean }>("GET", "/api/health"),
  examples: () => req<Example[]>("GET", "/api/examples"),
  record: (name: string, source: string, seed: number, chaos: number) =>
    req<Summary>("POST", "/api/sessions", { name, source, seed, chaos }),
  timeline: (id: string) => req<TimelineData>("GET", `/api/sessions/${id}/timeline`),
  state: (id: string, step: Step, signal?: AbortSignal) => req<StateView>("GET", `/api/sessions/${id}/state?step=${step}`, undefined, signal),
  analysis: (id: string) => req<Report>("GET", `/api/sessions/${id}/analysis`),
  history: (id: string, loc: string) => req<{ loc: string; name: string; points: HistoryPoint[] }>("GET", `/api/sessions/${id}/history?loc=${encodeURIComponent(loc)}`),
  diff: (a: string, b: string) => req<Divergence>("GET", `/api/sessions/${a}/diff/${b}`),
  explain: (id: string, llm: boolean) => req<Explanation>("POST", `/api/sessions/${id}/explain`, { llm }),
  hunt: (name: string, source: string, runs: number, chaos: number) => req<Hunt>("POST", "/api/hunt", { name, source, runs, chaos }),
  openRun: (hunt: string, seed: number) => req<Summary>("POST", `/api/hunt/${hunt}/open/${seed}`),
  native: () => req<NativeSummary[]>("GET", "/api/native"),
  nativeGet: (id: string) => req<NativeRecording>("GET", `/api/native/${id}`),
  exportUrl: (id: string) => `/api/sessions/${id}/export`,
};
