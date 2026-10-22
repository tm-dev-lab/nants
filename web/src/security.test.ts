import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

// `npm test` roda a partir de web/ (o ambiente jsdom não expõe import.meta.url como arquivo).
const SRC_DIR = resolve(process.cwd(), "src");
const SELF = resolve(SRC_DIR, "security.test.ts");

/** Lista recursivamente arquivos de código em src/ (exceto este próprio teste). */
function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      out.push(...sourceFiles(full));
    } else if (/\.(ts|tsx|css|html)$/.test(name) && full !== SELF) {
      out.push(full);
    }
  }
  return out;
}

/** Padrões proibidos em src/: injeção de HTML, eval e armazenamento local. */
const FORBIDDEN: Array<{ label: string; pattern: RegExp }> = [
  { label: "dangerouslySetInnerHTML", pattern: /dangerouslySetInnerHTML/ },
  { label: "innerHTML", pattern: /\binnerHTML\b/ },
  { label: "outerHTML", pattern: /\bouterHTML\b/ },
  { label: "eval", pattern: /\beval\s*\(/ },
  { label: "new Function", pattern: /\bnew\s+Function\s*\(/ },
  { label: "localStorage", pattern: /\blocalStorage\b/ },
  { label: "sessionStorage", pattern: /\bsessionStorage\b/ },
];

/** Exceções declaradas (dado não sensível, sempre em try/catch). */
const STORAGE_ALLOWED: ReadonlySet<string> = new Set([
  // Cache do ui-state (o servidor é a fonte; envelope validado).
  "desktop/uiState.ts",
  // Limpeza na carga das chaves fora do formato do ui-state (lista explícita).
  "staleStorage.ts",
  "staleStorage.test.ts",
]);

/** Só a limpeza na carga toca o `sessionStorage` (para apagar, nunca para gravar). */
const SESSION_ALLOWED: ReadonlySet<string> = new Set(["staleStorage.ts", "staleStorage.test.ts"]);

describe("segurança do código da SPA", () => {
  const files = sourceFiles(SRC_DIR);

  it("varre ao menos os módulos principais", () => {
    const names = files.map((f) => relative(SRC_DIR, f));
    expect(names).toEqual(
      expect.arrayContaining(["api/client.ts", "api/events.ts", "store.ts", "main.tsx"]),
    );
  });

  it("nenhum arquivo em src/ usa dangerouslySetInnerHTML, innerHTML, eval ou storage", () => {
    const hits: string[] = [];
    for (const file of files) {
      const text = readFileSync(file, "utf8");
      const rel = relative(SRC_DIR, file);
      const allowed = new Set<string>();
      if (STORAGE_ALLOWED.has(rel)) allowed.add("localStorage");
      if (SESSION_ALLOWED.has(rel)) allowed.add("sessionStorage");
      for (const { label, pattern } of FORBIDDEN) {
        if (allowed.has(label)) continue;
        if (pattern.test(text)) hits.push(`${relative(SRC_DIR, file)}: ${label}`);
      }
    }
    expect(hits).toEqual([]);
  });

  it("nenhum arquivo em src/ referencia CDN em runtime", () => {
    const hits: string[] = [];
    for (const file of files) {
      const text = readFileSync(file, "utf8");
      if (/https?:\/\/(cdn\.|fonts\.googleapis|fonts\.gstatic)/.test(text)) {
        hits.push(relative(SRC_DIR, file));
      }
    }
    expect(hits).toEqual([]);
  });
});
