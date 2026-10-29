/*
 * Menções da caixa da Conversa (spec 136 T06, critérios 3 e 5): o disparo do `@` só no início de
 * palavra, os trechos acompanhando as edições, a etiqueta inteira no Backspace e no Delete, o cursor
 * fora da etiqueta, o texto enviado com `kb:` e o rascunho refeito.
 */
import { describe, expect, it } from "vitest";
import { splitCitations } from "../knowledge/knowledgeModel";
import { decodeMentions, encodeMentions, findTrigger, insertMention, mentionLabel, reconcileMentions, removeMentionAt, snapCaret, type DraftMention } from "./mentions";

const GUIA = "guia-de-estilo-3fa9c2e1";
const REST = "padroes-rest-8b1d07aa";

function mention(text: string, label: string, doc: string, anchor: string | null = null, section: string | null = null): DraftMention {
  const start = text.indexOf(label);
  return { start, end: start + label.length, doc, anchor, title: "x", section };
}

describe("disparo do @", () => {
  it("no início do texto, depois de espaço ou de abre-parênteses", () => {
    expect(findTrigger("@gui", 4, [])).toEqual({ start: 0, query: "gui" });
    expect(findTrigger("siga o @gui", 11, [])).toEqual({ start: 7, query: "gui" });
    expect(findTrigger("(@gui", 5, [])).toEqual({ start: 1, query: "gui" });
    expect(findTrigger("siga @", 6, [])).toEqual({ start: 5, query: "" });
  });

  it("no meio de palavra, não: e-mail e `x@y`", () => {
    expect(findTrigger("mail@dominio", 12, [])).toBeNull();
    expect(findTrigger("x@y", 3, [])).toBeNull();
  });

  it("`@RestController` e `@types/node` são disparos; quem fecha é a lista sem resultado", () => {
    expect(findTrigger("use @RestController", 19, [])?.query).toBe("RestController");
    expect(findTrigger("npm i @types/node", 17, [])?.query).toBe("types/node");
  });

  it("espaço logo depois, outra linha, dentro de etiqueta ou consulta longa: não", () => {
    expect(findTrigger("@ guia", 6, [])).toBeNull();
    expect(findTrigger("@guia\nmais", 10, [])).toBeNull();
    const text = "@Guia ok";
    expect(findTrigger(text, 5, [mention(text, "@Guia", GUIA)])).toBeNull();
    expect(findTrigger(`@${"a".repeat(81)}`, 82, [])).toBeNull();
  });
});

describe("etiquetas no texto", () => {
  it("rótulo com a seção", () => {
    expect(mentionLabel("Guia", null)).toBe("@Guia");
    expect(mentionLabel("Guia\nda  API", "Erros")).toBe("@Guia da API #Erros");
  });

  it("inserir troca o @ e a consulta pela etiqueta, com um espaço e o cursor depois", () => {
    const edit = insertMention("siga o @gui fim", 7, 11, [], { doc: GUIA, anchor: "erros", title: "Guia", section: "Erros" });
    expect(edit.text).toBe("siga o @Guia #Erros fim");
    expect(edit.mentions).toEqual([{ start: 7, end: 19, doc: GUIA, anchor: "erros", title: "Guia", section: "Erros" }]);
    expect(edit.caret).toBe(20);
  });

  it("as edições antes deslocam, depois mantêm, e por dentro desfazem a etiqueta", () => {
    const text = "a @Guia b";
    const list = [mention(text, "@Guia", GUIA)];
    expect(reconcileMentions(text, `xx${text}`, list)[0]).toMatchObject({ start: 4, end: 9 });
    expect(reconcileMentions(text, `${text} c`, list)[0]).toMatchObject({ start: 2, end: 7 });
    expect(reconcileMentions(text, "a @Gxia b", list)).toEqual([]);
    expect(reconcileMentions(text, "a @Guia!b", list)[0]).toMatchObject({ start: 2, end: 7 });
  });

  it("Backspace no fim e Delete no começo apagam a etiqueta inteira; com seleção, não", () => {
    const text = "a @Guia #Erros b";
    const list = [mention(text, "@Guia #Erros", GUIA, "erros", "Erros")];
    expect(removeMentionAt(text, 14, 14, "Backspace", list)).toEqual({ text: "a  b", caret: 2, mentions: [] });
    expect(removeMentionAt(text, 2, 2, "Delete", list)?.text).toBe("a  b");
    expect(removeMentionAt(text, 2, 2, "Backspace", list)).toBeNull();
    expect(removeMentionAt(text, 10, 14, "Backspace", list)).toBeNull();
  });

  it("o cursor dentro da etiqueta vai para a borda mais próxima", () => {
    const text = "a @Guia b";
    const list = [mention(text, "@Guia", GUIA)];
    expect(snapCaret(3, list)).toBe(2);
    expect(snapCaret(6, list)).toBe(7);
    expect(snapCaret(8, list)).toBe(8);
  });
});

