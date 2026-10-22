import { describe, expect, it } from "vitest";
import { findHowToSection, parseReportMarkdown } from "./reportMarkdown";
import type { Block, ListBlock, ListItem } from "./reportMarkdown";

const titleOf = (block: Block | undefined): string =>
  block?.kind === "heading" || block?.kind === "paragraph"
    ? block.inlines.map((i) => i.text).join("")
    : (block?.kind ?? "");

/** Partes de um item na ordem do Markdown, como resumo legível. */
const shape = (item: ListItem | undefined): string[] =>
  (item?.parts ?? []).map((part) => {
    if (part.kind === "inlines") return `t:${part.inlines.map((i) => i.text).join("")}`;
    if (part.kind === "code") return `c:${part.block.text}`;
    return `l:${part.list.items.map((sub) => shape(sub).join("|")).join(";")}`;
  });

describe("F-01: bloco de código dentro de item de lista", () => {
  it("cerca indentada sob o item vira parte do item; numeração preservada", () => {
    const md = "1. Instale:\n   ```bash\n   npm ci\n   npm start\n   ```\n2. Abra http://localhost:3000";
    const blocks = parseReportMarkdown(md);
    expect(blocks).toHaveLength(1);
    const list = blocks[0] as ListBlock;
    expect(list.ordered).toBe(true);
    expect(list.items.map((item) => item.number)).toEqual([1, 2]);
    expect(shape(list.items[0])).toEqual(["t:Instale:", "c:npm ci\nnpm start"]);
    expect(shape(list.items[1])).toEqual(["t:Abra http://localhost:3000"]);
  });

  it("cerca sem fechamento vai até o fim do item (não engole o item seguinte)", () => {
    const list = parseReportMarkdown("1. Rode\n   ```\n   npm test\n2. Depois")[0] as ListBlock;
    expect(list.items.map((item) => item.number)).toEqual([1, 2]);
    expect(shape(list.items[0])).toEqual(["t:Rode", "c:npm test"]);
  });

  it("múltiplos blocos e parágrafo continuado no mesmo item, na ordem do Markdown", () => {
    const md = "1. Passo\n   continua aqui\n   ```\n   a\n   ```\n   ```\n   b\n   ```\n2. Próximo";
    const list = parseReportMarkdown(md)[0] as ListBlock;
    expect(shape(list.items[0])).toEqual(["t:Passo continua aqui", "c:a", "c:b"]);
  });

  it("linhas de comando dentro da cerca não viram itens de lista", () => {
    const list = parseReportMarkdown("1. Rode\n   ```\n   - não é item\n   2. nem isto\n   ```")[0] as ListBlock;
    expect(list.items).toHaveLength(1);
    expect(shape(list.items[0])).toEqual(["t:Rode", "c:- não é item\n2. nem isto"]);
  });

  it("F-07: texto depois da cerca fica depois do bloco; sublista também respeita a ordem", () => {
    const md = "2. Inicie:\n   ```bash\n   npm start\n   ```\n   Acesse http://localhost:3000\n   - sub um\n   - sub dois\n   Fim";
    const list = parseReportMarkdown(md)[0] as ListBlock;
    expect(shape(list.items[0])).toEqual([
      "t:Inicie:",
      "c:npm start",
      "t:Acesse http://localhost:3000",
      "l:t:sub um;t:sub dois",
      "t:Fim",
    ]);
  });
});

