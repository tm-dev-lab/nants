/*
 * Analisador de Markdown restrito para o relatório do Arquiteto (texto de agente, não confiável).
 * Só produz árvore de dados: a renderização (`apps/shared/ReportMarkdown.tsx`) emite texto React,
 * nunca HTML. Suportado: títulos (# a ###, mais fundos viram ###), parágrafos, listas ordenadas e
 * não ordenadas aninhadas (numeração escrita preservada), blocos de código ``` (também dentro de
 * item de lista, na ordem do Markdown), código inline, negrito e ênfase. Fora do conjunto, o texto
 * segue literal: HTML bruto, imagens (viram o texto alternativo), links (viram só o rótulo; URL
 * http(s) aparece entre parênteses, sem ser clicável).
 *
 * Tempo linear por construção (não por instância de regex):
 * - o texto é normalizado antes (quebras Unicode viram "\n"; controles saem);
 * - linha de prosa é limitada a `MAX_PROSE_LINE_CHARS` ANTES de qualquer análise; linhas de cerca
 *   não passam por regex de prosa;
 * - cabeçalho, item e título são lidos por laços manuais (sem `$`, `.*` ou `+$` sobre texto do agente);
 * - inline só roda sobre até `MAX_INLINE_CHARS`; acima disso o texto vira texto simples.
 *
 * O texto vem como foi escrito (não se traduz); só a reserva da imagem sem descrição sai do
 * dicionário, e o título da seção de execução é reconhecido nos dois idiomas.
 */
import { t } from "./i18n";

/** Profundidade máxima de listas aninhadas; além disso, o texto do item é achatado no pai. */
export const MAX_LIST_DEPTH = 6;

/** Texto acima deste tamanho (por parágrafo, item ou título) não recebe marcação inline. */
const MAX_INLINE_CHARS = 8 * 1024;

/** Comprimento máximo de uma linha de prosa (excedente vira "…"). */
const MAX_PROSE_LINE_CHARS = 1024;

/** Título candidato a "Como executar": até este número de caracteres e de palavras. */
const MAX_TITLE_CHARS = 80;
const MAX_TITLE_WORDS = 6;

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "strong"; text: string }
  | { kind: "em"; text: string }
  | { kind: "code"; text: string }
  /** Quebra de linha visual (só com `keepLineBreaks`, mensagens de agentes). */
  | { kind: "break"; text: string };

interface ParagraphBlock {
  kind: "paragraph";
  inlines: Inline[];
}

interface HeadingBlock {
  kind: "heading";
  level: 1 | 2 | 3;
  inlines: Inline[];
}

interface CodeBlock {
  kind: "code";
  lang: string;
  text: string;
}

/** Parte de um item de lista, na ordem em que aparece no Markdown. */
type ItemPart =
  | { kind: "inlines"; inlines: Inline[] }
  | { kind: "code"; block: CodeBlock }
  | { kind: "list"; list: ListBlock };

export interface ListItem {
  /** Número escrito no Markdown (listas ordenadas); 0 nas não ordenadas. */
  number: number;
  parts: ItemPart[];
}

export interface ListBlock {
  kind: "list";
  ordered: boolean;
  items: ListItem[];
}

export type Block = ParagraphBlock | HeadingBlock | CodeBlock | ListBlock;

/** Normaliza quebras de linha Unicode e remove controles (exceto tab e quebra de linha). */
function sanitizeReportText(input: string): string {
  return input
    .replace(/\r\n?|[\u2028\u2029\u0085]/g, "\n")
    .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "");
}

/** Inline, por alternância (ordem importa). Rótulos e URLs não atravessam `[`/`]`. */
const INLINE_SOURCE = [
  "`([^`\\n]+)`",
  "\\*\\*(?=\\S)([^*\\n]+?)(?<=\\S)\\*\\*",
  "(?<![\\p{L}\\p{N}_])__(?=\\S)([^_\\n]+?)(?<=\\S)__(?![\\p{L}\\p{N}_])",
  "\\*(?=\\S)([^*\\n]+?)(?<=\\S)\\*",
  "(?<![\\p{L}\\p{N}_])_(?=\\S)([^_\\n]+?)(?<=\\S)_(?![\\p{L}\\p{N}_])",
  "!\\[([^\\[\\]\\n]*)\\]\\(([^()\\[\\]\\s]*(?:\\([^()\\[\\]\\s]*\\)[^()\\[\\]\\s]*)*)\\)",
  "\\[([^\\[\\]\\n]+)\\]\\(([^()\\[\\]\\s]*(?:\\([^()\\[\\]\\s]*\\)[^()\\[\\]\\s]*)*)\\)",
].join("|");

