import type { ReactNode } from "react";

const KW = new Set(["let", "fn", "if", "else", "while", "for", "in", "return", "spawn", "break", "continue", "true", "false", "nil"]);
const BUILTINS = new Set(["print", "assert", "len", "push", "str", "rand", "now", "input", "mutex", "lock", "unlock", "join", "yield", "panic"]);
const TOKEN = /(\/\/.*$)|("(?:[^"\\]|\\.)*")|(\b\d[\d_]*\b)|([A-Za-z_]\w*)(?=\s*\()|([A-Za-z_]\w*)|(\s+)|(.)/g;

// Good enough for Forge: one line at a time, no nesting to worry about.
export function highlight(line: string): ReactNode[] {
  const out: ReactNode[] = [];
  let m: RegExpExecArray | null;
  let k = 0;
  TOKEN.lastIndex = 0;
  while ((m = TOKEN.exec(line))) {
    const [text, com, str, num, call, ident] = m;
    if (com) out.push(<span key={k++} className="tok-com">{text}</span>);
    else if (str) out.push(<span key={k++} className="tok-str">{text}</span>);
    else if (num) out.push(<span key={k++} className="tok-num">{text}</span>);
    else if (call) out.push(<span key={k++} className={BUILTINS.has(call) ? "tok-bi" : "tok-fn"}>{text}</span>);
    else if (ident && KW.has(ident)) out.push(<span key={k++} className="tok-kw">{text}</span>);
    else out.push(text);
  }
  return out;
}

export const threadColor = (tid: number) => `var(--t${tid % 6})`;
export const THREAD_HEX = ["#ece9e2", "#ff6b1a", "#4fd1c5", "#b794f6", "#f6d860", "#f687b3"];
export const threadHex = (tid: number) => THREAD_HEX[tid % THREAD_HEX.length];