describe("F-02: tempo limitado em entradas patológicas", () => {
  const cases: [string, string][] = [
    ["colchetes sem fechamento", "[".repeat(65536)],
    ["imagens sem fechamento", "![".repeat(32768)],
    ["cabeçalho com espaços", `# a${" ".repeat(65536)}b`],
    ["asteriscos", "*".repeat(65536)],
    ["sublinhados", "_".repeat(65536)],
    ["crases", "`".repeat(65536)],
    ["link com parênteses", "[a](".repeat(16384)],
    ["F-06 item com espaços", `- ${" ".repeat(65536)} `],
    ["F-06 cabeçalho com espaços", `# ${" ".repeat(65536)} `],
    ["F-06 separador Unicode", `- ${" ".repeat(65536)}\u2028 b`],
  ];
  it.each(cases)("%s termina em menos de 1 s", (_name, input) => {
    const start = Date.now();
    parseReportMarkdown(input);
    expect(Date.now() - start).toBeLessThan(1000);
  });

  it("listas aninhadas profundas terminam em menos de 1 s", () => {
    const lines: string[] = [];
    for (let depth = 0; depth < 2000; depth += 1) lines.push(`${" ".repeat(depth)}- nível ${depth} \`x\``);
    const start = Date.now();
    parseReportMarkdown(lines.join("\n"));
    expect(Date.now() - start).toBeLessThan(1000);
  });

  it("parágrafo acima de 8 KiB vira texto simples (sem marcação)", () => {
    const [block] = parseReportMarkdown(`**${"a".repeat(9000)}**`);
    expect(block?.kind === "paragraph" ? block.inlines.map((i) => i.kind) : []).toEqual(["text"]);
  });

  it("linha de prosa acima de 1024 caracteres é limitada com reticências", () => {
    const [block] = parseReportMarkdown(`${"x".repeat(5000)}`);
    const text = block?.kind === "paragraph" ? block.inlines.map((i) => i.text).join("") : "";
    expect(text.length).toBe(1024);
    expect(text.endsWith("…")).toBe(true);
  });

  it("separadores Unicode viram quebra de linha e controles somem", () => {
    const blocks = parseReportMarkdown("um\u2028dois\u0007\n\n# t\u2029x");
    expect(blocks.map((b) => (b.kind === "paragraph" ? titleOf(b) : b.kind))).toEqual(["um dois", "heading", "x"]);
  });
});

describe("F-08/F-03: detecção da seção 'Como executar'", () => {
  it.each([
    ["## Como executar", "## Como executar\n\n1. npm ci"],
    ["### 3. Como rodar", "### 3. Como rodar\n\n1. npm start"],
    ["## Como executar o sistema", "## Como executar o sistema\n\n1. npm start"],
    ["**Como executar**", "**Como executar**\n\n1. npm start"],
    ["Como Executar:", "Como Executar:\n\n1. npm start"],
    ["## Como iniciar", "## Como iniciar\n\n1. npm start"],
    ["## Como subir", "## Como subir\n\n1. npm start"],
  ])("reconhece %s", (_label, md) => {
    const { howTo, rest } = findHowToSection(parseReportMarkdown(md));
    expect(howTo).not.toBeNull();
    expect(howTo?.some((b) => b.kind === "list")).toBe(true);
    expect(rest).toEqual([]);
  });

  it("parágrafo que só começa com 'Como executar' não é título e não engole o vizinho", () => {
    const md = "## Visão geral\nComo rodar é simples.\n\nTexto.\n\n## Testes";
    const { howTo, rest } = findHowToSection(parseReportMarkdown(md));
    expect(howTo).toBeNull();
    expect(rest.map(titleOf)).toEqual(["Visão geral", "Como rodar é simples.", "Texto.", "Testes"]);
  });

  it("'Como executar é simples.' em parágrafo não inicia seção", () => {
    const { howTo } = findHowToSection(parseReportMarkdown("Como executar é simples.\n\n1. x"));
    expect(howTo).toBeNull();
  });

  it("**Como testar** encerra a seção iniciada em cabeçalho e não é movido para ela", () => {
    const md = "## Como executar\n\n1. npm start\n\n**Como testar**\n\n1. npm test";
    const { howTo, rest } = findHowToSection(parseReportMarkdown(md));
    expect(howTo?.map(titleOf)).toEqual(["Como executar", "list"]);
    expect(rest.map(titleOf)).toEqual(["Como testar", "list"]);
  });

  it("título em negrito de seção comum (sem ser conhecida) não encerra a seção", () => {
    const md = "## Como executar\n\n1. npm start\n\n**Atenção**\n\nCuidado.";
    const { howTo } = findHowToSection(parseReportMarkdown(md));
    expect(howTo?.map(titleOf)).toEqual(["Como executar", "list", "Atenção", "Cuidado."]);
  });

  it("heading de nível 2 termina no próximo heading de nível 2 ou superior, e não em um de nível 3", () => {
    const md = "## Como executar\n\n1. npm start\n\n### Detalhe\n\nmais\n\n## Como parar\n\nCtrl+C";
    const { howTo, rest } = findHowToSection(parseReportMarkdown(md));
    expect(howTo?.map(titleOf)).toEqual(["Como executar", "list", "Detalhe", "mais"]);
    expect(rest.map(titleOf)).toEqual(["Como parar", "Ctrl+C"]);
  });

  it("título que não é de execução não é tratado como seção", () => {
    expect(findHowToSection(parseReportMarkdown("## Como testar\n\n1. npm test")).howTo).toBeNull();
  });
});
