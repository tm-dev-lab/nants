/*
 * Edição de markdown sobre texto puro (spec 136 T11): as ações da barra e dos atalhos, a
 * continuação de lista no Enter, o recuo das listas, o gatilho `[[` dos links e a arrumação ao
 * salvar. Funções puras, sem DOM: recebem o texto e a seleção e devolvem o texto e a seleção novos.
 * O campo (um `textarea`) aplica o resultado com `setRangeText`, o que preserva o desfazer do
 * navegador.
 */

/** Texto e seleção `[start, end)`. */
export interface Sel {
  text: string;
  start: number;
  end: number;
}

/** Uma troca de trecho: `[from, to)` vira `insert`, e a seleção final. */
export interface Edit {
  from: number;
  to: number;
  insert: string;
  selStart: number;
  selEnd: number;
}

type InlineMark = "bold" | "italic" | "code";
type BlockMark = "h1" | "h2" | "h3" | "bullet" | "numbered" | "quote";

const INLINE: Record<InlineMark, string> = { bold: "**", italic: "*", code: "`" };

function lineStartAt(text: string, pos: number): number {
  return text.lastIndexOf("\n", pos - 1) + 1;
}

function lineEndAt(text: string, pos: number): number {
  const at = text.indexOf("\n", pos);
  return at < 0 ? text.length : at;
}

/** Negrito, itálico ou código no trecho: tira se já tem, põe se não tem; sem seleção, o par vazio. */
export function toggleInline({ text, start, end }: Sel, mark: InlineMark): Edit {
  const m = INLINE[mark];
  const inner = text.slice(start, end);
  // Já marcado por fora (`**|texto|**`): tira as marcas.
  const before = text.slice(start - m.length, start);
  const after = text.slice(end, end + m.length);
  const outsideOther = mark === "italic" && text.slice(start - 2, start) === "**" && text.slice(start - 3, start) !== "***";
  if (before === m && after === m && !outsideOther) {
    return { from: start - m.length, to: end + m.length, insert: inner, selStart: start - m.length, selEnd: end - m.length };
  }
  // Já marcado por dentro (`|**texto**|`): tira as marcas.
  if (inner.length >= m.length * 2 && inner.startsWith(m) && inner.endsWith(m) && !(mark === "italic" && inner.startsWith("**"))) {
    const bare = inner.slice(m.length, inner.length - m.length);
    return { from: start, to: end, insert: bare, selStart: start, selEnd: start + bare.length };
  }
  // Espaços nas pontas ficam de fora das marcas (`** texto **` não é negrito).
  const lead = inner.length - inner.trimStart().length;
  const trail = inner.length - inner.trimEnd().length;
  const core = inner.slice(lead, inner.length - trail);
  const insert = `${inner.slice(0, lead)}${m}${core}${m}${inner.slice(inner.length - trail)}`;
  const coreStart = start + lead + m.length;
  return { from: start, to: end, insert, selStart: coreStart, selEnd: coreStart + core.length };
}

const BLOCK_PREFIX = /^(\s*)(#{1,6} |> ?|[-*+] \[[ xX]\] |[-*+] |\d{1,9}[.)] )?/;

function prefixFor(mark: BlockMark, index: number): string {
  switch (mark) {
    case "h1":
      return "# ";
    case "h2":
      return "## ";
    case "h3":
      return "### ";
    case "bullet":
      return "- ";
    case "numbered":
      return `${index + 1}. `;
    case "quote":
      return "> ";
  }
}

function hasMark(prefix: string, mark: BlockMark): boolean {
  switch (mark) {
    case "h1":
      return prefix === "# ";
    case "h2":
      return prefix === "## ";
    case "h3":
      return prefix === "### ";
    case "bullet":
      return /^[-*+] $/.test(prefix);
    case "numbered":
      return /^\d+[.)] $/.test(prefix);
    case "quote":
      return prefix.startsWith(">");
  }
}

/**
 * Título, lista, lista numerada ou citação nas linhas da seleção. Se todas já têm a marca, tira;
 * senão, troca o prefixo de bloco de cada linha pela marca (um título vira lista e vice-versa).
 */
