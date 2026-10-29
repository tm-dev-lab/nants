/*
 * Filtro do seletor da base (spec 136 T06, critério 2): sem acento e sem caixa, no cliente, com a
 * ordem (começo do título, começo de palavra, contém, só a origem), os trechos casados, a origem,
 * só os documentos prontos e o sumário filtrado em ordem.
 */
import { describe, expect, it } from "vitest";
import type { KbDoc, KbSection } from "../../api/kb";
import { docByTitle, docOrigin, filterKbDocs, filterKbSections, foldText, hasSections } from "./kbPickerModel";

function doc(id: string, title: string, extra: Partial<KbDoc> = {}): KbDoc {
  return {
    id,
    title,
    kind: "markdown",
    source: "file",
    file_name: `${id}.md`,
    source_url: null,
    size: 1,
    added_at: "2026-10-28T12:00:00Z",
    fetched_at: null,
    status: "ready",
    pages: null,
    description: "",
    links_out: [],
    ...extra,
  };
}

const DOCS = [
  doc("a", "Guia de estilo da API"),
  doc("b", "Padrões REST da equipe", { file_name: "padroes-rest.md" }),
  doc("c", "Spring Boot: amostra", { kind: "pdf", file_name: "livro.pdf" }),
  doc("d", "Guide to Spring HATEOAS", { kind: "link", source: "url", file_name: null, source_url: "https://www.baeldung.com/spring-hateoas" }),
  doc("e", "Os padrões de projeto"),
  doc("f", "Manual do H2", { status: "extracting" }),
  doc("g", "Glossário", { file_name: "termos-da-agenda.txt", indicators: [{ code: "no_headings" }] }),
];

describe("filtro dos documentos", () => {
  it("dobra acento e caixa", () => {
    expect(foldText("Padrões ÇÃO")).toBe("padroes cao");
    expect(filterKbDocs(DOCS, "PADROES").map((m) => m.doc.id)).toEqual(["b", "e"]);
  });

  it("ordena: começo do título, começo de palavra, contém, e só a origem por último", () => {
    const docs = [doc("x", "Os padrões"), doc("y", "Antipadrões"), doc("z", "Padrões"), doc("w", "Outro", { file_name: "padroes.md" })];
    expect(filterKbDocs(docs, "padr").map((m) => m.doc.id)).toEqual(["z", "x", "y", "w"]);
  });

  it("todas as palavras casam, em qualquer ordem, com os trechos no texto original", () => {
    const [match] = filterKbDocs(DOCS, "api guia");
    expect(match?.doc.id).toBe("a");
    expect(match?.ranges).toEqual([
      [0, 4],
      [18, 21],
    ]);
    const [accent] = filterKbDocs(DOCS, "padroes");
    expect(accent?.doc.title.slice(...(accent?.ranges[0] ?? [0, 0]))).toBe("Padrões");
  });

  it("sem consulta, os prontos na ordem da lista; o documento não pronto nunca aparece", () => {
    expect(filterKbDocs(DOCS, "").map((m) => m.doc.id)).toEqual(["a", "b", "c", "d", "e", "g"]);
    expect(filterKbDocs(DOCS, "manual")).toEqual([]);
  });

  it("sem resultado para `RestController` e `types/node`", () => {
    expect(filterKbDocs(DOCS, "RestController")).toEqual([]);
    expect(filterKbDocs(DOCS, "types/node")).toEqual([]);
  });

  it("acha pela origem (arquivo ou domínio)", () => {
    expect(filterKbDocs(DOCS, "baeldung").map((m) => m.doc.id)).toEqual(["d"]);
    expect(filterKbDocs(DOCS, "termos").map((m) => m.doc.id)).toEqual(["g"]);
  });
});

describe("origem, seções e título exato", () => {
  it("origem: o domínio do link sem www, ou o nome do arquivo", () => {
    expect(docOrigin(DOCS[3] as KbDoc)).toBe("baeldung.com");
    expect(docOrigin(DOCS[2] as KbDoc)).toBe("livro.pdf");
  });

  it("documento sem títulos não tem o passo da seção", () => {
    expect(hasSections(DOCS[0] as KbDoc)).toBe(true);
    expect(hasSections(DOCS[6] as KbDoc)).toBe(false);
  });

  it("título exato sem acento e sem caixa, só entre os prontos", () => {
    expect(docByTitle(DOCS, " guia de ESTILO da api ")?.id).toBe("a");
    expect(docByTitle(DOCS, "Manual do H2")).toBeNull();
    expect(docByTitle(DOCS, "")).toBeNull();
  });

  it("seções filtradas mantêm a ordem do documento", () => {
    const sections: KbSection[] = [
      { anchor: "erros", title: "Tratamento de erros", level: 2 },
      { anchor: "validacao", title: "Erros de validação", level: 3 },
      { anchor: "versao", title: "Versionamento", level: 2 },
    ];
    expect(filterKbSections(sections, "").map((s) => s.section.anchor)).toEqual(["erros", "validacao", "versao"]);
    expect(filterKbSections(sections, "erro").map((s) => s.section.anchor)).toEqual(["erros", "validacao"]);
    expect(filterKbSections(sections, "validacao")[0]?.ranges).toEqual([[9, 18]]);
  });
});
