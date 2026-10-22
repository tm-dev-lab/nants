/*
 * Regras puras do Quadro: uma coluna por
 * estado da máquina de tarefas, na ordem TODO → … → DONE. Padrão: A fazer, Em desenvolvimento,
 * Em revisão e Concluídas; as opcionais (Ajustes pedidos, Aprovada, Integrando, Bloqueada) são
 * escolhidas pelo usuário e ficam no ui-state (`selection.boardColumns`), não num storage
 * próprio. O cartão de uma coluna oculta vai para a coluna do grupo, com o selo do estado real.
 * Bloqueada aparece sempre que há tarefa bloqueada; status ausente, com erro ou desconhecido vai
 * para "Indefinida". Também: o resumo da janela (subtítulo, selo e vazio).
 *
 * Spec 102: os rótulos saem do dicionário no idioma ativo (lidos na hora; quem memoriza colunas
 * inclui o idioma nas dependências).
 */
import type { TaskStatus, TaskView } from "../../api/types";
import { TASK_STATUS_LABEL } from "../../copy";
import type { Tone } from "../../desktop/theme/tokens";
import { localized, t, tp, type MessageKey } from "../../i18n";
import { parseReviewMessage, type ReviewMessage } from "../../reviewMessage";
import { belongsToSpec, type ChatMessage, type RecoveryEntry } from "../../store";
import type { AppSummary, AppSummaryInput } from "../registry";

const FLOW: readonly TaskStatus[] = ["TODO", "IN_PROGRESS", "IN_REVIEW", "CHANGES_REQUESTED", "APPROVED", "INTEGRATING", "DONE"];

const KNOWN: ReadonlySet<string> = new Set<string>([...FLOW, "BLOCKED"]);

/** Colunas que o usuário mostra ou oculta (na ordem do fluxo). */
export const OPTIONAL_COLUMNS: readonly TaskStatus[] = ["CHANGES_REQUESTED", "APPROVED", "INTEGRATING", "BLOCKED"];

/** Coluna visível do mesmo grupo para cada estado de coluna ocultável. */
const FALLBACK_COLUMN: Partial<Record<TaskStatus, TaskStatus>> = {
  CHANGES_REQUESTED: "IN_PROGRESS",
  APPROVED: "IN_REVIEW",
  INTEGRATING: "IN_REVIEW",
};

/** Rótulo da coluna quando difere do rótulo do estado. */
const COLUMN_LABEL_KEY: Partial<Record<TaskStatus, MessageKey>> = { DONE: "board.column.DONE" };

/** Tom de cada estado (tokens; nunca classe de paleta). */
export const STATUS_TONE: Readonly<Record<TaskStatus, Tone>> = {
  TODO: "muted",
  IN_PROGRESS: "cyan",
  IN_REVIEW: "amber",
  CHANGES_REQUESTED: "amber",
  APPROVED: "green",
  INTEGRATING: "green",
  DONE: "green",
  BLOCKED: "rose",
};

/** Quem está com a tarefa em cada estado ativo (avatar no cartão). */
export const CARD_OWNER: Readonly<Partial<Record<TaskStatus, string>>> = {
  IN_PROGRESS: "developer",
  IN_REVIEW: "reviewer",
  INTEGRATING: "architect",
};

/**
 * Rótulo curto do selo de veredito no cartão (094/T08, A-04): cabe numa linha no cartão estreito.
 * O rótulo completo ("Revisão aprovada") fica no `title` e, para o leitor de tela, no prefixo oculto.
 */
export const CARD_VERDICT_LABEL: Readonly<Record<"APPROVED" | "CHANGES_REQUESTED", string>> = localized({
  APPROVED: "board.cardVerdict.APPROVED",
  CHANGES_REQUESTED: "board.cardVerdict.CHANGES_REQUESTED",
});

export function statusLabel(status: TaskStatus): string {
  return TASK_STATUS_LABEL[status];
}

function columnLabel(status: TaskStatus): string {
  const key = COLUMN_LABEL_KEY[status];
  return key === undefined ? statusLabel(status) : t(key);
}

/** Colunas opcionais escolhidas (do ui-state); valores desconhecidos são ignorados. */
export function parseBoardColumns(value: readonly string[] | undefined): ReadonlySet<TaskStatus> {
  if (value === undefined) return new Set();
  return new Set(OPTIONAL_COLUMNS.filter((status) => value.includes(status)));
}

/** Liga ou desliga uma coluna opcional; o resultado vai para `selection.boardColumns`. */
export function toggleBoardColumn(current: ReadonlySet<TaskStatus>, status: TaskStatus): TaskStatus[] {
  const next = new Set(current);
  if (next.has(status)) next.delete(status);
  else next.add(status);
  return OPTIONAL_COLUMNS.filter((s) => next.has(s));
}

/** Coluna (estado) em que a tarefa aparece dado o conjunto de colunas opcionais visíveis. */
export function columnFor(status: TaskStatus, shown: ReadonlySet<TaskStatus>): TaskStatus {
  if (status === "BLOCKED") return "BLOCKED";
  if (!shown.has(status)) return FALLBACK_COLUMN[status] ?? status;
  return status;
}

export interface BoardColumn {
  key: string;
  label: string;
  tone: Tone;
  tasks: TaskView[];
  /** Coluna especial: Bloqueada (destaque) ou Indefinida (discreta). */
  kind: "flow" | "blocked" | "undefined";
}

