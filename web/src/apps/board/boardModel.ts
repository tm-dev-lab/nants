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
import { parseReviewMessage, reviewStateCounts, reviewStateLine, type ReviewMessage } from "../../reviewMessage";
import { belongsToSpec, type ChatMessage, type RecoveryEntry, type RunStart } from "../../store";
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

// --- Tarefa de execução pausada (spec 114 T04) ---

/**
 * Motivo curto da pausa: o provedor falhou, a pessoa parou, erro interno, interrupção sem aviso
 * (servidor reiniciado) ou, com a execução viva, um pedido esperando a decisão da pessoa.
 */
export type PauseReason = "provider" | "plan_limit" | "plan_not_eligible" | "plan_sign_in" | "stopped" | "failed" | "interrupted" | "decision";

/** Estados em que a tarefa está com um agente: parada a execução, ela fica pausada (não "em andamento"). */
const HALTABLE: ReadonlySet<string> = new Set(["IN_PROGRESS", "IN_REVIEW", "CHANGES_REQUESTED", "APPROVED", "INTEGRATING"]);

/** `code` das mensagens do sistema que encerram o run (`src/server/runs.rs`) e o motivo de cada um. */
const STOP_CODES: Readonly<Record<string, PauseReason>> = {
  "run.stopped": "stopped",
  "run.provider_failed": "provider",
  "run.provider_failed_detail": "provider",
  // Plano ChatGPT (spec 116 T03): cada pausa com o motivo próprio.
  "run.plan_limit": "plan_limit",
  "run.plan_not_eligible": "plan_not_eligible",
  "run.plan_needs_login": "plan_sign_in",
  "run.failed": "failed",
  "run.failed_cause": "failed",
};

/**
 * Motivo da parada ATUAL do run da spec (spec 114 T04): a última mensagem do sistema com `code` de
 * parada, desde que venha depois do início ou da retomada mais recente da execução. Esse marco é o
 * `since` (`runStarts` do store, de `project_status` executando ou da transição do estado para
 * "executando"), comparado pelo `seq` de chegada ao vivo (a hora pode empatar) e, nas linhas do
 * histórico, pela hora. Sem marco (página recarregada), vale qualquer mensagem de agente posterior
 * à parada, que só existe se a execução voltou. Sem parada depois do marco, "execução interrompida".
 */
export function runStopReason(messages: readonly ChatMessage[] | undefined, specId: string, since?: RunStart): PauseReason {
  const code = runStopCode(messages, specId, since);
  return code === null ? "interrupted" : (STOP_CODES[code] ?? "interrupted");
}

/**
 * `code` da parada ATUAL do run da spec (a mesma regra de `runStopReason`), ou `null` (execução
 * interrompida). A Conversa usa para o cartão próprio das pausas do plano ChatGPT (spec 116 T03).
 */
export function runStopCode(messages: readonly ChatMessage[] | undefined, specId: string, since?: RunStart): string | null {
  if (messages === undefined) return null;
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i];
    if (m === undefined || m.role !== "agent" || !belongsToSpec(m, specId)) continue;
    if (since !== undefined && (m.seq !== undefined ? m.seq <= since.seq : m.at < since.at)) return null;
    // Um agente falou depois: a execução foi retomada depois de qualquer parada mais antiga.
    if (m.agent !== "system") return null;
    if (m.code !== undefined && STOP_CODES[m.code] !== undefined) return m.code;
  }
  return null;
}

/**
 * Tarefas pausadas, sem status novo no disco: o estado vem da execução (`halted`: spec em
 * andamento sem run, a mesma regra da Conversa) e dos pedidos pendentes (`waiting`: tarefas com
 * permissão aguardando a pessoa). Só as que estão com um agente; BLOCKED tem a própria coluna.
 */
export function taskPauses(tasks: readonly TaskView[], halted: boolean, stopReason: PauseReason, waiting: ReadonlySet<string>): Map<string, PauseReason> {
  const out = new Map<string, PauseReason>();
  for (const task of tasks) {
    if (task.status === null || !HALTABLE.has(task.status)) continue;
    if (halted) out.set(task.id, stopReason);
    else if (waiting.has(task.id)) out.set(task.id, "decision");
  }
  return out;
}

/** Rótulo curto do motivo da pausa, no idioma ativo. */
const PAUSE_REASON_KEY: Readonly<Record<PauseReason, MessageKey>> = {
  provider: "board.pause.provider",
  plan_limit: "board.pause.planLimit",
  plan_not_eligible: "board.pause.planNotEligible",
  plan_sign_in: "board.pause.planNeedsLogin",
  stopped: "board.pause.stopped",
  failed: "board.pause.failed",
  interrupted: "board.pause.interrupted",
  decision: "board.pause.decision",
};

export function pauseReasonText(reason: PauseReason): string {
  return t(PAUSE_REASON_KEY[reason]);
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

/**
 * Linha recolhida da última revisão: "Última revisão · T02 · Pediu ajustes" (sem veredito, sem o fim),
 * seguida do resumo dos estados dos apontamentos (spec 124 T01), quando a mensagem os traz:
 * "· 1 novo · 2 ainda abertos · 1 resolvido".
 */
export function lastReviewLine(taskId: string, verdict: ReviewMessage["verdict"], review?: ReviewMessage): string {
  const head = t("board.lastReview", { task: taskId });
  const states = review === undefined ? "" : reviewStateLine(reviewStateCounts(review), tp);
  const withVerdict = verdict === null ? head : `${head} · ${t(verdict === "approved" ? "review.approved" : "review.changes")}`;
  return states === "" ? withVerdict : `${withVerdict} · ${states}`;
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
