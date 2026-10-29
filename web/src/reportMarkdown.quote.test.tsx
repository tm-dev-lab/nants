/*
 * Renderizador compartilhado, spec 136 T11: a citação (`>`), a lista de tarefas, os marcadores das
 * listas e o link clicável só para `http:` e `https:`, em outra aba e sem `opener`. Nada vira HTML.
 */
import { cleanup, render, screen } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { MarkdownText } from "./apps/shared/ReportMarkdown";
import { MAX_QUOTE_DEPTH, parseReportMarkdown } from "./reportMarkdown";

afterEach(cleanup);

describe("citação", () => {
  it("as linhas seguidas com `>` viram um bloco, com a mesma análise dentro", () => {
    const blocks = parseReportMarkdown("> **Atenção:** leia antes.\n> - um\n> - dois\n\nDepois.");
    expect(blocks.map((b) => b.kind)).toEqual(["quote", "paragraph"]);
    const quote = blocks[0];
    expect(quote?.kind === "quote" ? quote.blocks.map((b) => b.kind) : []).toEqual(["paragraph", "list"]);
  });

  it("a citação interrompe o parágrafo e aceita `>` sem espaço", () => {
    const blocks = parseReportMarkdown("Antes\n>dentro");
    expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "quote"]);
  });

  it("o aninhamento para na profundidade máxima: o resto vira parágrafo", () => {
    const deep = parseReportMarkdown(`${">".repeat(MAX_QUOTE_DEPTH + 3)} fundo`);
    let level = 0;
    let current = deep[0];
    while (current?.kind === "quote") {
      level += 1;
      current = current.blocks[0];
    }
    expect(level).toBe(MAX_QUOTE_DEPTH);
    expect(current?.kind).toBe("paragraph");
  });

  it("`>` dentro de um bloco de código continua código", () => {
    expect(parseReportMarkdown("```\n> não é citação\n```").map((b) => b.kind)).toEqual(["code"]);
  });
});

describe("lista de tarefas", () => {
  it("`[ ]` e `[x]` no começo do item marcam a tarefa; o item comum fica sem a marca", () => {
    const [list] = parseReportMarkdown("- [ ] abrir\n- [x] fechar\n- [X] também\n- comum\n- [link](a.md)");
    const items = list?.kind === "list" ? list.items : [];
    expect(items.map((i) => i.checked)).toEqual([false, true, true, undefined, undefined]);
    expect(items[0]?.parts).toEqual([{ kind: "inlines", inlines: [{ kind: "text", text: "abrir" }] }]);
  });

  it("vira uma caixa desabilitada, com o nome acessível do estado", () => {
    render(<MarkdownText text={"- [ ] revisar\n- [x] testar"} />);
    const boxes = screen.getAllByRole("checkbox");
    expect(boxes.map((b) => [(b as HTMLInputElement).checked, (b as HTMLInputElement).disabled])).toEqual([
      [false, true],
      [true, true],
    ]);
    expect(boxes.map((b) => b.getAttribute("aria-label"))).toEqual(["Tarefa pendente", "Tarefa concluída"]);
  });
});

describe("render", () => {
  it("o link http(s) abre em outra aba, sem opener; o relativo é só texto", () => {
    render(<MarkdownText text="Veja o [guia](https://exemplo.com/guia) e o [banco](banco.md)." />);
    const link = screen.getByRole("link", { name: "guia" });
    expect([link.getAttribute("href"), link.getAttribute("target"), link.getAttribute("rel")]).toEqual(["https://exemplo.com/guia", "_blank", "noopener noreferrer"]);
    expect(screen.queryByRole("link", { name: "banco" })).toBeNull();
    expect(screen.getByText("banco")).toBeTruthy();
  });

  it("a citação é um blockquote e nada vira HTML", () => {
    const { container } = render(<MarkdownText text={"> <b>oi</b> <script>x</script>\n> [j](javascript:alert(1))"} />);
    const quote = container.querySelector("blockquote.nd-md-quote");
    expect(quote?.textContent).toContain("<b>oi</b> <script>x</script>");
    expect(container.querySelector("b, script")).toBeNull();
    expect(container.querySelector("a")).toBeNull();
  });

  it("as listas mostram o marcador (a base do Tailwind o tira) e a tarefa troca o marcador pela caixa", () => {
    const css = readFileSync(resolve(process.cwd(), "src/apps/shared/apps.css"), "utf8");
    expect(css).toMatch(/ul\.nd-md-list \{\s*list-style: disc;/);
    expect(css).toMatch(/ol\.nd-md-list \{\s*list-style: decimal;/);
    expect(css).toMatch(/\.nd-md-list li\[data-task\] \{\s*list-style: none;/);
  });
});
