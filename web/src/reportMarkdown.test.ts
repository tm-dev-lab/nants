import { describe, expect, it } from "vitest";
import { setLanguage } from "./i18n";
import { MAX_LIST_DEPTH, findHowToSection, parseReportMarkdown } from "./reportMarkdown";
import type { Block, Inline, ListBlock, ListItem } from "./reportMarkdown";

const text = (inlines: Inline[]): string => inlines.map((i) => i.text).join("");
const itemText = (item: ListItem | undefined): string =>
  (item?.parts ?? []).map((part) => (part.kind === "inlines" ? text(part.inlines) : "")).join("");
const childLists = (item: ListItem | undefined): ListBlock[] =>
  (item?.parts ?? []).flatMap((part) => (part.kind === "list" ? [part.list] : []));

describe("parseReportMarkdown: blocos", () => {
  it("títulos #, ## e ### com níveis 1..3; mais fundos viram nível 3", () => {
    const blocks = parseReportMarkdown("# Um\n## Dois\n### Três\n#### Quatro");
    expect(blocks).toEqual([
      { kind: "heading", level: 1, inlines: [{ kind: "text", text: "Um" }] },
      { kind: "heading", level: 2, inlines: [{ kind: "text", text: "Dois" }] },
      { kind: "heading", level: 3, inlines: [{ kind: "text", text: "Três" }] },
      { kind: "heading", level: 3, inlines: [{ kind: "text", text: "Quatro" }] },
    ]);
  });

  it("linhas consecutivas formam um parágrafo; linha em branco separa", () => {
    const blocks = parseReportMarkdown("primeira linha\nsegunda linha\n\nterceiro");
    expect(blocks.map((b) => (b.kind === "paragraph" ? text(b.inlines) : b.kind))).toEqual([
      "primeira linha segunda linha",
      "terceiro",
    ]);
  });

  it("lista ordenada preserva a numeração escrita (valor de cada item)", () => {
    const [block] = parseReportMarkdown("3. três\n4. quatro\n7. sete");
    expect(block?.kind).toBe("list");
    const list = block as ListBlock;
    expect(list.ordered).toBe(true);
    expect(list.items.map((item) => item.number)).toEqual([3, 4, 7]);
    expect(list.items.map((item) => itemText(item))).toEqual(["três", "quatro", "sete"]);
  });

  it("listas não ordenadas aceitam -, * e +", () => {
    const [block] = parseReportMarkdown("- a\n* b\n+ c");
    expect((block as ListBlock).ordered).toBe(false);
    expect((block as ListBlock).items).toHaveLength(3);
  });

  it("lista aninhada por indentação vira filha do item anterior", () => {
    const blocks = parseReportMarkdown("1. Instale\n   - npm ci\n   - opcional\n2. Rode\n");
    expect(blocks).toHaveLength(1);
    const list = blocks[0] as ListBlock;
    expect(list.items.map((item) => item.number)).toEqual([1, 2]);
    const children = childLists(list.items[0]);
    expect(children).toHaveLength(1);
    expect(children[0]?.ordered).toBe(false);
    expect(children[0]?.items.map((item) => itemText(item))).toEqual(["npm ci", "opcional"]);
    expect(childLists(list.items[1])).toEqual([]);
  });

  it("lista com linha em branco entre itens continua a mesma lista", () => {
    const blocks = parseReportMarkdown("1. a\n\n2. b");
    expect(blocks).toHaveLength(1);
    expect((blocks[0] as ListBlock).items.map((item) => item.number)).toEqual([1, 2]);
  });

  it("linha de continuação indentada entra no texto do item", () => {
    const [block] = parseReportMarkdown("1. Rode o comando\n   com a flag --fast");
    expect(itemText((block as ListBlock).items[0])).toBe("Rode o comando com a flag --fast");
  });

  it("bloco de código preserva o conteúdo literal e a linguagem", () => {
    const [block] = parseReportMarkdown("```bash\nnpm test\n  <b>x</b>\n```");
    expect(block).toEqual({ kind: "code", lang: "bash", text: "npm test\n  <b>x</b>" });
  });

  it("bloco de código sem fechamento vai até o fim do texto", () => {
    const [block] = parseReportMarkdown("```\nlinha 1\nlinha 2");
    expect(block).toEqual({ kind: "code", lang: "", text: "linha 1\nlinha 2" });
  });

  it("profundidade de lista é limitada (sem recursão infinita nem estouro)", () => {
    const lines: string[] = [];
    for (let depth = 0; depth < 400; depth += 1) lines.push(`${" ".repeat(depth)}- nível ${depth}`);
    const blocks = parseReportMarkdown(lines.join("\n"));
    let depth = 0;
    let current: Block | undefined = blocks[0];
    while (current?.kind === "list") {
      depth += 1;
      current = childLists(current.items[0])[0];
    }
    expect(depth).toBeLessThanOrEqual(MAX_LIST_DEPTH);
    expect(depth).toBeGreaterThan(0);
  });
});