export function toggleBlock({ text, start, end }: Sel, mark: BlockMark): Edit {
  const from = lineStartAt(text, start);
  const to = lineEndAt(text, end > start && text[end - 1] === "\n" ? end - 1 : end);
  const lines = text.slice(from, to).split("\n");
  const parsed = lines.map((line) => {
    const match = BLOCK_PREFIX.exec(line);
    const indent = match?.[1] ?? "";
    const prefix = match?.[2] ?? "";
    return { indent, prefix, body: line.slice(indent.length + prefix.length) };
  });
  const content = parsed.filter((l) => l.body.trim() !== "" || l.prefix !== "");
  const remove = content.length > 0 && content.every((l) => hasMark(l.prefix, mark));
  let n = 0;
  const out = parsed.map((l) => {
    if (l.body.trim() === "" && l.prefix === "" && lines.length > 1) return l.indent + l.body;
    if (remove) return l.indent + l.body;
    const prefix = prefixFor(mark, n);
    n += 1;
    return l.indent + prefix + l.body;
  });
  const insert = out.join("\n");
  // Uma linha só: o cursor acompanha o texto; várias: a seleção cobre o bloco.
  if (lines.length === 1) {
    const delta = insert.length - (to - from);
    const head = parsed[0];
    const oldPrefixEnd = from + (head?.indent.length ?? 0) + (head?.prefix.length ?? 0);
    const newPrefixEnd = oldPrefixEnd + delta;
    const shift = (p: number) => (p < oldPrefixEnd ? newPrefixEnd : p + delta);
    return { from, to, insert, selStart: shift(start), selEnd: shift(end) };
  }
  return { from, to, insert, selStart: from, selEnd: from + insert.length };
}

/** Botão "Título": a linha passa por título 1, 2 e 3 e volta ao texto comum. */
export function cycleHeading(sel: Sel): Edit {
  const { text, start } = sel;
  const line = text.slice(lineStartAt(text, start), lineEndAt(text, start));
  const level = /^\s*(#{1,6}) /.exec(line)?.[1]?.length ?? 0;
  const next: BlockMark | null = level === 0 ? "h1" : level === 1 ? "h2" : level === 2 ? "h3" : null;
  if (next !== null) return toggleBlock(sel, next);
  return toggleBlock(sel, level === 3 ? "h3" : "h1");
}

/** Código: no trecho de uma linha, código em linha; em várias linhas ou na linha vazia, um bloco. */
export function toggleCode(sel: Sel): Edit {
  const { text, start, end } = sel;
  const inner = text.slice(start, end);
  const lineEmpty = text.slice(lineStartAt(text, start), lineEndAt(text, end)).trim() === "";
  if (!inner.includes("\n") && !lineEmpty) return toggleInline(sel, "code");
  const from = lineStartAt(text, start);
  const to = lineEndAt(text, end);
  const body = text.slice(from, to);
  const fenced = /^```[^\n]*\n([\s\S]*)\n```$/.exec(body);
  if (fenced) {
    const bare = fenced[1] ?? "";
    return { from, to, insert: bare, selStart: from, selEnd: from + bare.length };
  }
  const insert = `\`\`\`\n${body}\n\`\`\``;
  const caret = from + 4;
  return { from, to, insert, selStart: caret, selEnd: caret + body.length };
}

const URL_LIKE = /^(https?:\/\/|www\.)\S+$/i;

/**
 * Link (Ctrl+K): com um endereço selecionado, `[](endereço)` com o cursor no texto; com outro
 * texto, `[texto]()` com o cursor no endereço; sem seleção, `[]()`.
 */
export function insertLink({ text, start, end }: Sel): Edit {
  const inner = text.slice(start, end);
  if (URL_LIKE.test(inner.trim())) {
    const insert = `[](${inner.trim()})`;
    return { from: start, to: end, insert, selStart: start + 1, selEnd: start + 1 };
  }
  const insert = `[${inner}]()`;
  const caret = start + inner.length + 3;
  return { from: start, to: end, insert, selStart: inner === "" ? start + 1 : caret, selEnd: inner === "" ? start + 1 : caret };
}

const LIST_ITEM = /^(\s*)(?:([-*+])( \[[ xX]\])?|(\d{1,9})([.)]))( +)(.*)$/;
const QUOTE_LINE = /^(\s*)(>+ ?)(.*)$/;

/**
 * Enter numa lista ou citação: continua com o próximo marcador (o número seguinte, a caixa de
 * tarefa desmarcada). No item vazio, encerra a lista (tira o marcador). `null` fora de lista ou
 * com seleção.
 */
export function continueList({ text, start, end }: Sel): Edit | null {
  if (start !== end) return null;
  const from = lineStartAt(text, start);
  const line = text.slice(from, lineEndAt(text, start));
  const head = text.slice(from, start);
  const item = LIST_ITEM.exec(line);
  if (item) {
    const [, indent = "", bullet, task, num, delim, space = " ", body = ""] = item;
    const prefixLen = line.length - body.length;
    if (head.length < prefixLen) return null; // cursor dentro do marcador: Enter comum
    if (body.trim() === "") {
      return { from, to: from + line.length, insert: "", selStart: from, selEnd: from };
    }
    const next = bullet !== undefined ? `${bullet}${task !== undefined ? " [ ]" : ""}` : `${Number(num) + 1}${delim ?? "."}`;
    const insert = `\n${indent}${next}${space}`;
    return { from: start, to: start, insert, selStart: start + insert.length, selEnd: start + insert.length };
  }
  const quote = QUOTE_LINE.exec(line);
  if (quote) {
    const [, indent = "", marks = "> ", body = ""] = quote;
    if (head.length < indent.length + marks.length) return null;
    if (body.trim() === "") return { from, to: from + line.length, insert: "", selStart: from, selEnd: from };
    const insert = `\n${indent}${marks}`;
    return { from: start, to: start, insert, selStart: start + insert.length, selEnd: start + insert.length };
  }
  return null;
}

