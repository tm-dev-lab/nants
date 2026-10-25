/*
 * Operações longas em curso (spec 123 T02): a rodada de testes ou de checagem do runner e o comando
 * de um agente. A Atividade mostra o detalhe; o Quadro e a barra do topo mostram a FASE de forma
 * curta ("testes rodando · 7 min", "Testes · 7 min"), para a pessoa saber o que está acontecendo
 * sem abrir a Atividade.
 *
 * Tudo vem do registro ao vivo do store: o início (`commandStartedAt`) e o limite
 * (`commandLimitMs`) são os que o servidor mandou; a web não fixa limite nenhum.
 *
 * Desempenho: o Quadro e a barra assinam só a ASSINATURA das operações (texto estável que muda no
 * início e no fim de cada uma), nunca a lista de atividades; o relógio anda por um temporizador
 * local, só enquanto há operação.
 */
import { t } from "./i18n";
import type { ActivityEntry } from "./store";
import { elapsedText } from "./apps/activity/activityModel";

type LongOperationKind = "tests" | "check" | "command";

export interface LongOperation {
  kind: LongOperationKind;
  /** Tarefa da operação (`FINAL` na verificação final), quando há. */
  taskId: string | null;
  /** Início (ms, época Unix), do servidor. */
  startedAt: number;
  /** Limite de tempo (ms), do servidor; `null` sem limite conhecido. */
  limitMs: number | null;
}

/** O comando de um agente só vira "operação longa" depois deste tempo (os curtos não piscam). */
export const LONG_COMMAND_MS = 10_000;
/** A partir desta fração do limite, o aviso discreto de "perto do limite". */
const NEAR_LIMIT_RATIO = 0.8;
/** Passo do relógio das fases (o texto mostra segundos e minutos). */
export const OPERATION_TICK_MS = 5_000;

const CHECK_STAGES: ReadonlySet<string> = new Set(["check", "check_base"]);

function kindOf(a: Pick<ActivityEntry, "agent" | "stageCode">): LongOperationKind {
  if (a.agent !== "tests") return "command";
  return a.stageCode !== undefined && CHECK_STAGES.has(a.stageCode.code) ? "check" : "tests";
}

/** As operações em curso da lista (todo comando com início conhecido), em ordem estável. */
export function longOperationsOf(list: readonly ActivityEntry[] | undefined): LongOperation[] {
  const out: LongOperation[] = [];
  for (const a of list ?? []) {
    if (a.command === undefined || a.commandStartedAt === undefined) continue;
    out.push({ kind: kindOf(a), taskId: a.taskId ?? null, startedAt: a.commandStartedAt, limitMs: a.commandLimitMs ?? null });
  }
  return out.sort((x, y) => x.startedAt - y.startedAt || (x.taskId ?? "").localeCompare(y.taskId ?? "") || x.kind.localeCompare(y.kind));
}

/** Assinatura estável das operações (para o seletor do store: só muda no início e no fim). */
export function longOperationsKey(list: readonly ActivityEntry[] | undefined): string {
  const ops = longOperationsOf(list);
  return ops.length === 0 ? "" : JSON.stringify(ops);
}

/** Lê a assinatura de volta (texto inválido = nenhuma operação). */
export function parseLongOperations(key: string): LongOperation[] {
  if (key === "") return [];
  try {
    const raw: unknown = JSON.parse(key);
    return Array.isArray(raw) ? (raw as LongOperation[]) : [];
  } catch {
    return [];
  }
}

/** A rodada do runner sempre aparece; o comando de agente, só depois de [`LONG_COMMAND_MS`]. */
export function isLongNow(op: LongOperation, now: number): boolean {
  return op.kind !== "command" || now - op.startedAt >= LONG_COMMAND_MS;
}

/** `true` quando o tempo já passou de [`NEAR_LIMIT_RATIO`] do limite que o servidor mandou. */
export function nearLimit(op: Pick<LongOperation, "startedAt" | "limitMs">, now: number): boolean {
  return op.limitMs !== null && op.limitMs > 0 && now - op.startedAt >= op.limitMs * NEAR_LIMIT_RATIO;
}

/** Peso da fase: a rodada de testes e a checagem vencem o comando de um agente. */
function weight(op: LongOperation): number {
  return op.kind === "command" ? 0 : 1;
}

/** A operação que representa a execução agora (testes antes de comando; a mais antiga), ou `null`. */
export function currentOperation(ops: readonly LongOperation[], now: number, taskId?: string): LongOperation | null {
  let best: LongOperation | null = null;
  for (const op of ops) {
    if (taskId !== undefined && op.taskId !== taskId) continue;
    if (!isLongNow(op, now)) continue;
    if (best === null || weight(op) > weight(best) || (weight(op) === weight(best) && op.startedAt < best.startedAt)) best = op;
  }
  return best;
}

const BAR_KEYS = { tests: "run.op.tests", check: "run.op.check", command: "run.op.command" } as const;
const BOARD_KEYS = { tests: "board.phase.tests", check: "board.phase.check", command: "board.phase.command" } as const;

/** Fase curta da barra do topo: "Testes · 7 min". */
export function barOperationText(op: LongOperation, now: number): string {
  return t(BAR_KEYS[op.kind], { time: elapsedText(now - op.startedAt) });
}

/** Fase do cartão do Quadro: "testes rodando · 7 min". */
export function boardOperationText(op: LongOperation, now: number): string {
  return t(BOARD_KEYS[op.kind], { time: elapsedText(now - op.startedAt) });
}
