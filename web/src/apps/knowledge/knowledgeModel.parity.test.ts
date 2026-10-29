// @vitest-environment node
/*
 * Paridade da leitura das citações (spec 136 T04, C-14): os casos de `src/kb/cite_cases.json`, os
 * mesmos do `kb::cite::find` no servidor, passam pelo `splitCitations` da interface.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { splitCitations } from "./knowledgeModel";

interface Case {
  text: string;
  cites: { doc: string; section: string | null }[];
}

const cases = (JSON.parse(readFileSync(resolve(process.cwd(), "../src/kb/cite_cases.json"), "utf8")) as { cases: Case[] }).cases;

describe("paridade com kb::cite::find", () => {
  it("lê os casos compartilhados", () => {
    expect(cases.length).toBeGreaterThanOrEqual(10);
  });

  it.each(cases.map((c) => [c.text, c] as const))("%s", (_text, c) => {
    const parts = splitCitations(c.text);
    const found: Case["cites"] = [];
    for (const part of parts) {
      if (part.kind !== "cite") continue;
      const cite = { doc: part.cite.doc, section: part.cite.section };
      if (!found.some((f) => f.doc === cite.doc && f.section === cite.section)) found.push(cite);
    }
    expect(found).toEqual(c.cites);
    // O texto volta inteiro: nenhuma parte some nem se repete.
    expect(parts.map((p) => (p.kind === "text" ? p.text : p.cite.text)).join("")).toBe(c.text);
  });
});
