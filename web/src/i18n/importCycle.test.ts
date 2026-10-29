/*
 * O i18n não pode alcançar o cliente da API pelos imports (spec 134 T06): o par formava um ciclo e
 * o carregamento dependia da ordem dos imports (no ESM do Vite em desenvolvimento, o cliente
 * carregado antes do i18n quebrava com "requestLanguage before initialization"). Confere o grafo
 * estático dos imports relativos a partir de `i18n/index.ts`.
 */
import { existsSync, readFileSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = resolve(process.cwd(), "src");

function resolveImport(from: string, spec: string): string | null {
  const base = resolve(dirname(from), spec);
  for (const candidate of [base, `${base}.ts`, `${base}.tsx`, resolve(base, "index.ts")]) {
    if (existsSync(candidate) && candidate.match(/\.tsx?$/)) return candidate;
  }
  return null;
}

/** Imports relativos de valor (os `import type` somem na compilação e não formam ciclo). */
function valueImports(file: string): string[] {
  const code = readFileSync(file, "utf8");
  const out: string[] = [];
  for (const m of code.matchAll(/^(?:import|export)\s+(?!type\b)[^;]*?from\s+"(\.[^"]+)";/gms)) {
    const target = resolveImport(file, m[1] ?? "");
    if (target !== null) out.push(target);
  }
  for (const m of code.matchAll(/^import\s+"(\.[^"]+)";/gm)) {
    const target = resolveImport(file, m[1] ?? "");
    if (target !== null) out.push(target);
  }
  return out;
}

function reachable(start: string): Set<string> {
  const seen = new Set<string>();
  const stack = [start];
  while (stack.length > 0) {
    const file = stack.pop() ?? "";
    if (seen.has(file)) continue;
    seen.add(file);
    stack.push(...valueImports(file));
  }
  return seen;
}

describe("i18n sem ciclo com o cliente da API", () => {
  it("nenhum import de valor a partir do i18n chega a api/client.ts", () => {
    const files = [...reachable(resolve(SRC, "i18n/index.ts"))].map((f) => relative(SRC, f));
    expect(files).toContain("api/requestLanguage.ts");
    expect(files).not.toContain("api/client.ts");
    expect(files).not.toContain("desktop/uiState.ts");
  });

  it("o idioma da requisição e o armazenamento local não importam nada", () => {
    for (const file of ["api/requestLanguage.ts", "desktop/browserStorage.ts"]) {
      expect(valueImports(resolve(SRC, file))).toEqual([]);
    }
  });
});