/** Tab e Shift+Tab num item de lista: recua ou avança o item (dois espaços). `null` fora de lista. */
export function indentList({ text, start, end }: Sel, outdent: boolean): Edit | null {
  const from = lineStartAt(text, start);
  const to = lineEndAt(text, end);
  const lines = text.slice(from, to).split("\n");
  // Só com ao menos um item de lista: na linha em branco (ou no texto vazio) o Tab segue o foco.
  if (!lines.some((l) => LIST_ITEM.test(l)) || !lines.every((l) => LIST_ITEM.test(l) || l.trim() === "")) return null;
  let delta0 = 0;
  let total = 0;
  const out = lines.map((l, i) => {
    if (l.trim() === "") return l;
    let next = l;
    if (outdent) next = l.replace(/^ {1,2}|^\t/, "");
    else next = `  ${l}`;
    if (i === 0) delta0 = next.length - l.length;
    total += next.length - l.length;
    return next;
  });
  const insert = out.join("\n");
  return { from, to, insert, selStart: Math.max(from, start + delta0), selEnd: end + total };
}

/** Gatilho do link a outro documento: `[[` antes do cursor, na mesma linha, ainda sem `]]`. */
export function findLinkTrigger(text: string, caret: number): { start: number; query: string } | null {
  const from = lineStartAt(text, caret);
  const head = text.slice(from, caret);
  const at = head.lastIndexOf("[[");
  if (at < 0) return null;
  const query = head.slice(at + 2);
  if (query.includes("]") || query.length > 80) return null;
  return { start: from + at, query };
}

/** Alvo relativo de um documento da base: o nome do arquivo, com a âncora da seção. */
export function linkTarget(fileName: string, anchor: string | null): string {
  // Espaço, parênteses e afins quebrariam o `[rótulo](alvo)`: viram `%XX` (o servidor decodifica).
  const name = Array.from(fileName)
    .map((ch) => {
      if (!/[\s()<>[\]%#?]/.test(ch)) return ch;
      const code = ch.charCodeAt(0);
      return code < 128 ? `%${code.toString(16).toUpperCase().padStart(2, "0")}` : encodeURIComponent(ch);
    })
    .join("");
  return anchor === null ? name : `${name}#${anchor}`;
}

/** Troca o `[[consulta` pelo link markdown `[título](arquivo.md#âncora)`. */
export function insertDocLink(text: string, trigger: { start: number }, caret: number, label: string, target: string): Edit {
  const safe = label.replace(/[[\]]/g, "");
  const insert = `[${safe}](${target})`;
  const after = text.slice(caret, caret + 2) === "]]" ? 2 : 0;
  return { from: trigger.start, to: caret + after, insert, selStart: trigger.start + insert.length, selEnd: trigger.start + insert.length };
}

/**
 * Arrumação ao salvar, leve e previsível: tira os espaços do fim das linhas (a quebra forçada de
 * dois espaços fica), junta as linhas em branco repetidas, usa `-` nos marcadores de lista e
 * termina com uma quebra de linha. Nada muda dentro dos blocos de código.
 */
export function tidyMarkdown(text: string): string {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  let fence: string | null = null;
  for (let i = 0; i < lines.length; i += 1) {
    const raw = lines[i] ?? "";
    const opener = /^\s*(`{3,}|~{3,})/.exec(raw);
    if (fence !== null) {
      out.push(raw);
      if (opener && opener[1]?.startsWith(fence)) fence = null;
      continue;
    }
    if (opener) {
      fence = opener[1] ?? "```";
      out.push(raw.trimEnd());
      continue;
    }
    const next = lines[i + 1] ?? "";
    const hardBreak = /\S {2,}$/.test(raw) && next.trim() !== "";
    let line = hardBreak ? `${raw.trimEnd()}  ` : raw.trimEnd();
    line = line.replace(/^(\s*)[*+] (?!\*)/, "$1- ");
    if (line === "" && out.length > 0 && out[out.length - 1] === "") continue;
    out.push(line);
  }
  while (out.length > 0 && out[out.length - 1] === "") out.pop();
  while (out.length > 0 && out[0] === "") out.shift();
  return out.length === 0 ? "" : `${out.join("\n")}\n`;
}
