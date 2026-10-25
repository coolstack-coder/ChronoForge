// Renders docs/media/banner.png with the UI's own fonts and palette.
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
const fonts = path.resolve("../ui/node_modules/@fontsource");
const html = `<!doctype html><html><head><style>
@font-face { font-family: IS; src: url(file://${fonts}/instrument-serif/files/instrument-serif-latin-400-normal.woff2); }
@font-face { font-family: IS; font-style: italic; src: url(file://${fonts}/instrument-serif/files/instrument-serif-latin-400-italic.woff2); }
@font-face { font-family: JB; src: url(file://${fonts}/jetbrains-mono/files/jetbrains-mono-latin-400-normal.woff2); }
html,body { margin:0; width:1600px; height:440px; background:#0b0b0c; overflow:hidden; }
body { position:relative; font-family: JB; color:#ece9e2;
  background-image: linear-gradient(#1a1a1f 1px, transparent 1px), linear-gradient(90deg, #1a1a1f 1px, transparent 1px);
  background-size: 40px 40px; }
.glow { position:absolute; inset:0; background: radial-gradient(circle at 78% 30%, #ff6b1a2e, transparent 45%), radial-gradient(circle at 10% 100%, #4fd1c51a, transparent 40%); }
.k { position:absolute; left:90px; top:70px; font-size:15px; letter-spacing:3px; color:#ff5252; }
.k i { display:inline-block; width:9px; height:9px; border-radius:50%; background:#ff5252; margin-right:10px; }
h1 { position:absolute; left:86px; top:96px; margin:0; font: 400 150px/1 IS; letter-spacing:-2px; }
h1 em { color:#ff6b1a; }
p { position:absolute; left:92px; top:268px; margin:0; font-size:21px; color:#8d8a82; }
p b { color:#ece9e2; font-weight:400; }
svg { position:absolute; left:0; bottom:0; }
</style></head><body><div class="glow"></div>
<div class="k"><i></i>REC · DETERMINISTIC REPLAY · TIME-TRAVEL DEBUGGING</div>
<h1>Chrono<em>Forge</em></h1>
<p><b>Record once.</b> Replay forever. <b>Rewind anywhere.</b></p>
<svg width="1600" height="110" viewBox="0 0 1600 110"></svg>
<script>
  // three thread lanes, a race arc and a playhead — the timeline, as a logo
  const svg = document.querySelector("svg"), C = ["#ece9e2", "#ff6b1a", "#4fd1c5"];
  let s = 7; const r = () => ((s = (s * 9301 + 49297) % 233280) / 233280);
  let out = "";
  for (let lane = 0; lane < 3; lane++) {
    let x = 90;
    while (x < 1510) {
      const w = 6 + r() * 26, on = r() > 0.42;
      if (on) out += '<rect x="'+x+'" y="'+(14 + lane * 30)+'" width="'+w+'" height="'+(r() > .8 ? 22 : 12)+'" fill="'+C[lane]+'" opacity="'+(.35 + r() * .6)+'" rx="1"/>';
      x += w + 3;
    }
  }
  out += '<path d="M610 44 C 640 -10, 860 -10, 890 74" stroke="#ffb547" stroke-width="2" stroke-dasharray="6 5" fill="none"/>';
  out += '<text x="738" y="16" fill="#ffb547" font-size="16">⚡</text>';
  out += '<rect x="1188" y="0" width="2.5" height="110" fill="#ff6b1a"/><path d="M1180 0 L1198 0 L1189 11 z" fill="#ff6b1a"/>';
  svg.innerHTML = out;
</script></body></html>`;
const b = await chromium.launch();
const p = await b.newPage({ viewport: { width: 1600, height: 440 }, deviceScaleFactor: 1 });
// file:// page so the file:// fonts are same-origin and actually load
const tmp = path.resolve(".banner.html");
fs.writeFileSync(tmp, html);
await p.goto("file://" + tmp);
await p.evaluate(() => document.fonts.ready);
await p.waitForTimeout(300);
await p.screenshot({ path: "../docs/media/banner.png" });
await b.close();
fs.rmSync(tmp);
