/* Modelo da janela Conhecimento (spec 134 T06): citações `kb:`, ids reais, indicadores e plural. */
import { afterEach, describe, expect, it } from "vitest";
import type { KbDoc } from "../../api/kb";
import { setLanguage } from "../../i18n";
import { indicatorText, isDocId, kbSubtitle, splitAtSection, splitCitations } from "./knowledgeModel";

afterEach(() => setLanguage("pt-BR", { persist: false }));

function doc(id: string, status: KbDoc["status"] = "ready"): KbDoc {
  return {
    id,
    title: id,
    kind: "markdown",
    source: "file",
    file_name: null,
    source_url: null,
    size: 0,
    added_at: "2026-10-28T12:00:00Z",
    fetched_at: null,
    status,
    pages: null,
    role: "reference",
    description: "",
    applies_to: [],
    links_out: [],
  };
}

describe("ids e citações", () => {
  it("isDocId segue kb::is_doc_id: slug, hífen e 8, 16 ou 32 hex minúsculos", () => {
    for (const ok of ["padroes-3f9a1c2e", "a-b-0123456789abcdef", `x-${"a".repeat(32)}`]) expect(isDocId(ok)).toBe(true);
    for (const bad of ["session", "user", "padroes-3F9A1C2E", "-3f9a1c2e", "padroes-3f9a1c2", "padroes_3f9a1c2e", `${"a".repeat(56)}-3f9a1c2e`]) {
      expect(isDocId(bad)).toBe(false);
    }
  });

  it("splitCitations: só id real, com a seção; pontuação final fica no texto; kb: dentro de palavra não conta", () => {
    const parts = splitCitations("Veja kb:api-3f9a1c2e#versionamento. E kb:guia-0d6b72ae, kb:session, abckb:api-3f9a1c2e e kb:user:1.");
    const cites = parts.flatMap((p) => (p.kind === "cite" ? [p.cite] : []));
    expect(cites).toEqual([
      { text: "kb:api-3f9a1c2e#versionamento", doc: "api-3f9a1c2e", section: "versionamento" },
      { text: "kb:guia-0d6b72ae", doc: "guia-0d6b72ae", section: null },
    ]);
    expect(parts.map((p) => (p.kind === "text" ? p.text : p.cite.text)).join("")).toBe(
      "Veja kb:api-3f9a1c2e#versionamento. E kb:guia-0d6b72ae, kb:session, abckb:api-3f9a1c2e e kb:user:1.",
    );
    expect(splitCitations("sem citação")).toEqual([{ kind: "text", text: "sem citação" }]);
  });

  it("âncora neutra s.<n> é aceita", () => {
    const [, cite] = splitCitations("ver kb:api-3f9a1c2e#s.3");
    expect(cite).toEqual({ kind: "cite", cite: { text: "kb:api-3f9a1c2e#s.3", doc: "api-3f9a1c2e", section: "s.3" } });
  });
});

describe("indicadores e subtítulo", () => {
  it("o código vira texto no idioma ativo; código desconhecido some", () => {
    const docs = [doc("api-3f9a1c2e")];
    expect(indicatorText({ code: "duplicate", params: { doc: "api-3f9a1c2e" } }, docs)).toBe('Mesmo conteúdo de "api-3f9a1c2e"');
    expect(indicatorText({ code: "no_headings", params: {} }, docs)).toBe("Sem títulos: só dá para citar o documento inteiro");
    expect(indicatorText({ code: "outro_codigo" }, docs)).toBeNull();
    setLanguage("en", { persist: false });
    expect(indicatorText({ code: "broken_link", params: { target: "a.md" } }, docs)).toBe('The internal link to "a.md" does not point to any document in the base');
  });

  it("plural com .one e .other nos dois idiomas", () => {
    const one = { docs: [doc("a-3f9a1c2e", "extracting")], has_overview: false };
    expect(kbSubtitle(one)).toBe("1 documento · 1 indexando");
    const two = { docs: [doc("a-3f9a1c2e", "error"), doc("b-3f9a1c2e", "no_text")], has_overview: false };
    expect(kbSubtitle(two)).toBe("2 documentos · 2 com problema");
    setLanguage("en", { persist: false });
    expect(kbSubtitle(one)).toBe("1 document · 1 indexing");
    expect(kbSubtitle(two)).toBe("2 documents · 2 with a problem");
  });
});

describe("seção citada na prévia (F-03)", () => {
  // Linhas: 0 "# Guia", 2 "## Exemplo", 6 cerca com "## Exemplo" dentro (não é seção), 10 "## Exemplo".
  const content = ["# Guia", "", "## Exemplo", "", "Primeiro.", "", "```md", "## Exemplo", "```", "", "## Exemplo", "", "Segundo.", "", "## Fim", "", "Fim."].join("\n");
  const sections = [
    { anchor: "guia", title: "Guia", level: 1, line: 0 },
    { anchor: "exemplo", title: "Exemplo", level: 2, line: 2 },
    { anchor: "exemplo-2", title: "Exemplo", level: 2, line: 10 },
    { anchor: "fim", title: "Fim", level: 2, line: 14 },
  ];

  it("#exemplo-2 destaca o segundo Exemplo, pela linha do servidor, e o título na cerca não confunde", () => {
    const parts = splitAtSection(content, sections, "exemplo-2");
    expect(parts?.[1]).toBe("## Exemplo\n\nSegundo.\n");
    expect(parts?.[0]).toContain("Primeiro.");
    expect(parts?.[0]).toContain("```md\n## Exemplo\n```");
    expect(parts?.[2]).toBe("## Fim\n\nFim.");
  });

  it("#exemplo vai até a próxima seção do mesmo nível, com a cerca dentro", () => {
    const parts = splitAtSection(content, sections, "exemplo");
    expect(parts?.[1]).toBe("## Exemplo\n\nPrimeiro.\n\n```md\n## Exemplo\n```\n");
  });

  it("sem a linha, âncora desconhecida ou a seção de nível 1 do topo: sem destaque", () => {
    expect(splitAtSection(content, [{ anchor: "exemplo", title: "Exemplo", level: 2 }], "exemplo")).toBeNull();
    expect(splitAtSection(content, sections, "nada")).toBeNull();
    expect(splitAtSection(content, sections, null)).toBeNull();
  });
});
