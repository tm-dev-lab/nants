/*
 * Menções da caixa da Conversa (spec 136 T06): o campo continua um `textarea`; cada menção é um
 * trecho do texto (`@Título` ou `@Título #Seção`) com o documento e a âncora ao lado. As funções
 * daqui são puras: acham o disparo do `@`, acompanham os trechos a cada edição, apagam a menção
 * inteira e montam o texto enviado com `kb:<id>` ou `kb:<id>#<âncora>` (o que o servidor lê, T05).
 * O rascunho guardado é o texto codificado; `decodeMentions` refaz as etiquetas ao carregar.
 */
import { splitCitations } from "../knowledge/knowledgeModel";

export interface DraftMention {
  /** Trecho `[start, end)` no texto do campo. */
  start: number;
  end: number;
  doc: string;
  anchor: string | null;
  title: string;
  /** Título da seção, quando a menção é de uma seção. */
  section: string | null;
}

/** Teto do título na etiqueta (o título inteiro aparece na lista e na prévia). */
const MAX_LABEL_TITLE = 60;

function oneLine(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > MAX_LABEL_TITLE ? `${flat.slice(0, MAX_LABEL_TITLE - 3).trimEnd()}...` : flat;
}

/** Texto da etiqueta no campo. */
export function mentionLabel(title: string, section: string | null): string {
  return section === null ? `@${oneLine(title)}` : `@${oneLine(title)} #${oneLine(section)}`;
}

/** Caracteres que podem vir logo antes do `@` (além do início e do espaço). */
const OPENERS = new Set(["(", "[", "{", "\"", "'"]);
/** Consulta mais longa que isto não é mais uma menção sendo escrita. */
const MAX_QUERY = 80;

interface MentionTrigger {
  /** Posição do `@`. */
  start: number;
  /** Texto entre o `@` e o cursor. */
  query: string;
}

/**
 * O `@` sendo escrito antes do cursor (C-11): só no início de uma palavra (início do texto, depois
 * de espaço ou de um abre-parênteses ou aspas), na mesma linha, fora de uma menção e sem espaço logo
 * depois. `e-mail@dominio` e `x@y` não disparam. A lista decide se abre (sem resultado, fecha).
 */
export function findTrigger(text: string, caret: number, mentions: readonly DraftMention[]): MentionTrigger | null {
  const lineStart = text.lastIndexOf("\n", caret - 1) + 1;
  const at = text.lastIndexOf("@", caret - 1);
  if (at < lineStart) return null;
  if (mentions.some((m) => at >= m.start && at < m.end)) return null;
  const before = at > 0 ? text.charAt(at - 1) : "";
  if (before !== "" && !/\s/.test(before) && !OPENERS.has(before)) return null;
  const query = text.slice(at + 1, caret);
  if (query.length > MAX_QUERY || /^\s/.test(query) || query.includes("@")) return null;
  return { start: at, query };
}

/**
 * Acompanha as menções numa edição qualquer do campo (digitar, colar, cortar, corretor): o que fica
 * antes da parte trocada continua, o que fica depois anda junto, e a menção tocada pela troca deixa
 * de ser menção (o texto dela fica).
 */
export function reconcileMentions(prev: string, next: string, mentions: readonly DraftMention[]): DraftMention[] {
  if (mentions.length === 0 || prev === next) return [...mentions];
  let head = 0;
  const limit = Math.min(prev.length, next.length);
  while (head < limit && prev.charCodeAt(head) === next.charCodeAt(head)) head += 1;
  let tail = 0;
  while (tail < limit - head && prev.charCodeAt(prev.length - 1 - tail) === next.charCodeAt(next.length - 1 - tail)) tail += 1;
  const changedEnd = prev.length - tail;
  const delta = next.length - prev.length;
  const out: DraftMention[] = [];
  for (const m of mentions) {
    if (m.end <= head) out.push(m);
    else if (m.start >= changedEnd) out.push({ ...m, start: m.start + delta, end: m.end + delta });
  }
  return out;
}

interface DraftEdit {
  text: string;
  caret: number;
  mentions: DraftMention[];
}

