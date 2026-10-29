/*
 * Realce do markdown no campo de escrita (spec 136 T11): a camada atrás do `textarea` repete o
 * texto, caractere por caractere, em trechos com classe. Só a cor muda (nunca o peso, o tamanho ou
 * a família): assim cada letra ocupa o mesmo lugar que no campo, e o cursor e a seleção do campo
 * caem certos sobre a camada. Os marcadores (`#`, `**`, `-`, `>`, `[`, `](...)`) ficam apagados e o
 * conteúdo (título, negrito, link, código) ganha a cor do papel.
 */

/** Papel do trecho; `null` é texto comum. */
type HlRole = "mark" | "heading" | "strong" | "emph" | "code" | "link" | "url" | "quote" | "fence" | null;

export interface HlSpan {
  text: string;
  role: HlRole;
}

/** Acima deste tamanho o realce sai (o campo fica com o texto visível, sem a camada). */
export const MAX_HIGHLIGHT_CHARS = 200_000;

const INLINE = /(`+)([^`]+?)\1|\*\*([^*\n]+?)\*\*|__([^_\n]+?)__|(?<![*\w])\*([^*\n]+?)\*(?!\*)|(?<![_\w])_([^_\n]+?)_(?!\w)|\[\[([^\]\n]+)\]\]|\[([^\]\n]*)\]\(([^)\s]*)\)/g;

function inline(text: string, base: HlRole, out: HlSpan[]): void {
  let last = 0;
  INLINE.lastIndex = 0;
  for (let m = INLINE.exec(text); m !== null; m = INLINE.exec(text)) {
    if (m.index > last) out.push({ text: text.slice(last, m.index), role: base });
    const [whole, ticks, code, strongA, strongB, emA, emB, wiki, label, url] = m;
    if (ticks !== undefined && code !== undefined) {
      out.push({ text: ticks, role: "mark" }, { text: code, role: "code" }, { text: ticks, role: "mark" });
    } else if (strongA !== undefined || strongB !== undefined) {
      const mk = whole.slice(0, 2);
      out.push({ text: mk, role: "mark" }, { text: strongA ?? strongB ?? "", role: "strong" }, { text: mk, role: "mark" });
    } else if (emA !== undefined || emB !== undefined) {
      const mk = whole.slice(0, 1);
      out.push({ text: mk, role: "mark" }, { text: emA ?? emB ?? "", role: "emph" }, { text: mk, role: "mark" });
    } else if (wiki !== undefined) {
      out.push({ text: "[[", role: "mark" }, { text: wiki, role: "link" }, { text: "]]", role: "mark" });
    } else if (label !== undefined && url !== undefined) {
      out.push({ text: "[", role: "mark" }, { text: label, role: "link" }, { text: "](", role: "mark" }, { text: url, role: "url" }, { text: ")", role: "mark" });
    } else {
      out.push({ text: whole, role: base });
    }
    last = m.index + whole.length;
  }
  if (last < text.length) out.push({ text: text.slice(last), role: base });
}

const HEADING = /^(\s{0,3}#{1,6} )(.*)$/;
const QUOTE = /^(\s*>+ ?)(.*)$/;
const LIST = /^(\s*(?:[-*+]|\d{1,9}[.)]) (?:\[[ xX]\] )?)(.*)$/;
const FENCE = /^\s*(`{3,}|~{3,})/;

/** Trechos de cada linha do texto (as linhas, sem o `\n`). */
export function highlightMarkdown(text: string): HlSpan[][] {
  const lines = text.split("\n");
  const out: HlSpan[][] = [];
  let fence: string | null = null;
  for (const line of lines) {
    const spans: HlSpan[] = [];
    const opener = FENCE.exec(line);
    if (fence !== null) {
      if (opener?.[1]?.startsWith(fence)) {
        spans.push({ text: line, role: "mark" });
        fence = null;
      } else if (line !== "") {
        spans.push({ text: line, role: "fence" });
      }
      out.push(spans);
      continue;
    }
    if (opener) {
      fence = opener[1] ?? "```";
      spans.push({ text: line, role: "mark" });
      out.push(spans);
      continue;
    }
    const heading = HEADING.exec(line);
    const quote = heading ? null : QUOTE.exec(line);
    const list = heading || quote ? null : LIST.exec(line);
    if (heading) {
      spans.push({ text: heading[1] ?? "", role: "mark" });
      inline(heading[2] ?? "", "heading", spans);
    } else if (quote) {
      spans.push({ text: quote[1] ?? "", role: "mark" });
      inline(quote[2] ?? "", "quote", spans);
    } else if (list) {
      spans.push({ text: list[1] ?? "", role: "mark" });
      inline(list[2] ?? "", null, spans);
    } else if (line !== "") {
      inline(line, null, spans);
    }
    out.push(spans);
  }
  return out;
}