const HTTP_URL = /^https?:\/\//i;

/** Texto inline do Markdown restrito. Nunca interpreta HTML. */
function parseInline(source: string): Inline[] {
  if (source === "") return [];
  if (source.length > MAX_INLINE_CHARS) return [{ kind: "text", text: source }];
  const out: Inline[] = [];
  const re = new RegExp(INLINE_SOURCE, "gu");
  let last = 0;
  let match: RegExpExecArray | null;
  while ((match = re.exec(source)) !== null) {
    if (match.index > last) out.push({ kind: "text", text: source.slice(last, match.index) });
    last = match.index + match[0].length;
    const [, code, strongA, strongB, emA, emB, imageAlt, imageUrl, linkLabel, linkUrl] = match;
    if (code !== undefined) {
      out.push({ kind: "code", text: code });
    } else if (strongA !== undefined || strongB !== undefined) {
      out.push({ kind: "strong", text: strongA ?? strongB ?? "" });
    } else if (emA !== undefined || emB !== undefined) {
      out.push({ kind: "em", text: emA ?? emB ?? "" });
    } else if (imageAlt !== undefined) {
      out.push({ kind: "text", text: imageAlt.trim() === "" ? t("report.imageAlt") : imageAlt });
    } else if (linkLabel !== undefined) {
      const url = linkUrl ?? imageUrl ?? "";
      const showUrl = HTTP_URL.test(url) && url !== linkLabel;
      out.push({ kind: "text", text: showUrl ? `${linkLabel} (${url})` : linkLabel });
    }
  }
  if (last < source.length) out.push({ kind: "text", text: source.slice(last) });
  return out;
}

function plainText(inlines: readonly Inline[]): string {
  return inlines.map((inline) => inline.text).join("");
}

function isSpace(ch: string | undefined): boolean {
  return ch === " " || ch === "\t";
}

function isDigit(ch: string | undefined): boolean {
  return ch !== undefined && ch >= "0" && ch <= "9";
}

function isBlank(line: string): boolean {
  return line.trim() === "";
}

/** Largura da indentação em colunas (tab = 4) e posição do primeiro caractere que não é espaço. */
function indentInfo(line: string): { width: number; pos: number } {
  let width = 0;
  let pos = 0;
  while (pos < line.length) {
    const ch = line[pos];
    if (ch === " ") width += 1;
    else if (ch === "\t") width += 4;
    else break;
    pos += 1;
  }
  return { width, pos };
}

function indentOf(line: string): number {
  return indentInfo(line).width;
}

/** Remove até `width` colunas de indentação do início da linha. */
function stripIndent(line: string, width: number): string {
  let cut = 0;
  let col = 0;
  while (cut < line.length && col < width && isSpace(line[cut])) {
    col += line[cut] === "\t" ? 4 : 1;
    cut += 1;
  }
  return line.slice(cut);
}

/** Linha de prosa limitada; o excedente vira "…" (aplicado antes de qualquer análise). */
function prose(line: string): string {
  return line.length > MAX_PROSE_LINE_CHARS ? `${line.slice(0, MAX_PROSE_LINE_CHARS - 1)}…` : line;
}

interface HeadingLine {
  level: 1 | 2 | 3;
  text: string;
}

/** Cabeçalho: até 3 espaços, de 1 a 6 `#` seguidos de espaço (ou fim da linha). */
function headingOf(line: string): HeadingLine | null {
  const { width, pos } = indentInfo(line);
  if (width > 3) return null;
  let end = pos;
  while (end < line.length && line[end] === "#") end += 1;
  const count = end - pos;
  if (count < 1 || count > 6) return null;
  if (end < line.length && !isSpace(line[end])) return null;
  return { level: Math.min(count, 3) as 1 | 2 | 3, text: line.slice(end).trim() };
}

interface ItemLine {
  indent: number;
  ordered: boolean;
  number: number;
  text: string;
}

