/*
 * Modelo puro da janela Conhecimento (spec 134 T06): subtítulo vivo, estado e tom de cada
 * documento, origem, indicadores e a leitura das citações `kb:` do plano.
 */
import type { KbDoc, KbIndicator, KbList, KbSection, KbStatus } from "../../api/kb";
import { t, tp } from "../../i18n";
import { translateCode } from "../../i18n/serverCodes";
import type { Tone } from "../../desktop/theme/tokens";
import type { ContainerSize } from "../../desktop/useContainerSize";
import { formatBytes } from "../files/filesModel";
import type { KbState } from "./kbStore";

/** Teto do envio de arquivo (`kb::MAX_UPLOAD_BYTES`), em MiB. */
export const KB_MAX_UPLOAD_MIB = 32;
/** Teto da descrição (`kb::MAX_DESCRIPTION_CHARS`). */
export const KB_MAX_DESCRIPTION_CHARS = 200;

/** Subtítulo vivo: "12 documentos · 2 indexando" (e os com problema, quando há). */
export function kbSubtitle(list: KbList): string {
  const docs = list.docs;
  const parts = [tp("kb.subtitle.docs", docs.length)];
  const indexing = docs.filter((d) => d.status === "pending" || d.status === "extracting").length;
  if (indexing > 0) parts.push(tp("kb.subtitle.indexing", indexing));
  const failed = docs.filter((d) => d.status === "error" || d.status === "no_text").length;
  if (failed > 0) parts.push(tp("kb.subtitle.failed", failed));
  return parts.join(" · ");
}

export function kbSummaryText(state: KbState | null): string | undefined {
  return state?.status === "ok" ? kbSubtitle(state.list) : undefined;
}

export const STATUS_TONE: Readonly<Record<KbStatus, Tone>> = {
  pending: "muted",
  extracting: "cyan",
  ready: "green",
  no_text: "amber",
  error: "rose",
};

export function statusText(status: KbStatus): string {
  return t(`kb.status.${status}`);
}

/** Motivo da extração no idioma ativo (`kb.reason.*`), ou `null`. */
export function reasonText(doc: KbDoc): string | null {
  const code = doc.status_reason;
  if (code === null || code === undefined || code === "") return null;
  return translateCode("kb.reason", code, undefined);
}

/** Origem curta: o nome do arquivo ou o endereço do link sem o esquema. */
function originText(doc: KbDoc): string {
  if (doc.source === "url" && doc.source_url !== null) {
    try {
      const url = new URL(doc.source_url);
      return `${url.host}${url.pathname === "/" ? "" : url.pathname}`;
    } catch {
      return doc.source_url;
    }
  }
  return doc.file_name ?? doc.id;
}

/** Linha de detalhe: origem · tamanho · páginas. */
export function detailParts(doc: KbDoc): string[] {
  const parts = [originText(doc)];
  if (doc.size > 0) parts.push(formatBytes(doc.size));
  if (doc.pages !== null && doc.pages > 0) parts.push(tp("kb.pages", doc.pages));
  return parts;
}

export function kindIcon(doc: KbDoc): "link" | "report" | "file" {
  if (doc.kind === "link") return "link";
  if (doc.kind === "pdf") return "report";
  return "file";
}

/** Indicadores para exibir: o "sem texto" do estado e os do servidor (código conhecido). */
export function indicatorTexts(doc: KbDoc, docs: readonly KbDoc[]): string[] {
  const out: string[] = [];
  if (doc.status === "no_text") out.push(t("kb.indicator.no_text"));
  for (const ind of doc.indicators ?? []) {
    const text = indicatorText(ind, docs);
    if (text !== null) out.push(text);
  }
  return out;
}

/** Texto do indicador; a duplicata ganha o título do outro documento. Código novo: `null`. */
export function indicatorText(ind: KbIndicator, docs: readonly KbDoc[]): string | null {
  const params: Record<string, string | number> = { ...(ind.params ?? {}) };
  if (ind.code === "duplicate") {
    const other = docs.find((d) => d.id === params.doc);
    params.title = other?.title ?? String(params.doc ?? "");
  }
  return translateCode("kb.indicator", ind.code, params);
}

