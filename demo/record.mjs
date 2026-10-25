// Records the README demo: drives the real UI against a running
// `chronoforge serve`, burns subtitles into a strip under the app, and
// writes matching .srt/.vtt files from the actual timings.
//
//   chronoforge serve --store memory &   (from the repo root)
//   node demo/record.mjs                 → docs/media/raw.webm + captions
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";

const OUT = path.resolve(process.argv[2] ?? "../docs/media");
const URL = process.env.CF_URL ?? "http://127.0.0.1:7420/";
const W = 1440, H = 900, STRIP = 70;
fs.mkdirSync(OUT, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: W, height: H + STRIP },
  recordVideo: { dir: OUT, size: { width: W, height: H + STRIP } },
});
const t0 = Date.now();
const page = await ctx.newPage();
page.on("pageerror", (e) => console.error("pageerror:", e.message));

// shrink the app and put a caption strip underneath it
const STRIP_CSS = `
  html, body { height: ${H}px !important; }
  #cf-sub { position: fixed; left: 0; right: 0; top: ${H}px; height: ${STRIP}px; z-index: 99999;
    display: flex; align-items: center; justify-content: center; gap: 14px; padding: 0 40px;
    background: #050506; border-top: 1px solid #24242b;
    font: 500 19px/1.3 "Inter Variable", Inter, system-ui, sans-serif; color: #ece9e2; text-align: center; }
  #cf-sub b { color: #ff6b1a; font-weight: 600; }
  #cf-sub i { font-style: normal; font: 11px "JetBrains Mono", monospace; color: #5c5a55; letter-spacing: 1.5px; }
  #cf-cursor { position: fixed; z-index: 99998; width: 22px; height: 22px; margin: -11px 0 0 -11px; border-radius: 50%;
    border: 2px solid #ff6b1a; background: #ff6b1a33; pointer-events: none; transition: left .45s cubic-bezier(.3,.7,.2,1), top .45s cubic-bezier(.3,.7,.2,1); }
  #cf-cursor.tap { animation: tap .35s; }
  @keyframes tap { 50% { transform: scale(.6); background: #ff6b1a99; } }
`;
await page.addInitScript((css) => {
  window.addEventListener("DOMContentLoaded", () => {
    const s = document.createElement("style");
    s.textContent = css;
    document.head.appendChild(s);
    const sub = document.createElement("div");
    sub.id = "cf-sub";
    document.body.appendChild(sub);
    const c = document.createElement("div");
    c.id = "cf-cursor";
    c.style.left = "720px";
    c.style.top = "450px";
    document.body.appendChild(c);
  });
}, STRIP_CSS);

const cues = [];
let open = null;
const now = () => (Date.now() - t0) / 1000;
async function say(html, step = "") {
  if (open) open.end = now();
  open = { start: now(), end: null, text: html.replace(/<[^>]+>/g, "") };
  cues.push(open);
  await page.evaluate(
    ([h, s]) => (document.getElementById("cf-sub").innerHTML = (s ? `<i>${s}</i>` : "") + `<span>${h}</span>`),
    [html, step],
  );
}
const beat = (ms) => page.waitForTimeout(ms);

// a visible cursor so viewers can follow the clicks
async function point(locator) {
  const box = await locator.boundingBox();
  if (!box) return;
  const x = box.x + Math.min(box.width / 2, 60), y = box.y + box.height / 2;
  await page.evaluate(([x, y]) => {
    const c = document.getElementById("cf-cursor");
    c.style.left = x + "px";
    c.style.top = y + "px";
  }, [x, y]);
  await beat(520);
}
async function click(locator) {
  await point(locator);
  await page.evaluate(() => {
    const c = document.getElementById("cf-cursor");
    c.classList.remove("tap");
    void c.offsetWidth;
    c.classList.add("tap");
  });
  await locator.click();
}
async function huntAndOpen(name) {
  await click(page.locator(".tab", { hasText: name }));
  await beat(500);
  await click(page.getByTestId("hunt"));
  await beat(500);
  await click(page.getByText(/start hunt|hunt again/));
  await page.waitForSelector(".cell.fail", { timeout: 20000 });
  await beat(1400);
  await click(page.locator(".cell.fail").nth(1));
  await page.waitForSelector(".verdict h1");
}

// ── the story ────────────────────────────────────────────────────────────
await page.goto(URL);
await beat(400);
await say("3:07 AM. The ledger service broke <b>once</b> — and every re-run passes.", "THE PROBLEM");
await beat(3600);
await say("Thread interleavings, clocks, random seeds: the cause changes every time you run it.", "THE PROBLEM");
await beat(3000);
await point(page.getByText("Investigate the 3 AM incident"));
await page.keyboard.press("Enter");
await beat(700);
await say("ChronoForge records the program under a <b>chaos scheduler</b>, hundreds of times.", "01 · RECORD");
await beat(1200);
await click(page.getByText("start hunt"));
await page.waitForSelector(".cell.fail");
await beat(1600);
await say("Each run is captured as just its nondeterminism — <b>a few hundred bytes</b>. Red runs broke.", "01 · RECORD");
await beat(3200);
await say("Open a failing run. It isn't re-run — it's <b>replayed bit-for-bit</b> from its recording.", "02 · REPLAY");
await click(page.locator(".cell.fail").nth(2));
await page.waitForSelector(".verdict h1");
await beat(2200);
await say("The verdict: a <b>lost update on balance</b> — 94% confidence, backed by exact steps.", "03 · EXPLAIN");
await beat(3400);

