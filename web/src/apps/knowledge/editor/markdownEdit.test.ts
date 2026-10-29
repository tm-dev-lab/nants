/*
 * Edição de markdown sobre texto puro (spec 136 T11): as ações da barra, a continuação de lista, o
 * recuo, o gatilho `[[`, o link para documento, a arrumação ao salvar e o realce (que nunca muda
 * um caractere do texto).
 */
import { describe, expect, it } from "vitest";
import { highlightMarkdown } from "./markdownHighlight";
import {
  continueList,
  cycleHeading,
  findLinkTrigger,
  indentList,
  insertDocLink,
  insertLink,
  linkTarget,
  tidyMarkdown,
  toggleBlock,
  toggleCode,
  toggleInline,
  type Edit,
  type Sel,
} from "./markdownEdit";

/** Aplica a troca e devolve o texto com a seleção marcada por `[` e `]` (`|` sem seleção). */
function apply(text: string, edit: Edit | null): string {
  if (edit === null) return "null";
  const out = text.slice(0, edit.from) + edit.insert + text.slice(edit.to);
  if (edit.selStart === edit.selEnd) return `${out.slice(0, edit.selStart)}|${out.slice(edit.selStart)}`;
  return `${out.slice(0, edit.selStart)}[${out.slice(edit.selStart, edit.selEnd)}]${out.slice(edit.selEnd)}`;
}

/** Seleção a partir de um texto com `|` (cursor) ou `[...]` (trecho). */
function sel(marked: string): Sel {
  const caret = marked.indexOf("|");
  if (caret >= 0) return { text: marked.replace("|", ""), start: caret, end: caret };
  const start = marked.indexOf("[");
  const end = marked.indexOf("]") - 1;
  return { text: marked.replace("[", "").replace("]", ""), start, end };
}

const run = (marked: string, f: (s: Sel) => Edit | null): string => {
  const s = sel(marked);
  return apply(s.text, f(s));
};

describe("negrito, itálico e código em linha", () => {
  it("põe as marcas no trecho e mantém o trecho selecionado", () => {
    expect(run("um [texto] aqui", (s) => toggleInline(s, "bold"))).toBe("um **[texto]** aqui");
    expect(run("um [texto] aqui", (s) => toggleInline(s, "italic"))).toBe("um *[texto]* aqui");
  });

  it("tira as marcas de quem já tem, por fora ou por dentro", () => {
    expect(run("um **[texto]** aqui", (s) => toggleInline(s, "bold"))).toBe("um [texto] aqui");
    expect(run("um [**texto**] aqui", (s) => toggleInline(s, "bold"))).toBe("um [texto] aqui");
  });

  it("sem seleção, o par vazio com o cursor no meio; espaços das pontas ficam de fora", () => {
    expect(run("a |b", (s) => toggleInline(s, "bold"))).toBe("a **|**b");
    expect(run("a[ texto ]b", (s) => toggleInline(s, "bold"))).toBe("a **[texto]** b");
  });

  it("o itálico dentro do negrito não confunde as marcas", () => {
    expect(run("**[forte]**", (s) => toggleInline(s, "italic"))).toBe("***[forte]***");
  });
});

describe("títulos, listas e citação", () => {
  it("o título troca o prefixo e o botão passa por 1, 2, 3 e volta ao texto", () => {
    expect(run("Tex|to", (s) => toggleBlock(s, "h2"))).toBe("## Tex|to");
    expect(run("- Tex|to", (s) => toggleBlock(s, "h1"))).toBe("# Tex|to");
    let text = "Tex|to";
    const steps: string[] = [];
    for (let i = 0; i < 4; i += 1) {
      text = run(text, cycleHeading);
      steps.push(text);
    }
    expect(steps).toEqual(["# Tex|to", "## Tex|to", "### Tex|to", "Tex|to"]);
  });

  it("lista, lista numerada e citação em várias linhas; com todas marcadas, tira", () => {
    expect(run("[um\ndois]", (s) => toggleBlock(s, "bullet"))).toBe("[- um\n- dois]");
    expect(run("[um\n\ndois]", (s) => toggleBlock(s, "numbered"))).toBe("[1. um\n\n2. dois]");
    expect(run("[- um\n- dois]", (s) => toggleBlock(s, "bullet"))).toBe("[um\ndois]");
    expect(run("cita|ção", (s) => toggleBlock(s, "quote"))).toBe("> cita|ção");
  });
});

