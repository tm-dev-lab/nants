import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ReportMarkdown } from "./apps/shared/ReportMarkdown";
import { findHowToSection, parseReportMarkdown } from "./reportMarkdown";

/** PRNG determinístico (mulberry32): a mesma semente gera sempre as mesmas entradas. */
function prng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Alfabeto adversarial: marcadores de inline, cabeçalho, lista, cerca, espaços e quebras. */
const ALPHABET = [
  "[", "]", "(", ")", "*", "_", "`", "#", "-", "1.", " ", "\t", "\n", " ", "!", "<", ":", ".", "a", "```",
  "\u2028", "\r\n",
];
const MAX_CHARS = 64 * 1024;
const INPUT_COUNT = 300;

function adversarialInput(next: () => number): string {
  const target = 1 + Math.floor(next() * MAX_CHARS);
  const parts: string[] = [];
  let length = 0;
  while (length < target) {
    const token = ALPHABET[Math.floor(next() * ALPHABET.length)] ?? "a";
    parts.push(token);
    length += token.length;
  }
  return parts.join("").slice(0, MAX_CHARS);
}

function parseAndRender(input: string): void {
  const blocks = parseReportMarkdown(input);
  renderToStaticMarkup(<ReportMarkdown blocks={blocks} />);
  findHowToSection(blocks);
}

// Folga generosa: linear leva poucos ms; regressão quadrática em 64 KiB leva dezenas de segundos,
// então 5 s ainda a detecta e tolera máquina carregada (suíte cheia em paralelo).
const WORST_MS = 5000;

describe("propriedade de tempo: entradas adversariais de até 64 KiB", () => {
  it(`${INPUT_COUNT} entradas: média < 1 s e nenhuma > 5 s (parse + render + seção)`, () => {
    const next = prng(20261014);
    const durations: number[] = [];
    for (let index = 0; index < INPUT_COUNT; index += 1) {
      const input = adversarialInput(next);
      const start = performance.now();
      parseAndRender(input);
      durations.push(performance.now() - start);
    }
    const average = durations.reduce((sum, value) => sum + value, 0) / durations.length;
    const worst = Math.max(...durations);
    expect(average).toBeLessThan(1000);
    expect(worst).toBeLessThan(WORST_MS);
  });

  it("entradas nomeadas (linear por construção)", () => {
    const named = [
      "[".repeat(65536),
      "![".repeat(32768),
      `# a${" ".repeat(65536)}b`,
      "*".repeat(65536),
      "_".repeat(65536),
      "`".repeat(65536),
      "[a](".repeat(16384),
      `- ${" ".repeat(65536)} `,
      `# ${" ".repeat(65536)} `,
      `- ${" ".repeat(65536)}\u2028 b`,
      `${"## Como executar ".repeat(4000)}`,
    ];
    for (const input of named) {
      const start = performance.now();
      parseAndRender(input);
      expect(performance.now() - start).toBeLessThan(WORST_MS);
    }
  });
});
