// raw.webm + demo.srt → chronoforge-demo.mp4 (burned-in captions + a soft
// subtitle track), a README gif, and a poster frame. Trims the page-load
// lead-in and shifts the captions to match.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import ffmpeg from "ffmpeg-static";

const dir = path.resolve(process.argv[2] ?? "../docs/media");
const f = (n) => path.join(dir, n);
const run = (...a) => execFileSync(ffmpeg, ["-hide_banner", "-loglevel", "error", "-y", ...a], { stdio: "inherit" });

const srt = fs.readFileSync(f("demo.srt"), "utf8");
const toS = (t) => { const [h, m, s] = t.replace(",", ".").split(":"); return +h * 3600 + +m * 60 + +s; };
const fromS = (x, sep) => {
  const ms = Math.max(0, Math.round(x * 1000)), p = (n, w = 2) => String(n).padStart(w, "0");
  return `${p(Math.floor(ms / 3600000))}:${p(Math.floor(ms / 60000) % 60)}:${p(Math.floor(ms / 1000) % 60)}${sep}${p(ms % 1000, 3)}`;
};
const first = toS(srt.match(/(\d\d:\d\d:\d\d,\d\d\d) -->/)[1]);
const cut = Math.max(0, first - 0.6);
const shift = (text, sep) => text.replace(/(\d\d:\d\d:\d\d[,.]\d\d\d)/g, (t) => fromS(toS(t) - cut, sep));
fs.writeFileSync(f("chronoforge-demo.srt"), shift(srt, ","));
fs.writeFileSync(f("chronoforge-demo.vtt"), shift(fs.readFileSync(f("demo.vtt"), "utf8").replace(/,/g, "."), "."));

run("-ss", String(cut), "-i", f("raw.webm"), "-i", f("chronoforge-demo.srt"),
  "-map", "0:v", "-map", "1", "-c:v", "libx264", "-preset", "slow", "-crf", "24", "-pix_fmt", "yuv420p",
  "-c:s", "mov_text", "-metadata:s:s:0", "language=eng", "-movflags", "+faststart", f("chronoforge-demo.mp4"));

// gif: the whole story at 2.4x, under ~7 MB so it autoplays in a README
run("-i", f("chronoforge-demo.mp4"), "-vf",
  "setpts=PTS/2.4,fps=8,scale=840:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=64:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle",
  f("chronoforge-demo.gif"));

run("-ss", String(cut + 30), "-i", f("raw.webm"), "-frames:v", "1", "-vf", "scale=1440:-1", f("poster.png"));
for (const n of ["raw.webm", "demo.srt", "demo.vtt"]) fs.rmSync(f(n), { force: true });
for (const n of fs.readdirSync(dir)) console.log(n.padEnd(26), (fs.statSync(f(n)).size / 1e6).toFixed(2), "MB");
