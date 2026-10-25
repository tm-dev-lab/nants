/* Tabelas GFM no Markdown restrito (spec 126 T04): casos válidos, inválidos e o caso real do Test 4. */
import { describe, expect, it } from "vitest";
import { MAX_TABLE_COLUMNS, parseReportMarkdown } from "./reportMarkdown";
import type { Block, Inline, TableBlock } from "./reportMarkdown";

const text = (inlines: Inline[]): string => inlines.map((i) => i.text).join("");
const tables = (blocks: Block[]): TableBlock[] => blocks.flatMap((b) => (b.kind === "table" ? [b] : []));
const cells = (table: TableBlock | undefined): string[][] => [
  (table?.header ?? []).map(text),
  ...(table?.rows ?? []).map((row) => row.map(text)),
];

/** Resposta real do Arquiteto no Test 4 (`.chat.jsonl`), trecho com a tabela. */
const REAL = `The plan is saved as **\`001-first-playable\`**, awaiting approval.

**Confirmed:** view distance is a **12-chunk horizontal radius (384 blocks)**, with vertical chunks loaded as needed.

| Task | Area | Main scope |
|---|---|---|
| T01 | Infrastructure | Exact dependencies, application shell, Node and Playwright test harness |
| T02 | World and meshing | Worker generation, typed chunks, face culling/greedy meshing, baked AO |
| T03 | Rendering | Modern lighting, spatial batching, real draw-call counters |
| T04 | Movement | Collision, ray targeting, desktop and touch controls |
| T05 | Networking | Binary WebSockets, 20 Hz authority, prediction, reconciliation, AOI |
| T06 | Editing | Server validation, revisioned deltas, client convergence |
| T07 | Persistence | Durable journal, atomic snapshots, restart and crash recovery |
| T08 | Acceptance and docs | Cross-viewport evidence, execution guide, benchmark and extension notes |

Tasks follow that dependency order, with feature tests added alongside each implementation.

### Key test decisions
- **Fast:** \`npm run test:fast\`, Node unit and integration tests.`;

describe("parseReportMarkdown: tabelas válidas", () => {
  it("caso real do Test 4: cabeçalho, 8 linhas e o texto antes e depois", () => {
    for (const keepLineBreaks of [false, true]) {
      const blocks = parseReportMarkdown(REAL, { keepLineBreaks });
      expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "paragraph", "table", "paragraph", "heading", "list"]);
      const [table] = tables(blocks);
      expect(table?.align).toEqual([null, null, null]);
      const all = cells(table);
      expect(all[0]).toEqual(["Task", "Area", "Main scope"]);
      expect(all).toHaveLength(9);
      expect(all[8]).toEqual(["T08", "Acceptance and docs", "Cross-viewport evidence, execution guide, benchmark and extension notes"]);
    }
  });

  it("alinhamento por coluna: sem marca, esquerda, centro e direita", () => {
    const [table] = tables(parseReportMarkdown("| a | b | c | d |\n| --- | :--- | :-: | --: |\n| 1 | 2 | 3 | 4 |"));
    expect(table?.align).toEqual([null, "left", "center", "right"]);
    expect(cells(table)).toEqual([["a", "b", "c", "d"], ["1", "2", "3", "4"]]);
  });

  it("sem os pipes das pontas, com uma coluna e sem corpo", () => {
    expect(cells(tables(parseReportMarkdown("a | b\n--- | ---\n1 | 2"))[0])).toEqual([["a", "b"], ["1", "2"]]);
    expect(cells(tables(parseReportMarkdown("| só |\n|---|\n| x |"))[0])).toEqual([["só"], ["x"]]);
    const [empty] = tables(parseReportMarkdown("| a | b |\n|---|---|"));
    expect(empty?.rows).toEqual([]);
  });

  it("linhas com células a menos ou a mais ficam com o número de colunas do cabeçalho", () => {
    const [table] = tables(parseReportMarkdown("| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |"));
    expect(cells(table)).toEqual([["a", "b"], ["1", ""], ["1", "2"]]);
  });

  it("pipe escapado é texto; marcação inline vale nas células", () => {
    const [table] = tables(parseReportMarkdown("| cmd | nota |\n|---|---|\n| `a \\| b` | **forte** e *leve* |"));
    expect(table?.rows[0]?.[0]).toEqual([{ kind: "code", text: "a | b" }]);
    expect(table?.rows[0]?.[1]?.map((i) => i.kind)).toEqual(["strong", "text", "em"]);
  });

  it("a tabela interrompe o parágrafo e termina na linha em branco, num bloco ou numa linha sem pipe", () => {
    const blocks = parseReportMarkdown("Resumo:\n| a | b |\n|---|---|\n| 1 | 2 |\nfim sem pipe\n\n| x |\n|---|\n| y |\n- item");
    expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "table", "paragraph", "table", "list"]);
    expect(blocks[0]?.kind === "paragraph" ? text(blocks[0].inlines) : "").toBe("Resumo:");
    expect(cells(tables(blocks)[0])).toEqual([["a", "b"], ["1", "2"]]);
    expect(cells(tables(blocks)[1])).toEqual([["x"], ["y"]]);
  });
});

describe("parseReportMarkdown: o que não é tabela", () => {
  const kinds = (md: string) => parseReportMarkdown(md).map((b) => b.kind);

  it("sem linha separadora, ou separador com outro número de colunas", () => {
    expect(kinds("| a | b |\n| 1 | 2 |")).toEqual(["paragraph"]);
    expect(kinds("| a | b |\n|---|")).toEqual(["paragraph"]);
    expect(kinds("| a |\n|---|---|")).toEqual(["paragraph"]);
  });

  it("separador inválido: sem traço, com outro caractere ou só dois-pontos", () => {
    expect(kinds("| a | b |\n| :: | --- |")).toEqual(["paragraph"]);
    expect(kinds("| a | b |\n| -x- | --- |")).toEqual(["paragraph"]);
    expect(kinds("| a | b |\n|  | --- |")).toEqual(["paragraph"]);
  });

  it("linha sem pipe seguida de traços é texto (não é tabela)", () => {
    expect(kinds("título\n---")).toEqual(["paragraph"]);
    expect(kinds("a\n- b")).toEqual(["paragraph", "list"]);
  });

  it("indentação de 4 colunas e cabeçalho acima do teto de colunas", () => {
    expect(kinds("    | a | b |\n    |---|---|")).toEqual(["paragraph"]);
    const wide = Array.from({ length: MAX_TABLE_COLUMNS + 1 }, (_, k) => `c${k}`);
    expect(kinds(`| ${wide.join(" | ")} |\n|${wide.map(() => "---").join("|")}|`)).toEqual(["paragraph"]);
    const max = wide.slice(0, MAX_TABLE_COLUMNS);
    expect(kinds(`| ${max.join(" | ")} |\n|${max.map(() => "---").join("|")}|`)).toEqual(["table"]);
  });

  it("dentro de bloco de código, a tabela é código", () => {
    expect(kinds("```\n| a | b |\n|---|---|\n```")).toEqual(["code"]);
  });
});
