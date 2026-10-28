/* Títulos canônicos do spec.md e das tarefas no idioma ativo (spec 131 T06): só a exibição muda. */
import { describe, expect, it } from "vitest";
import { isSddDocumentPath, localizeHeadingTitle, localizeSddHeadings } from "./sddHeadings";
import { translateCode } from "./serverCodes";

/** Trecho de uma tarefa do Test 9 (projeto en, títulos gravados em pt-BR). */
const TASK = [
  "---",
  'title: "Executable launcher"',
  "---",
  "## Objetivo",
  "Ship the launcher.",
  "",
  "## Critérios de Aceitação",
  "- [ ] Runs with `## Objetivo` in the text.",
  "",
  "```md",
  "## Objetivo",
  "```",
  "",
  "## Tentativas anteriores",
  "### Tentativa 2",
  "### Pergunta (developer)",
  "## Análise de recuperação #3",
  "## Registro de Apontamentos",
  "### Reprovação 1",
  "## Notes from the team",
].join("\n");

describe("títulos canônicos na exibição", () => {
  it("em en, os títulos da tarefa saem traduzidos; o resto, a contagem de linhas e o código não mudam", () => {
    const shown = localizeSddHeadings(TASK, "en");
    expect(shown.split("\n")).toHaveLength(TASK.split("\n").length);
    const lines = shown.split("\n");
    expect(lines[3]).toBe("## Objective");
    expect(lines[6]).toBe("## Acceptance Criteria");
    expect(lines[7]).toBe("- [ ] Runs with `## Objetivo` in the text.");
    expect(lines[10]).toBe("## Objetivo");
    expect(lines.slice(13)).toEqual([
      "## Previous attempts",
      "### Attempt 2",
      "### Question (developer)",
      "## Recovery analysis #3",
      "## Findings Log",
      "### Rejection 1",
      "## Notes from the team",
    ]);
  });

  it("em pt-BR, o arquivo gravado em pt aparece como está, e o título em en volta ao pt", () => {
    expect(localizeSddHeadings(TASK, "pt-BR")).toBe(TASK);
    expect(localizeHeadingTitle("Acceptance Criteria", "pt-BR")).toBe("Critérios de Aceitação");
    expect(localizeHeadingTitle("Desenvolvedor", "en")).toBe("Developer");
    expect(localizeHeadingTitle("critérios de aceitação globais", "en")).toBe("Global Acceptance Criteria");
    expect(localizeHeadingTitle("Outra seção", "en")).toBe("Outra seção");
  });

  it("só o spec.md e as tarefas de uma spec contam", () => {
    expect(isSddDocumentPath(".sdd/specs/001-ledger/spec.md")).toBe(true);
    expect(isSddDocumentPath(".sdd/specs/001-ledger/tasks/T06.md")).toBe(true);
    expect(isSddDocumentPath(".sdd/specs/001-ledger/report.md")).toBe(false);
    expect(isSddDocumentPath("docs/spec.md")).toBe(false);
  });

  it("a Atividade da leitura de uma seção da spec mostra o título no idioma ativo", () => {
    expect(translateCode("activity.code", "tool.read_spec_section", { title: "Abordagem Técnica" }, "en")).toBe(
      "reading the spec section Technical Approach",
    );
    expect(translateCode("activity.code", "tool.read_spec_section", { title: "Abordagem Técnica" }, "pt-BR")).toBe(
      "lendo a seção Abordagem Técnica da spec",
    );
    expect(translateCode("activity.code", "tool.read_spec_index", {}, "en")).toBe("reading the spec index");
  });
});