export function buildColumns(tasks: readonly TaskView[], shown: ReadonlySet<TaskStatus>): BoardColumn[] {
  const byColumn = new Map<string, TaskView[]>();
  const undefinedTasks: TaskView[] = [];
  let hasBlocked = false;
  for (const task of tasks) {
    if (task.status === null || task.error !== undefined || !KNOWN.has(task.status)) {
      undefinedTasks.push(task);
      continue;
    }
    if (task.status === "BLOCKED") hasBlocked = true;
    const key = columnFor(task.status, shown);
    const list = byColumn.get(key) ?? [];
    list.push(task);
    byColumn.set(key, list);
  }
  const columns: BoardColumn[] = [];
  for (const status of FLOW) {
    if (OPTIONAL_COLUMNS.includes(status) && !shown.has(status)) continue;
    columns.push({ key: status, label: columnLabel(status), tone: STATUS_TONE[status], tasks: byColumn.get(status) ?? [], kind: "flow" });
  }
  if (hasBlocked || shown.has("BLOCKED")) {
    columns.push({ key: "BLOCKED", label: statusLabel("BLOCKED"), tone: "rose", tasks: byColumn.get("BLOCKED") ?? [], kind: "blocked" });
  }
  if (undefinedTasks.length > 0) {
    columns.push({ key: "UNDEFINED", label: t("board.column.undefined"), tone: "muted", tasks: undefinedTasks, kind: "undefined" });
  }
  return columns;
}

/** Coluna de cada cartão (id da tarefa → chave da coluna). */
export function columnIndex(columns: readonly BoardColumn[]): Map<string, string> {
  const map = new Map<string, string>();
  for (const column of columns) for (const task of column.tasks) map.set(task.id, column.key);
  return map;
}

/** Cartões que trocaram de coluna entre dois arranjos (nunca na carga inicial). */
export function movedCards(before: ReadonlyMap<string, string> | null, after: ReadonlyMap<string, string>): Set<string> {
  const moved = new Set<string>();
  if (before === null) return moved;
  for (const [id, column] of after) {
    const was = before.get(id);
    if (was !== undefined && was !== column) moved.add(id);
  }
  return moved;
}

const IN_FLIGHT: ReadonlySet<string> = new Set(["IN_PROGRESS", "IN_REVIEW", "CHANGES_REQUESTED", "APPROVED", "INTEGRATING", "BLOCKED"]);

/** Tarefa ativa: a em andamento; senão a primeira ainda não concluída. */
export function pickActiveTask(tasks: readonly TaskView[]): TaskView | null {
  return tasks.find((t) => t.status !== null && IN_FLIGHT.has(t.status)) ?? tasks.find((t) => t.status !== "DONE") ?? null;
}

/** Selo de recuperação do Arquiteto (sem caractere no lugar de ícone). */
export function recoveryLabel(entry: Pick<RecoveryEntry, "phase" | "cycle">): string {
  return t(entry.phase === "analyzing" ? "board.recovery.analyzing" : "board.recovery.applied", { cycle: entry.cycle });
}

/** Texto do link de ações: "Ações (N)". */
export function toolActionsLabel(count: number): string {
  return t("board.toolActions", { count });
}

/** Id da verificação final na trilha. */
export const FINAL_TASK_ID = "FINAL";

/**
 * Última mensagem do Revisor sobre a tarefa (na spec), com veredito ou matriz de critérios.
 * Devolve a própria mensagem do store (referência estável para o seletor).
 */
export function lastReviewMessage(messages: readonly ChatMessage[] | undefined, specId: string, taskId: string): ChatMessage | null {
  if (messages === undefined) return null;
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i];
    if (m === undefined || m.role !== "agent" || m.agent !== "reviewer" || m.taskId !== taskId || !belongsToSpec(m, specId)) continue;
    if (parseReviewMessage(m.text) !== null) return m;
  }
  return null;
}

/** Linha recolhida da última revisão: "Última revisão · T02 · Pediu ajustes" (sem veredito, sem o fim). */
export function lastReviewLine(taskId: string, verdict: ReviewMessage["verdict"]): string {
  const head = t("board.lastReview", { task: taskId });
  if (verdict === null) return head;
  return `${head} · ${t(verdict === "approved" ? "review.approved" : "review.changes")}`;
}

export function parsedReview(message: ChatMessage | null): ReviewMessage | null {
  return message === null ? null : parseReviewMessage(message.text);
}

/** Estado vazio do Quadro, no idioma ativo. */
export function boardEmptyMessage(): string {
  return t("windowEmpty.board");
}

/** Subtítulo "4 tarefas · 2 concluídas", selo das bloqueadas e o vazio. */
export function boardSummary({ space }: AppSummaryInput): AppSummary {
  if (space === null) return {};
  const tasks = space.activeSpec?.tasks ?? [];
  if (tasks.length === 0) return { subtitle: t("board.noTasks"), empty: { message: boardEmptyMessage() } };
  const done = tasks.filter((t) => t.status === "DONE").length;
  const blocked = tasks.filter((t) => t.status === "BLOCKED").length;
  const subtitle = `${tp("board.taskCount", tasks.length)} · ${tp("board.doneCount", done)}`;
  if (blocked > 0) return { subtitle, badge: { count: blocked, label: tp("board.blockedBadge", blocked) } };
  return { subtitle };
}