const ev = page.locator(".ev");
await say("Evidence 1 → T2 reads balance = 100 …", "REWIND");
await click(ev.nth(0));
await beat(2300);
await say("… 2 → T1 sneaks in and writes 20 …", "REWIND");
await click(ev.nth(1));
await beat(2300);
await say("… 3 → T2 writes its <b>stale</b> 50 over it. 80 + 50 paid out of a 100 balance.", "REWIND");
await click(ev.nth(2));
await beat(3000);

await say("Step <b>backwards</b> through any instruction — state rebuilt from checkpoints + replay.", "REVERSE STEP");
for (let i = 0; i < 7; i++) {
  await page.keyboard.press("ArrowLeft");
  await beat(330);
}
await beat(900);
await click(page.locator(".insp-tab", { hasText: "state" }));
await say("Plot any variable over time: the trace of <b>balance</b> goes 100 → 20 → 50.", "VALUE HISTORY");
await click(page.locator(".var .name", { hasText: /^balance$/ }));
await beat(3000);
await say("“Who wrote this?” — <b>⟲</b> reverse-jumps straight to the instruction that produced the value.", "REVERSE WATCHPOINT");
await click(page.locator(".var", { hasText: /^balance/ }).locator(".who"));
await beat(2600);

await say("Set a breakpoint, then <b>reverse-continue</b> to the last time it was hit.", "REVERSE CONTINUE");
await click(page.locator('.src-line[data-line="12"] .ln'));
await beat(600);
await page.keyboard.press("End");
await beat(700);
await click(page.locator(".tbtn", { hasText: "◀◀" }));
await beat(1500);
await click(page.locator(".tbtn", { hasText: "◀◀" }));
await beat(1800);

await say("Happens-before analysis flags the <b>data race</b> — it would even in runs that passed.", "CONCURRENCY");
await click(page.locator(".insp-tab", { hasText: "concurrency" }));
await beat(3000);
await say("Diff against a passing run: where the schedules split, and the <b>first write that differs</b>.", "TRACE DIFF");
await click(page.locator(".chip", { hasText: "diff against" }));
await beat(4200);
await page.keyboard.press("Escape").catch(() => {});
await click(page.getByText("esc"));
await beat(500);

await say("Deadlocks: the <b>lock-order graph</b> shows the fork cycle that froze the table.", "DEADLOCK");
await huntAndOpen("philosophers");
await click(page.locator(".insp-tab", { hasText: "concurrency" }));
await beat(3800);

await say("Single-threaded too: the slice walks back from the crash, through the loop, to <b>rand()</b>.", "ROOT CAUSE");
await huntAndOpen("checkout");
await beat(1000);
await click(page.locator(".ev").nth(0));
await beat(3400);

await say("And real Linux binaries: <b>ptrace</b> records syscalls, patches the vDSO, replays the segfault.", "NATIVE");
await click(page.locator(".tab", { hasText: "native" }));
await beat(2200);
const sc = page.locator(".syscall");
if ((await sc.count()) > 3) {
  await click(sc.filter({ hasText: "clock_gettime" }).first());
  await beat(2000);
  await click(sc.filter({ hasText: "SIG" }).first());
  await beat(2600);
}
await say("ChronoForge — <b>record once. replay forever. rewind anywhere.</b>", "");
await click(page.locator(".brand"));
await beat(3500);
open.end = now();

await page.close();
const video = await page.video().path();
await ctx.close();
await browser.close();
fs.renameSync(video, path.join(OUT, "raw.webm"));

const ts = (s, sep) => {
  const ms = Math.round(s * 1000);
  const p = (n, w = 2) => String(n).padStart(w, "0");
  return `${p(Math.floor(ms / 3600000))}:${p(Math.floor(ms / 60000) % 60)}:${p(Math.floor(ms / 1000) % 60)}${sep}${p(ms % 1000, 3)}`;
};
fs.writeFileSync(path.join(OUT, "demo.srt"), cues.map((c, i) => `${i + 1}\n${ts(c.start, ",")} --> ${ts(c.end, ",")}\n${c.text}\n`).join("\n"));
fs.writeFileSync(path.join(OUT, "demo.vtt"), "WEBVTT\n\n" + cues.map((c) => `${ts(c.start, ".")} --> ${ts(c.end, ".")}\n${c.text}\n`).join("\n"));
console.log(`recorded ${now().toFixed(1)}s, ${cues.length} captions → ${OUT}`);