/** Item de lista: marcador (`-`, `*`, `+`, ou número seguido de `.`/`)`) e pelo menos um espaço. */
function itemOf(line: string): ItemLine | null {
  const { width, pos } = indentInfo(line);
  const ch = line[pos];
  if (ch === "-" || ch === "*" || ch === "+") {
    if (!isSpace(line[pos + 1])) return null;
    return { indent: width, ordered: false, number: 0, text: line.slice(pos + 1).trim() };
  }
  let end = pos;
  while (end < line.length && isDigit(line[end])) end += 1;
  const digits = end - pos;
  if (digits < 1 || digits > 9) return null;
  const mark = line[end];
  if ((mark !== "." && mark !== ")") || !isSpace(line[end + 1])) return null;
  return {
    indent: width,
    ordered: true,
    number: Number.parseInt(line.slice(pos, end), 10),
    text: line.slice(end + 1).trim(),
  };
}

/** Abertura de cerca: linguagem (ou "") quando a indentação é no máximo `maxIndent`; senão `null`. */
function fenceOpenOf(line: string, maxIndent: number): string | null {
  const { width, pos } = indentInfo(line);
  if (width > maxIndent) return null;
  if (!line.startsWith("```", pos)) return null;
  return line.slice(pos + 3).trim().split(/\s+/)[0] ?? "";
}

function isFenceClose(line: string): boolean {
  return line.trim() === "```";
}

/** Parte de item antes do render: texto acumulado, cerca ou sublista. */
type RawPart =
  | { kind: "text"; text: string }
  | { kind: "code"; block: CodeBlock }
  | { kind: "list"; list: ListBlock };

interface RawItem {
  number: number;
  parts: RawPart[];
}

function appendText(item: RawItem, text: string): void {
  if (text === "") return;
  const last = item.parts[item.parts.length - 1];
  if (last?.kind === "text") last.text = `${last.text} ${text}`;
  else item.parts.push({ kind: "text", text });
}

/**
 * Cerca dentro de item: começa em `start` (indentada além do item). Vai até a cerca de fechamento
 * ou, sem ela, até a primeira linha não vazia indentada como o item ou menos.
 */
function readItemFence(
  lines: readonly string[],
  start: number,
  baseIndent: number,
  lang: string,
): { block: CodeBlock; next: number } {
  const fenceIndent = indentOf(lines[start] ?? "");
  const body: string[] = [];
  let j = start + 1;
  let closed = false;
  while (j < lines.length) {
    const cur = lines[j] ?? "";
    if (isFenceClose(cur)) {
      closed = true;
      break;
    }
    if (!isBlank(cur) && indentOf(cur) <= baseIndent) break;
    body.push(stripIndent(cur, fenceIndent));
    j += 1;
  }
  while (body.length > 0 && isBlank(body[body.length - 1] ?? "")) body.pop();
  return { block: { kind: "code", lang, text: body.join("\n") }, next: closed ? j + 1 : j };
}

