# GitHub repository settings

**Description** (About box, ≤350 chars):

> ⏪ Deterministic record/replay & time-travel debugger. Records only nondeterminism (schedules, clocks, rand, input) into ~1 KB, replays bit-for-bit, steps backwards, finds races/deadlocks with vector clocks and ranks root causes with evidence. Rust core, Linux ptrace backend, gRPC, React UI.

**Website:** leave empty, or link the demo: `https://github.com/coolstack-coder/ChronoForge/blob/main/docs/media/chronoforge-demo.mp4`

**Topics:**
`time-travel-debugging` `record-replay` `deterministic-replay` `reverse-debugging` `debugger` `rust` `ptrace` `linux` `concurrency` `race-detection` `deadlock-detection` `vector-clocks` `dynamic-slicing` `root-cause-analysis` `grpc` `rocksdb` `react` `typescript` `systems-programming` `llm`

**Resume line:**

> **ChronoForge | Deterministic Execution Replay & Time-Travel Debugging Platform** — Rust engine that records only nondeterminism (~1,700× smaller than a full trace) and replays bit-for-bit; checkpointed reverse execution with µs seeks; FastTrack-style race, Goodlock and wait-for deadlock detection; dynamic-slicing root-cause ranking; Linux ptrace backend (x86_64/arm64) with vDSO patching and syscall emulation; axum + tonic gRPC API; React/TypeScript timeline UI; Claude-based narration with citation grounding.

## Making the video play inline in the README

GitHub only shows an inline video player for files uploaded through its own editor; a video
committed to the repo shows as a link. The README therefore shows the GIF (autoplays) and links
the MP4. To get a real player with sound-off subtitles:

1. Open `README.md` on github.com → ✏️ edit.
2. Drag `docs/media/chronoforge-demo.mp4` into the editor where the GIF is.
3. GitHub inserts a `https://github.com/user-attachments/assets/…` link — that line renders as a player. Commit.