describe("parseReportMarkdown: texto inline", () => {
  it("código inline, negrito e ênfase", () => {
    const [block] = parseReportMarkdown("Rode `npm test`, **com cuidado** e *devagar* ou _assim_.");
    expect(block?.kind === "paragraph" ? block.inlines : []).toEqual([
      { kind: "text", text: "Rode " },
      { kind: "code", text: "npm test" },
      { kind: "text", text: ", " },
      { kind: "strong", text: "com cuidado" },
      { kind: "text", text: " e " },
      { kind: "em", text: "devagar" },
      { kind: "text", text: " ou " },
      { kind: "em", text: "assim" },
      { kind: "text", text: "." },
    ]);
  });

  it("asterisco solto entre espaços não vira ênfase", () => {
    const [block] = parseReportMarkdown("2 * 3 * 4 = 24");
    expect(block?.kind === "paragraph" ? block.inlines.map((i) => i.kind) : []).toEqual(["text"]);
  });

  it("link http(s) vira link com o rótulo (spec 136 T11); relativo e sem esquema viram só o rótulo", () => {
    const inlines = (md: string): Inline[] => {
      const [block] = parseReportMarkdown(md);
      return block?.kind === "paragraph" ? block.inlines : [];
    };
    expect(inlines("[site](https://exemplo.com)")).toEqual([{ kind: "link", text: "site", href: "https://exemplo.com" }]);
    expect(inlines("[a](http://exemplo.com/x?y=1)")).toEqual([{ kind: "link", text: "a", href: "http://exemplo.com/x?y=1" }]);
    expect(inlines("[banco](banco.md#versoes)")).toEqual([{ kind: "text", text: "banco" }]);
    expect(inlines("[f](file:///etc/passwd)")).toEqual([{ kind: "text", text: "f" }]);
    expect(inlines("[d](data:text/html,oi)")).toEqual([{ kind: "text", text: "d" }]);
    expect(inlines("[h](https://)")).toEqual([{ kind: "text", text: "h" }]);
  });

  it("link com esquema perigoso vira só o rótulo, sem URL", () => {
    const [block] = parseReportMarkdown("[x](javascript:alert(1))");
    expect(block?.kind === "paragraph" ? block.inlines : []).toEqual([{ kind: "text", text: "x" }]);
  });

  it("imagem vira só o texto alternativo", () => {
    const [block] = parseReportMarkdown("![i](http://x)");
    expect(block?.kind === "paragraph" ? block.inlines : []).toEqual([{ kind: "text", text: "i" }]);
  });

  it("imagem sem texto alternativo vira a reserva no idioma ativo", () => {
    const alt = (): Inline[] => {
      const [block] = parseReportMarkdown("![](http://x)");
      return block?.kind === "paragraph" ? block.inlines : [];
    };
    expect(alt()).toEqual([{ kind: "text", text: "imagem" }]);
    setLanguage("en", { persist: false });
    try {
      expect(alt()).toEqual([{ kind: "text", text: "image" }]);
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });

  it("HTML bruto permanece como texto literal (não é interpretado)", () => {
    const [block] = parseReportMarkdown("<script>alert(1)</script>");
    expect(block?.kind === "paragraph" ? text(block.inlines) : "").toBe("<script>alert(1)</script>");
  });
});

describe("parseReportMarkdown: robustez", () => {
  it("texto vazio ou só espaços não gera blocos", () => {
    expect(parseReportMarkdown("")).toEqual([]);
    expect(parseReportMarkdown("   \n\n  \n")).toEqual([]);
  });

  it("markdown grande (acima de 64 KiB) é analisado sem lançar", () => {
    const line = "- item com `código` e **negrito** em uma linha bem comprida para encher o texto\n";
    const big = line.repeat(1200);
    expect(big.length).toBeGreaterThan(64 * 1024);
    const blocks = parseReportMarkdown(big);
    expect(blocks.length).toBeGreaterThan(0);
  });
});

describe("findHowToSection", () => {
  it("separa a seção 'Como executar' até o próximo título de mesmo nível ou superior", () => {
    const blocks = parseReportMarkdown(
      "# Título\n\nResumo.\n\n## Como executar\n\n1. npm ci\n2. npm start\n\n### Detalhe\n\nmais\n\n## Como parar\n\nCtrl+C",
    );
    const { howTo, rest } = findHowToSection(blocks);
    expect(howTo?.map((b) => (b.kind === "heading" ? text(b.inlines) : b.kind))).toEqual([
      "Como executar",
      "list",
      "Detalhe",
      "paragraph",
    ]);
    expect(rest.map((b) => (b.kind === "heading" ? text(b.inlines) : b.kind))).toEqual([
      "Título",
      "paragraph",
      "Como parar",
      "paragraph",
    ]);
  });

  it("aceita variações de caixa, numeração e dois-pontos no título", () => {
    const { howTo } = findHowToSection(parseReportMarkdown("### 3. COMO EXECUTAR:\n\n1. passo"));
    expect(howTo).not.toBeNull();
  });

  it("relatório em inglês (spec 102): 'How to run' é a seção de execução e 'How to stop' a encerra", () => {
    const blocks = parseReportMarkdown("# Title\n\nSummary.\n\n## How to run\n\n1. npm ci\n2. npm start\n\n**How to stop:**\n\nCtrl+C\n\n## Decisions\n\n- SQLite");
    const { howTo, rest } = findHowToSection(blocks);
    expect(howTo?.map((b) => (b.kind === "heading" ? text(b.inlines) : b.kind))).toEqual(["How to run", "list"]);
    expect(rest.map((b) => (b.kind === "heading" ? text(b.inlines) : b.kind))).toEqual(["Title", "paragraph", "paragraph", "paragraph", "Decisions", "list"]);
    for (const title of ["### 2. HOW TO START:", "**How to launch**", "How to execute the app:"]) {
      expect(findHowToSection(parseReportMarkdown(`${title}\n\n1. step`)).howTo).not.toBeNull();
    }
    // Outras seções em inglês não são a de execução.
    for (const title of ["## How to test", "## How to use", "## Next steps", "## How it works"]) {
      expect(findHowToSection(parseReportMarkdown(`${title}\n\n1. step`)).howTo).toBeNull();
    }
  });

  it("a seção em pt-BR termina num título conhecido em inglês (e vice-versa)", () => {
    const pt = findHowToSection(parseReportMarkdown("**Como executar:**\n\n1. npm start\n\n**Prerequisites:**\n\nNode"));
    expect(pt.howTo?.map((b) => b.kind)).toEqual(["paragraph", "list"]);
    const en = findHowToSection(parseReportMarkdown("**How to run:**\n\n1. npm start\n\n**Pré-requisitos:**\n\nNode"));
    expect(en.howTo?.map((b) => b.kind)).toEqual(["paragraph", "list"]);
  });

  it("sem seção, devolve null e mantém todos os blocos", () => {
    const blocks = parseReportMarkdown("# Só resumo\n\ntexto");
    expect(findHowToSection(blocks)).toEqual({ howTo: null, rest: blocks });
  });
});

describe("parseReportMarkdown: keepLineBreaks (mensagens de agentes)", () => {
  it("padrão: linhas consecutivas continuam unidas", () => {
    const [block] = parseReportMarkdown("a/b.rs\nc/d.rs");
    expect(block?.kind === "paragraph" ? text(block.inlines) : "").toBe("a/b.rs c/d.rs");
  });

  it("preserva a quebra simples como inline break, sem HTML", () => {
    const blocks = parseReportMarkdown("a/b.rs\n<b>x</b>\nc/d.rs", { keepLineBreaks: true });
    expect(blocks).toHaveLength(1);
    const block = blocks[0];
    expect(block?.kind).toBe("paragraph");
    const inlines = block?.kind === "paragraph" ? block.inlines : [];
    expect(inlines.filter((i) => i.kind === "break")).toHaveLength(2);
    expect(text(inlines)).toBe("a/b.rs\n<b>x</b>\nc/d.rs");
    expect(inlines.every((i) => ["text", "break"].includes(i.kind))).toBe(true);
  });

  it("continua linear em entrada patológica", () => {
    const big = "x\n".repeat(20000);
    const start = Date.now();
    parseReportMarkdown(big, { keepLineBreaks: true });
    expect(Date.now() - start).toBeLessThan(2000);
  });
});
