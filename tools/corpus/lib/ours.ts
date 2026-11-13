import { HEIGHT, WIDTH, type Word } from "./webkit.ts";

export type Outcome = "ok" | "crash" | "panic" | "timeout";
export type Run = { outcome: Outcome; code: number | null; signal: string | null; ms: number; stdout: string; stderr: string };
export type OursBoxes = { rects: ([number, number, number, number] | "nobox" | "nomatch")[]; height: number; words: Word[]; cssDrops: Record<string, number>; jsErrors: string[]; stderrTail: string };

const TIMEOUT_MS = 90_000;

export async function runRenderer(bin: string, args: string[], env: Record<string, string>, size: readonly [number, number] = [WIDTH, HEIGHT]): Promise<Run> {
  const started = Date.now();
  const proc = Bun.spawn([bin, ...args, "--width", String(size[0]), "--height", String(size[1]), "--no-cache"], {
    env: { ...process.env, RENDER_LOOPBACK_ONLY: "1", ...env },
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
    timeout: TIMEOUT_MS,
    killSignal: "SIGKILL",
  });
  const [stdout, stderr] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()]);
  const code = await proc.exited;
  const ms = Date.now() - started;
  const signal = proc.signalCode ?? null;
  const outcome: Outcome = ms >= TIMEOUT_MS - 50 && signal ? "timeout" : /panicked at/.test(stderr) ? "panic" : code === 0 ? "ok" : "crash";
  return { outcome, code, signal, ms, stdout, stderr };
}

// Our text items are fragments of a line; words get x positions in proportion to characters.
function splitWords(x: number, y: number, w: number, h: number, text: string): Word[] {
  const out: Word[] = [];
  const len = Math.max(text.length, 1);
  for (const m of text.matchAll(/\S+/g)) {
    const i = m.index ?? 0;
    out.push([m[0], x + (w * i) / len, y, (w * m[0].length) / len, h]);
  }
  return out;
}

const JS_ERROR = /^\[js\] (?!cannot read script|warning:)(.*(error|uncaught|exception|not a function|undefined|null|is not defined).*)$/i;

export function parseBoxes(run: Run, count: number): OursBoxes {
  const rects: OursBoxes["rects"] = Array.from({ length: count }, () => "nomatch" as const);
  const words: Word[] = [];
  let height = 0;
  for (const line of run.stdout.split("\n")) {
    if (line.startsWith("text ")) {
      const bar = line.indexOf(" | ");
      const [x, y, w, h] = line.slice(5, bar).split(" ").map(Number);
      words.push(...splitWords(x, y, w, h, line.slice(bar + 3)));
    } else if (line.startsWith("height ")) height = Number(line.slice(7));
    else {
      const m = /^(\d+) (\?|-|([-\d.]+) ([-\d.]+) ([-\d.]+) ([-\d.]+))/.exec(line);
      if (!m) continue;
      const i = Number(m[1]);
      rects[i] = m[2] === "?" ? "nomatch" : m[2] === "-" ? "nobox" : [Number(m[3]), Number(m[4]), Number(m[5]), Number(m[6])];
    }
  }
  const cssDrops: Record<string, number> = {};
  const jsErrors: string[] = [];
  for (const line of run.stderr.split("\n")) {
    const d = /^\[css-drop\] (\S+) (.+) (\d+)$/.exec(line);
    if (d) cssDrops[`${d[1]} ${d[2]}`] = (cssDrops[`${d[1]} ${d[2]}`] ?? 0) + Number(d[3]);
    else if (JS_ERROR.test(line)) jsErrors.push(line.slice(5, 300));
  }
  return { rects, height, words, cssDrops, jsErrors, stderrTail: run.stderr.split("\n").filter((l) => !l.startsWith("[css-drop]")).slice(-15).join("\n") };
}