/** Lista que começa em `start` (linha de item). Devolve a lista e a próxima linha não consumida. */
function parseList(lines: readonly string[], start: number, depth: number): { list: ListBlock; next: number } {
  const first = itemOf(prose(lines[start] ?? ""));
  const baseIndent = first?.indent ?? 0;
  const ordered = first?.ordered ?? false;
  const raw: RawItem[] = [];
  let i = start;

  while (i < lines.length) {
    const line = prose(lines[i] ?? "");
    if (isBlank(line)) {
      // Linha em branco só fecha a lista se o próximo conteúdo não pertencer a ela.
      let probe = i + 1;
      while (probe < lines.length && isBlank(lines[probe] ?? "")) probe += 1;
      if (probe >= lines.length) break;
      const next = prose(lines[probe] ?? "");
      const nextIndent = indentOf(next);
      const nextItem = itemOf(next);
      const continues =
        nextIndent > baseIndent || (nextIndent === baseIndent && nextItem !== null && nextItem.ordered === ordered);
      if (!continues) break;
      i = probe;
      continue;
    }

    const item = itemOf(line);
    const indent = indentOf(line);
    if (item !== null) {
      if (indent < baseIndent) break;
      if (indent === baseIndent) {
        if (item.ordered !== ordered) break;
        const entry: RawItem = { number: item.number, parts: [] };
        appendText(entry, item.text);
        raw.push(entry);
        i += 1;
        continue;
      }
      const parent = raw[raw.length - 1];
      if (parent === undefined) break;
      if (depth >= MAX_LIST_DEPTH) {
        // Profundidade máxima: o texto do item vira continuação do pai (sem mais aninhamento).
        appendText(parent, item.text);
        i += 1;
        continue;
      }
      const nested = parseList(lines, i, depth + 1);
      parent.parts.push({ kind: "list", list: nested.list });
      i = nested.next;
      continue;
    }

    const parent = raw[raw.length - 1];
    if (indent > baseIndent && parent !== undefined) {
      // Cerca sob o item vira parte do item, na posição em que aparece.
      const lang = fenceOpenOf(line, Number.POSITIVE_INFINITY);
      if (lang !== null) {
        const fenced = readItemFence(lines, i, baseIndent, lang);
        parent.parts.push({ kind: "code", block: fenced.block });
        i = fenced.next;
        continue;
      }
      // Linha indentada além do marcador continua o texto do item.
      appendText(parent, line.trim());
      i += 1;
      continue;
    }
    break;
  }

  const items: ListItem[] = raw.map((entry) => ({
    number: entry.number,
    parts: entry.parts.map((part): ItemPart => {
      if (part.kind === "text") return { kind: "inlines", inlines: parseInline(part.text) };
      return part;
    }),
  }));
  return { list: { kind: "list", ordered, items }, next: i };
}

function startsBlock(line: string): boolean {
  return fenceOpenOf(line, 3) !== null || headingOf(line) !== null || itemOf(line) !== null;
}

/** Converte o Markdown restrito em blocos. Entrada vazia devolve `[]`. */
export function parseReportMarkdown(markdown: string, options: { keepLineBreaks?: boolean } = {}): Block[] {
  const lines = sanitizeReportText(markdown).split("\n");
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const raw = lines[i] ?? "";
    if (isBlank(raw)) {
      i += 1;
      continue;
    }

    const lang = fenceOpenOf(raw, 3);
    if (lang !== null) {
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !isFenceClose(lines[i] ?? "")) {
        body.push(lines[i] ?? "");
        i += 1;
      }
      i += 1; // pula a cerca de fechamento (ou passa do fim, sem fechamento)
      blocks.push({ kind: "code", lang, text: body.join("\n") });
      continue;
    }

    const line = prose(raw);
    const heading = headingOf(line);
    if (heading !== null) {
      blocks.push({ kind: "heading", level: heading.level, inlines: parseInline(heading.text) });
      i += 1;
      continue;
    }

    if (itemOf(line) !== null) {
      const parsed = parseList(lines, i, 1);
      blocks.push(parsed.list);
      i = parsed.next;
      continue;
    }

    const parts: string[] = [];
    while (i < lines.length && !isBlank(lines[i] ?? "") && !startsBlock(prose(lines[i] ?? ""))) {
      parts.push(prose(lines[i] ?? "").trim());
      i += 1;
    }
    if (options.keepLineBreaks === true) {
      // Mensagens de agentes: cada linha vira inline próprio, separado por quebra visual (continua linear).
      const inlines: Inline[] = [];
      parts.forEach((part, index) => {
        if (index > 0) inlines.push({ kind: "break", text: "\n" });
        inlines.push(...parseInline(part));
      });
      blocks.push({ kind: "paragraph", inlines });
    } else {
      blocks.push({ kind: "paragraph", inlines: parseInline(parts.join(" ")) });
    }
  }
  return blocks;
}

