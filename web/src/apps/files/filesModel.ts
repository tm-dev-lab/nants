/*
 * Regras puras da janela Arquivos (spec 093, T08): intervalos da atualização, tetos do realce, diff, tamanhos, divisão
 * árvore/visualizador e o resumo da janela.
 */
import type { ContainerSize } from "../../desktop/useContainerSize";
import { FILES_SPLIT_MAX, FILES_SPLIT_MIN } from "../../desktop/uiState";
import { formatNumber, t } from "../../i18n";
import type { AppSummary, AppSummaryInput } from "../registry";

/** Intervalo do polling leve e debounce das atualizações por evento (spec 072/T04). */
export const FILES_POLL_MS = 8000;
export const FILES_EVENT_DEBOUNCE_MS = 1500;

/** Acima disto o texto puro é mantido (editor pesado demais para o ganho). */
export const HIGHLIGHT_MAX_BYTES = 200 * 1024;
export const HIGHLIGHT_MAX_LINES = 5000;

/** Divisão padrão (fração da árvore) e o passo do teclado. */
export const DEFAULT_FILES_SPLIT = 0.34;
const FILES_SPLIT_STEP = 0.04;

type DiffLineKind = "head" | "hunk" | "add" | "del" | "ctx";

/** Sinal de adição do diff (por código: é dado do diff, não um ícone). */
const PLUS = String.fromCharCode(43);

/** Linha de diff unificado: tipo pela primeira coluna (cabeçalhos +++/--- não contam). */
export function diffLineKind(line: string): DiffLineKind {
  if (line.startsWith(PLUS.repeat(3)) || line.startsWith("---")) return "head";
  if (line.startsWith("@@")) return "hunk";
  if (line.startsWith(PLUS)) return "add";
  if (line.startsWith("-")) return "del";
  return "ctx";
}

/** Tamanho com até uma casa decimal, no formato de número do idioma ativo ("1,5 KB" / "1.5 KB"). */
export function formatBytes(size: number): string {
  const one = (value: number): string => formatNumber(value, { maximumFractionDigits: 1 });
  if (size < 1024) return `${formatNumber(size)} B`;
  if (size < 1024 * 1024) return `${one(size / 1024)} KB`;
  return `${one(size / (1024 * 1024))} MB`;
}

/** Linhas do texto; uma quebra final não vira linha vazia extra. */
export function splitLines(content: string): string[] {
  if (content === "") return [];
  const lines = content.split(/\r\n|\n/);
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/** Fração da árvore dentro dos limites. */
export function clampSplit(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_FILES_SPLIT;
  return Math.min(FILES_SPLIT_MAX, Math.max(FILES_SPLIT_MIN, value));
}

/** Fração a partir da posição do ponteiro no contêiner (início e tamanho no mesmo eixo). */
export function splitFromPointer(position: number, start: number, size: number): number {
  if (size <= 0) return DEFAULT_FILES_SPLIT;
  return clampSplit((position - start) / size);
}

/** Nova fração por tecla na divisória (setas em passos; Home/End vão aos limites), ou `null`. */
export function splitByKey(current: number, key: string): number | null {
  switch (key) {
    case "ArrowLeft":
    case "ArrowUp":
      return clampSplit(current - FILES_SPLIT_STEP);
    case "ArrowRight":
    case "ArrowDown":
      return clampSplit(current + FILES_SPLIT_STEP);
    case "Home":
      return FILES_SPLIT_MIN;
    case "End":
      return FILES_SPLIT_MAX;
    default:
      return null;
  }
}

/** Arranjo pelo tamanho do contêiner: estreita, árvore OU arquivo (com "Voltar"); senão, lado a lado. */
export function filesArrangement(size: ContainerSize): "stacked" | "split" {
  return size === "narrow" ? "stacked" : "split";
}

/** Tarefa em andamento da spec ativa (a fonte "Trabalho em andamento"), ou `null`. */
function runningTaskId(input: AppSummaryInput): string | null {
  const spec = input.space?.activeSpec ?? null;
  if (spec === null || input.space?.running !== true) return null;
  return spec.tasks.find((item) => item.status === "IN_PROGRESS")?.id ?? null;
}

export function filesSummary(input: AppSummaryInput): AppSummary {
  if (input.space === null) return {};
  const task = runningTaskId(input);
  return { subtitle: task === null ? t("files.subtitle.base") : t("files.subtitle.task", { task }) };
}