describe("texto enviado e rascunho", () => {
  it("as etiquetas viram kb:<id> e kb:<id>#<âncora>, lidas pelo mesmo parser do servidor", () => {
    const text = "siga @Guia #Erros e @Padrões.";
    const list = [mention(text, "@Guia #Erros", GUIA, "erros", "Erros"), mention(text, "@Padrões", REST)];
    const out = encodeMentions(text, list);
    expect(out).toBe(`siga kb:${GUIA}#erros e kb:${REST}.`);
    const cites = splitCitations(out).flatMap((p) => (p.kind === "cite" ? [[p.cite.doc, p.cite.section]] : []));
    expect(cites).toEqual([
      [GUIA, "erros"],
      [REST, null],
    ]);
  });

  it("o espaço depois da citação só entra quando a leitura mudaria", () => {
    const at = (text: string, label: string, anchor: string | null = null) => [mention(text, label, GUIA, anchor)];
    expect(encodeMentions("use o @Guia.", at("use o @Guia.", "@Guia"))).toBe(`use o kb:${GUIA}.`);
    expect(encodeMentions("@Guia_x", at("@Guia_x", "@Guia"))).toBe(`kb:${GUIA}_x`);
    expect(encodeMentions("@GuiaMais", at("@GuiaMais", "@Guia"))).toBe(`kb:${GUIA}Mais`);
    expect(encodeMentions("@Guia #E.Mais", at("@Guia #E.Mais", "@Guia #E", "ancora"))).toBe(`kb:${GUIA}#ancora .Mais`);
    expect(encodeMentions("@Guia-x", at("@Guia-x", "@Guia"))).toBe(`kb:${GUIA} -x`);
    expect(encodeMentions("@Guia#x", at("@Guia#x", "@Guia"))).toBe(`kb:${GUIA} #x`);
    expect(encodeMentions("@Guia #E", at("@Guia #E", "@Guia #E", "ancora"))).toBe(`kb:${GUIA}#ancora`);
  });

  it("letra, número ou _ colado antes da etiqueta ganha um espaço; o resto fica como está", () => {
    const at = (text: string) => [mention(text, "@Guia", GUIA)];
    for (const [text, expected] of [
      ["x@Guia fim", `x kb:${GUIA} fim`],
      ["9@Guia", `9 kb:${GUIA}`],
      ["_@Guia", `_ kb:${GUIA}`],
      ["é@Guia", `é kb:${GUIA}`],
      ["(@Guia)", `(kb:${GUIA})`],
      ["-@Guia", `-kb:${GUIA}`],
      ["veja @Guia", `veja kb:${GUIA}`],
    ] as const) {
      const out = encodeMentions(text, at(text));
      expect(out).toBe(expected);
      const cites = splitCitations(out).filter((p) => p.kind === "cite");
      expect(cites).toHaveLength(1);
    }
  });

  it("o rascunho refaz as etiquetas; o documento que saiu da base fica texto comum", () => {
    const encoded = `siga kb:${GUIA}#erros e kb:${REST} e kb:sumiu-0000aaaa fim`;
    const decoded = decodeMentions(encoded, (doc, anchor) => {
      if (doc === GUIA) return { title: "Guia", section: anchor === "erros" ? "Erros" : null };
      if (doc === REST) return { title: "Padrões REST", section: null };
      return null;
    });
    expect(decoded.text).toBe("siga @Guia #Erros e @Padrões REST e kb:sumiu-0000aaaa fim");
    expect(decoded.mentions.map((m) => decoded.text.slice(m.start, m.end))).toEqual(["@Guia #Erros", "@Padrões REST"]);
    expect(encodeMentions(decoded.text, decoded.mentions)).toBe(encoded);
  });
});