/** Texto normalizado para comparar títulos: sem marcação, numeração, pontuação final e caixa. */
function normalizeTitle(raw: string): string {
  let s = raw.toLowerCase().replace(/[*_`]/g, "").trim();
  let k = 0;
  while (k < s.length && isDigit(s[k])) k += 1;
  if (k > 0 && (s[k] === "." || s[k] === ")")) s = s.slice(k + 1);
  let end = s.length;
  while (end > 0 && (isSpace(s[end - 1]) || s[end - 1] === ":" || s[end - 1] === "." || s[end - 1] === "!")) end -= 1;
  return s.slice(0, end).split(/\s+/).filter((word) => word !== "").join(" ");
}

type TitleKind = "heading" | "bold" | "colon";

/**
 * Título de seção: cabeçalho; parágrafo que é só negrito; ou parágrafo com até 6 palavras que
 * termina em ":". Acima de 80 caracteres não é título.
 */
function titleOf(block: Block): { kind: TitleKind; norm: string; level: 1 | 2 | 3 | null } | null {
  if (block.kind !== "heading" && block.kind !== "paragraph") return null;
  const raw = plainText(block.inlines);
  if (raw.length > MAX_TITLE_CHARS) return null;
  if (block.kind === "heading") return { kind: "heading", norm: normalizeTitle(raw), level: block.level };
  if (block.kind === "paragraph" && block.inlines.length === 1 && block.inlines[0]?.kind === "strong") {
    return { kind: "bold", norm: normalizeTitle(raw), level: null };
  }
  const trimmed = raw.trim();
  const words = trimmed.split(/\s+/).filter((word) => word !== "").length;
  if (trimmed.endsWith(":") && words <= MAX_TITLE_WORDS) {
    return { kind: "colon", norm: normalizeTitle(raw), level: null };
  }
  return null;
}

/** Verbos de execução depois de "como" (pt-BR). */
const HOW_TO_VERBS_PT: readonly string[] = ["executar", "rodar", "iniciar", "subir"];
/** Verbos de execução depois de "how to" (en): o relatório em inglês sai com "How to run". */
const HOW_TO_VERBS_EN: readonly string[] = ["run", "start", "launch", "execute"];

/**
 * Título da seção de execução, nos dois idiomas (o relatório segue o idioma de quando foi
 * gerado): "como executar|rodar|iniciar|subir" ou "how to run|start|launch|execute" como
 * primeiras palavras.
 */
function isHowToTitle(norm: string): boolean {
  const words = norm.split(" ");
  if (words[0] === "como") return HOW_TO_VERBS_PT.includes(words[1] ?? "");
  return words[0] === "how" && words[1] === "to" && HOW_TO_VERBS_EN.includes(words[2] ?? "");
}

/** Títulos de outras seções conhecidas, nos dois idiomas: encerram a seção de execução. */
const OTHER_SECTION_PREFIXES: readonly string[] = [
  "como testar",
  "como parar",
  "como usar",
  "como instalar",
  "pré-requisitos",
  "pre-requisitos",
  "pré requisitos",
  "requisitos",
  "instalação",
  "instalacao",
  "instalar",
  "decisões",
  "decisoes",
  "limitações",
  "limitacoes",
  "próximos passos",
  "proximos passos",
  "how to test",
  "how to stop",
  "how to use",
  "how to install",
  "prerequisites",
  "pre-requisites",
  "requirements",
  "installation",
  "install",
  "decisions",
  "limitations",
  "known limitations",
  "next steps",
];

function isOtherSectionTitle(norm: string): boolean {
  return OTHER_SECTION_PREFIXES.some((prefix) => norm === prefix || norm.startsWith(`${prefix} `));
}

/**
 * Separa a seção "Como executar" (título de execução, em cabeçalho, negrito ou parágrafo com ":")
 * e tudo até o próximo cabeçalho de nível igual ou superior (negrito/":" conta como nível 3) ou até
 * o título de outra seção conhecida (testar, parar, usar, pré-requisitos, instalação...).
 * `howTo` é `null` quando não há a seção; nesse caso `rest` é o próprio relatório.
 */
export function findHowToSection(blocks: readonly Block[]): { howTo: Block[] | null; rest: Block[] } {
  let start = -1;
  for (let index = 0; index < blocks.length; index += 1) {
    const block = blocks[index];
    const title = block === undefined ? null : titleOf(block);
    if (title !== null && isHowToTitle(title.norm)) {
      start = index;
      break;
    }
  }
  if (start === -1) return { howTo: null, rest: [...blocks] };
  const head = blocks[start];
  const startLevel = head?.kind === "heading" ? head.level : 3;
  let end = start + 1;
  while (end < blocks.length) {
    const block = blocks[end];
    if (block === undefined) break;
    if (block.kind === "heading" && block.level <= startLevel) break;
    const title = titleOf(block);
    if (title !== null && isOtherSectionTitle(title.norm)) break;
    end += 1;
  }
  return {
    howTo: blocks.slice(start, end),
    rest: [...blocks.slice(0, start), ...blocks.slice(end)],
  };
}