describe("código e link", () => {
  it("numa linha, código em linha; em várias ou na linha vazia, um bloco", () => {
    expect(run("use [npm]", toggleCode)).toBe("use `[npm]`");
    expect(run("[a\nb]", toggleCode)).toBe("```\n[a\nb]\n```");
    expect(run("|", toggleCode)).toBe("```\n|\n```");
    expect(run("[```\na\n```]", toggleCode)).toBe("[a]");
  });

  it("Ctrl+K: texto vira rótulo com o cursor no endereço; endereço vira alvo com o cursor no rótulo", () => {
    expect(run("veja [o guia]", insertLink)).toBe("veja [o guia](|)");
    expect(run("veja [https://exemplo.com]", insertLink)).toBe("veja [|](https://exemplo.com)");
    expect(run("veja |", insertLink)).toBe("veja [|]()");
  });
});

describe("Enter nas listas e Tab", () => {
  it("continua a lista, a numeração, a tarefa e a citação", () => {
    expect(run("- um|", continueList)).toBe("- um\n- |");
    expect(run("  * um|", continueList)).toBe("  * um\n  * |");
    expect(run("9. nove|", continueList)).toBe("9. nove\n10. |");
    expect(run("1) um|", continueList)).toBe("1) um\n2) |");
    expect(run("- [x] feito|", continueList)).toBe("- [x] feito\n- [ ] |");
    expect(run("> dito|", continueList)).toBe("> dito\n> |");
  });

  it("no item vazio, encerra a lista; fora de lista ou com seleção, Enter comum", () => {
    expect(run("- um\n- |", continueList)).toBe("- um\n|");
    expect(run("texto|", continueList)).toBe("null");
    expect(run("- [um]", continueList)).toBe("null");
    expect(run("|- um", continueList)).toBe("null");
  });

  it("Tab avança e Shift+Tab recua o item; fora de lista, o Tab segue o foco", () => {
    expect(run("- um\n- do|is", (s) => indentList(s, false))).toBe("- um\n  - do|is");
    expect(run("  - do|is", (s) => indentList(s, true))).toBe("- do|is");
    expect(run("texto|", (s) => indentList(s, false))).toBe("null");
    expect(run("|", (s) => indentList(s, false))).toBe("null");
    expect(run("- um\n|\n", (s) => indentList(s, false))).toBe("null");
  });
});

describe("links para documentos da base", () => {
  it("`[[` antes do cursor, na mesma linha e sem `]`, é o gatilho", () => {
    expect(findLinkTrigger("veja [[web", 10)).toEqual({ start: 5, query: "web" });
    expect(findLinkTrigger("[[a]] b", 7)).toBeNull();
    expect(findLinkTrigger("[[a\nb", 5)).toBeNull();
  });

  it("o alvo é o nome do arquivo, com a âncora, e codifica o que quebraria o link", () => {
    expect(linkTarget("padroes-api.md", null)).toBe("padroes-api.md");
    expect(linkTarget("meu guia (v2).md", "erros")).toBe("meu%20guia%20%28v2%29.md#erros");
  });

  it("troca o `[[consulta` (e o `]]` que vier) pelo link markdown", () => {
    const text = "veja [[web]] depois";
    expect(apply(text, insertDocLink(text, { start: 5 }, 10, "Webhooks [v2]", "webhooks.md"))).toBe("veja [Webhooks v2](webhooks.md)| depois");
  });
});

describe("arrumação ao salvar", () => {
  it("tira espaços do fim, junta linhas em branco, usa `-` e termina com uma quebra", () => {
    expect(tidyMarkdown("\n\n# T  \n\n\n\ntexto \t\n* um\n+ dois\n**forte** fim\n\n")).toBe("# T\n\ntexto\n- um\n- dois\n**forte** fim\n");
  });

  it("mantém a quebra forçada de dois espaços e não toca nos blocos de código", () => {
    expect(tidyMarkdown("linha   \nseguinte")).toBe("linha  \nseguinte\n");
    expect(tidyMarkdown("```\n* x  \n\n\n\n```\n")).toBe("```\n* x  \n\n\n\n```\n");
  });

  it("não reescreve o texto já arrumado e o vazio continua vazio", () => {
    const tidy = "# Título\n\n- item\n\nTexto com [link](a.md).\n";
    expect(tidyMarkdown(tidy)).toBe(tidy);
    expect(tidyMarkdown("  \n\n")).toBe("");
  });
});

describe("realce", () => {
  it("repete o texto caractere por caractere, só com os papéis", () => {
    const text = "# Título\n\nUm **forte**, *leve*, `código` e [link](a.md) e [[doc]].\n> citação\n- item\n```\n# não é título\n```";
    const lines = highlightMarkdown(text);
    expect(lines.map((spans) => spans.map((s) => s.text).join("")).join("\n")).toBe(text);
    const roles = lines.flat().filter((s) => s.role !== null && s.role !== "mark").map((s) => `${s.role}:${s.text}`);
    expect(roles).toEqual(["heading:Título", "strong:forte", "emph:leve", "code:código", "link:link", "url:a.md", "link:doc", "quote:citação", "fence:# não é título"]);
  });
});