/** Insere a menção no lugar de `[from, to)` (o `@` e a consulta), com um espaço depois. */
export function insertMention(
  text: string,
  from: number,
  to: number,
  mentions: readonly DraftMention[],
  mention: Omit<DraftMention, "start" | "end">,
): DraftEdit {
  const label = mentionLabel(mention.title, mention.section);
  const after = text.slice(to);
  const spacer = after.startsWith(" ") ? "" : " ";
  const nextText = `${text.slice(0, from)}${label}${spacer}${after}`;
  const kept = reconcileMentions(text, `${text.slice(0, from)}${after}`, mentions).map((m) =>
    m.start >= from ? { ...m, start: m.start + label.length + spacer.length, end: m.end + label.length + spacer.length } : m,
  );
  const added: DraftMention = { ...mention, start: from, end: from + label.length };
  return { text: nextText, caret: from + label.length + 1, mentions: [...kept, added].sort((a, b) => a.start - b.start) };
}

/**
 * Backspace com o cursor no fim (ou dentro) de uma menção, ou Delete no começo dela: apaga a menção
 * inteira. Com seleção, ou fora de menção, `null` (o campo apaga como sempre).
 */
export function removeMentionAt(
  text: string,
  selStart: number,
  selEnd: number,
  key: "Backspace" | "Delete",
  mentions: readonly DraftMention[],
): DraftEdit | null {
  if (selStart !== selEnd) return null;
  const hit = mentions.find((m) => (key === "Backspace" ? selStart > m.start && selStart <= m.end : selStart >= m.start && selStart < m.end));
  if (hit === undefined) return null;
  const nextText = text.slice(0, hit.start) + text.slice(hit.end);
  return { text: nextText, caret: hit.start, mentions: reconcileMentions(text, nextText, mentions.filter((m) => m !== hit)) };
}

/** Cursor dentro de uma menção vai para a borda mais próxima (a etiqueta é um bloco só). */
export function snapCaret(caret: number, mentions: readonly DraftMention[]): number {
  const hit = mentions.find((m) => caret > m.start && caret < m.end);
  if (hit === undefined) return caret;
  return caret - hit.start < hit.end - caret ? hit.start : hit.end;
}

/**
 * A citação seguida do resto é lida como ela mesma? Decide pela leitura (a mesma do servidor): só
 * põe o espaço quando o que vem depois mudaria o documento, a âncora ou o tamanho lido.
 */
function readsAlone(cite: string, rest: string): boolean {
  const first = splitCitations(cite + rest)[0];
  return first?.kind === "cite" && first.cite.text === cite;
}

/**
 * `true` se a citação logo depois de `before` ainda é lida como citação: o parser
 * (`splitCitations`, como o servidor) recusa o `kb:` colado numa letra, número ou `_`
 * (observação da T06: "x" seguido da etiqueta viraria "xkb:<id>" e a menção se perderia).
 */
function readsAfter(before: string, cite: string): boolean {
  const last = before.slice(-1);
  const parts = splitCitations(last + cite);
  const tail = parts[parts.length - 1];
  return tail?.kind === "cite" && tail.cite.text === cite;
}

/**
 * O texto enviado: cada menção vira `kb:<id>` ou `kb:<id>#<âncora>`, com um espaço antes ou
 * depois só quando, sem ele, a citação deixaria de ser lida ou mudaria de leitura.
 */
export function encodeMentions(text: string, mentions: readonly DraftMention[]): string {
  let out = text;
  for (const m of [...mentions].sort((a, b) => b.start - a.start)) {
    const cite = m.anchor === null ? `kb:${m.doc}` : `kb:${m.doc}#${m.anchor}`;
    const before = out.slice(0, m.start);
    const rest = out.slice(m.end);
    out = `${before}${readsAfter(before, cite) ? "" : " "}${cite}${readsAlone(cite, rest) ? "" : " "}${rest}`;
  }
  return out;
}

/** Título do documento e da seção de uma citação, ou `null` (saiu da base: fica texto comum). */
type MentionResolver = (doc: string, anchor: string | null) => { title: string; section: string | null } | null;

/** Refaz as etiquetas de um texto codificado (o rascunho guardado). */
export function decodeMentions(text: string, resolve: MentionResolver): { text: string; mentions: DraftMention[] } {
  let out = "";
  const mentions: DraftMention[] = [];
  for (const part of splitCitations(text)) {
    if (part.kind === "text") {
      out += part.text;
      continue;
    }
    const found = resolve(part.cite.doc, part.cite.section);
    if (found === null) {
      out += part.cite.text;
      continue;
    }
    const label = mentionLabel(found.title, found.section);
    mentions.push({ start: out.length, end: out.length + label.length, doc: part.cite.doc, anchor: part.cite.section, title: found.title, section: found.section });
    out += label;
  }
  return { text: out, mentions };
}