/** Lista e prévia lado a lado, ou uma de cada vez (estreita). */
export function kbArrangement(size: ContainerSize): "stacked" | "split" {
  return size === "narrow" ? "stacked" : "split";
}

/** Uma citação `kb:<id>` ou `kb:<id>#<âncora>` encontrada num texto do plano. */
interface KbCitation {
  text: string;
  doc: string;
  section: string | null;
}

type CitedPart = { kind: "text"; text: string } | { kind: "cite"; cite: KbCitation };

/** Tamanhos do sufixo hexadecimal do id (`kb::ID_SUFFIX_LENS`). */
const ID_SUFFIX_LENS = [8, 16, 32];
const MAX_ID_CHARS = 64;

/** Id com a forma real da base (`kb::is_doc_id`): `<slug>-<hex de 8, 16 ou 32>`. */
export function isDocId(id: string): boolean {
  if (id.length === 0 || id.length > MAX_ID_CHARS || !/^[a-z0-9][a-z0-9-]*$/.test(id)) return false;
  const cut = id.lastIndexOf("-");
  if (cut <= 0) return false;
  const suffix = id.slice(cut + 1);
  return ID_SUFFIX_LENS.includes(suffix.length) && /^[0-9a-f]+$/.test(suffix);
}

const isWordChar = (c: string): boolean => /[\p{L}\p{N}_]/u.test(c);

/** Separa o texto do plano em trechos e citações. */
export function splitCitations(text: string): CitedPart[] {
  // A mesma leitura do servidor (`kb::cite::find`): só `kb:` seguido de um id real é citação.
  const parts: CitedPart[] = [];
  let last = 0;
  let from = 0;
  for (;;) {
    const at = text.indexOf("kb:", from);
    if (at < 0) break;
    from = at + 3;
    if (at > 0 && isWordChar(text.charAt(at - 1))) continue;
    const rest = text.slice(at + 3);
    const run = /^[a-z0-9-]*/.exec(rest)?.[0] ?? "";
    const doc = run.replace(/-+$/, "");
    if (!isDocId(doc)) continue;
    let length = 3 + doc.length;
    let section: string | null = null;
    if (doc.length === run.length && rest.charAt(run.length) === "#") {
      const anchor = (/^[A-Za-z0-9._-]*/.exec(rest.slice(run.length + 1))?.[0] ?? "").replace(/[.-]+$/, "");
      if (anchor !== "") {
        section = anchor;
        length += 1 + anchor.length;
      }
    }
    if (at > last) parts.push({ kind: "text", text: text.slice(last, at) });
    parts.push({ kind: "cite", cite: { text: text.slice(at, at + length), doc, section } });
    last = at + length;
    from = last;
  }
  if (last < text.length) parts.push({ kind: "text", text: text.slice(last) });
  return parts;
}

/**
 * Corta a nota em antes, a seção citada e depois, pela linha que o servidor devolve para cada
 * seção (`sections[].line`, no `content`): a seção vai até a próxima de nível igual ou maior.
 * Assim `#exemplo-2` destaca o segundo "Exemplo", e um título dentro de cerca de código, que o
 * servidor não conta como seção, não confunde. Sem a linha (ou a seção do topo), `null`.
 */
export function splitAtSection(content: string, sections: readonly KbSection[], anchor: string | null): [string, string, string] | null {
  if (anchor === null) return null;
  const i = sections.findIndex((s) => s.anchor === anchor);
  const target = sections[i];
  if (target === undefined || target.level < 1 || typeof target.line !== "number") return null;
  const lines = content.split("\n");
  if (target.line < 0 || target.line >= lines.length) return null;
  const next = sections.slice(i + 1).find((s) => s.level >= 1 && s.level <= target.level && typeof s.line === "number");
  const end = next?.line !== undefined && next.line > target.line ? Math.min(next.line, lines.length) : lines.length;
  return [lines.slice(0, target.line).join("\n"), lines.slice(target.line, end).join("\n"), lines.slice(end).join("\n")];
}
