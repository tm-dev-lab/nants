// @vitest-environment node
/*
 * Entrada escondida presa ao rótulo (141/T11, item 9 da spec). O `.nd-sr-only` é `position: absolute`.
 * Dentro de um rótulo sem posição, a entrada se posicionava num ancestral distante, fora da rolagem
 * da conversa; o foco do clique (trocar a opção de uma pergunta do Arquiteto) fazia o Chrome rolar o
 * `.nd-content` da janela, que tem `overflow: hidden`, e a tela ficava em branco no computador.
 *
 * Contrato:
 * 1. toda entrada focável com `.nd-sr-only` fica dentro de um `<label>`;
 * 2. a classe desse rótulo tem, no CSS, uma regra de nível de arquivo com `position` não estática.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRules } from "../test-utils";

const SRC = resolve(process.cwd(), "src");

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (/\.(tsx|css)$/.test(name) && !/\.test\.tsx?$/.test(name)) out.push(full);
  }
  return out;
}

const files = walk(SRC).map((full) => ({ file: relative(SRC, full).split(sep).join("/"), text: readFileSync(full, "utf8") }));
const tsx = files.filter((f) => f.file.endsWith(".tsx"));
const rules = files.filter((f) => f.file.endsWith(".css")).flatMap((f) => cssRules(f.text));

/**
 * Aberturas `<nome …>` no fonte, com a posição. Pula expressões `{…}` e textos entre aspas, que podem
 * conter `>` (ex.: `onChange={() => …}`).
 */
function tags(text: string, name: string): { at: number; tag: string }[] {
  const out: { at: number; tag: string }[] = [];
  const open = new RegExp(`<${name}\\b`, "g");
  for (const m of text.matchAll(open)) {
    const at = m.index ?? 0;
    let depth = 0;
    let quote: string | null = null;
    let i = at + m[0].length;
    for (; i < text.length; i += 1) {
      const c = text[i];
      if (quote !== null) {
        if (c === quote) quote = null;
      } else if (depth === 0 && (c === '"' || c === "'")) quote = c;
      else if (c === "{") depth += 1;
      else if (c === "}") depth -= 1;
      else if (c === ">" && depth === 0) break;
    }
    out.push({ at, tag: text.slice(at, i + 1) });
  }
  return out;
}

const HIDDEN_CLASS = /\sclassName="[^"]*\bnd-sr-only\b[^"]*"/;
const FIRST_CLASS = /\sclassName="([^"\s]+)/;

interface Anchor {
  file: string;
  labelClass: string;
}

function anchors(): { found: Anchor[]; orphans: string[] } {
  const found: Anchor[] = [];
  const orphans: string[] = [];
  for (const { file, text } of tsx) {
    const hidden = ["input", "select", "textarea"].flatMap((name) => tags(text, name)).filter((t) => HIDDEN_CLASS.test(t.tag));
    if (hidden.length === 0) continue;
    const labels = tags(text, "label").map((l) => {
      const close = text.indexOf("</label>", l.at);
      return { start: l.at, end: close === -1 ? l.at : close, cls: FIRST_CLASS.exec(l.tag)?.[1] ?? "" };
    });
    for (const { at } of hidden) {
      const label = labels.find((l) => l.start < at && at < l.end);
      if (label === undefined) orphans.push(`${file}@${at}`);
      else found.push({ file, labelClass: label.cls });
    }
  }
  return { found, orphans };
}

function positioned(cls: string): boolean {
  return rules.some((r) => r.at === null && r.selectors.includes(`.${cls}`) && /(?:^|;)\s*position:\s*(relative|absolute|fixed|sticky)\s*;/.test(r.body));
}

describe("entrada escondida (.nd-sr-only) presa ao rótulo", () => {
  const { found, orphans } = anchors();

  it("a varredura acha os rótulos conhecidos (opção da pergunta e item do segmento)", () => {
    const classes = new Set(found.map((a) => a.labelClass));
    expect(classes.has("nd-option")).toBe(true);
    expect(classes.has("nd-segment-item")).toBe(true);
  });

  it("toda entrada focável com .nd-sr-only fica dentro de um <label>", () => {
    expect(orphans).toEqual([]);
  });

  it("o rótulo de cada entrada escondida tem position não estática no CSS", () => {
    const loose = [...new Set(found.filter((a) => !positioned(a.labelClass)).map((a) => `${a.file}: .${a.labelClass}`))];
    expect(loose).toEqual([]);
  });

  it("a regra pega um rótulo sem posição", () => {
    expect(positioned("nd-classe-que-nao-existe")).toBe(false);
  });
});
