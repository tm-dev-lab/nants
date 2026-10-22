/*
 * Derivações de fase e de spec ativa a partir do estado do projeto e do store.
 * Funções puras (exceto o hook do destaque de integração), usadas pela pílula de execução, pelo
 * seletor de espaços, pelos resumos das janelas e pelos layouts por fase.
 */
import { localized } from "./i18n";
import { useEffect, useState } from "react";
import type { PlanReviewView, ProjectState, SpecView } from "./api/types";
import type { ProjectPhase } from "./store";

/** Tempo em que a luz fica esmeralda após `integrated`, antes de voltar ao neutro. */
const INTEGRATED_FLASH_MS = 4_000;

export const PHASE_LABELS: Readonly<Record<ProjectPhase, string>> = localized({
  idle: "phase.idle",
  planning: "phase.planning",
  executing: "phase.executing",
  waiting_approval: "phase.waiting_approval",
  finalizing: "phase.finalizing",
  integrated: "phase.integrated",
  blocked: "phase.blocked",
  stalled: "phase.stalled",
  reviewing_plan: "phase.reviewing_plan",
});

/** Fases em que a pílula pulsa (há trabalho ou espera do usuário em curso). */
export const PULSING_PHASES: ReadonlySet<ProjectPhase> = new Set<ProjectPhase>([
  "planning",
  "reviewing_plan",
  "executing",
  "finalizing",
  "waiting_approval",
]);

/**
 * Etapa do plano do espaço (spec 094, decisão 3):
 * - `planning`: o turno do Arquiteto está aberto (o rascunho pode já estar gravado como
 *   AWAITING_APPROVAL); nada de "Plano pronto", abertura automática ou ações;
 * - `reviewing`: sem turno, com a revisão do plano `pending`/`running`; ações desabilitadas
 *   ("aguardando o Revisor");
 * - `awaiting`: sem turno e com a revisão concluída (`ok`, `stale`, `unavailable` ou sem
 *   revisão); só aqui aparecem o "Plano pronto", a abertura automática e as ações.
 */
export type PlanStage = "planning" | "reviewing" | "awaiting";

/** `true` enquanto a revisão da versão atual do plano não terminou (`pending`/`running`). */
export function reviewWaiting(review: PlanReviewView | undefined): boolean {
  return review?.status === "pending" || review?.status === "running";
}

/**
 * Derivação ÚNICA da etapa do plano no espaço ativo: pílula, subtítulos, cartão da Conversa,
 * `PlanCard`, abertura automática e lançador leem esta função, nunca o status da spec sozinho.
 * Com execução (`state.running`), sem estado ou sem spec em plano devolve `null`.
 * Paridade com o servidor (`plan_awaiting_approval` do `/api/attention`, que aplica `!running`,
 * `!turn_open` e a revisão fora de `pending`/`running`): `awaiting` vale se e somente se o
 * servidor conta o plano (fixture `src/server/testdata/plan_stage_parity.json`).
 * Rascunho (DRAFT) só conta como `planning` com o turno aberto; sem turno, `null` (como antes).
 */
export function planStage(
  state: Pick<ProjectState, "running"> | null,
  activeSpec: SpecView | null,
  architectTurnActive: boolean,
): PlanStage | null {
  if (state === null || state.running || activeSpec === null) return null;
  if (activeSpec.status === "DRAFT") return architectTurnActive ? "planning" : null;
  if (activeSpec.status !== "AWAITING_APPROVAL") return null;
  if (architectTurnActive) return "planning";
  if (reviewWaiting(activeSpec.plan_review)) return "reviewing";
  return "awaiting";
}

/** Fase exibida de cada etapa do plano. */
const STAGE_PHASE: Readonly<Record<PlanStage, ProjectPhase>> = {
  planning: "planning",
  reviewing: "reviewing_plan",
  awaiting: "waiting_approval",
};

/**
 * Spec ativa: a em execução; senão a aguardando aprovação; senão o rascunho; senão a última.
 * Ordem da lista do servidor preservada; a mais recente é a última.
 */
export function pickActiveSpec(specs: readonly SpecView[]): SpecView | null {
  const last = (status: SpecView["status"]): SpecView | null =>
    [...specs].reverse().find((spec) => spec.status === status) ?? null;
  return (
    last("IN_PROGRESS") ?? last("AWAITING_APPROVAL") ?? last("DRAFT") ?? specs[specs.length - 1] ?? null
  );
}

/**
 * Modo de execução: há spec rodando (runner ativo ou `IN_PROGRESS`) ou a spec ativa está
 * concluída. Rascunho novo (DRAFT/AWAITING_APPROVAL)
 * vira a spec ativa e leva ao planejamento.
 */
export function isExecutionMode(state: ProjectState, active: SpecView | null): boolean {
  return state.running || active?.status === "IN_PROGRESS" || active?.status === "COMPLETED";
}

/**
 * Execução parada: spec ativa IN_PROGRESS, sem runner/run, nenhuma tarefa BLOCKED (isso é
 * "execução pausada") e ainda há tarefa não DONE.
 */
export function isStalledRun(state: ProjectState, spec: SpecView | null): boolean {
  if (spec === null || spec.status !== "IN_PROGRESS") return false;
  if (state.running || state.run !== null) return false;
  if (spec.tasks.some((task) => task.status === "BLOCKED")) return false;
  return spec.tasks.some((task) => task.status !== "DONE");
}

/** Fase inicial quando ainda não chegou nenhum evento para o projeto. */
export function derivePhase(state: ProjectState | null): ProjectPhase {
  if (state === null) return "idle";
  if (state.running) return "executing";
  const active = pickActiveSpec(state.specs);
  if (active?.tasks.some((task) => task.status === "BLOCKED")) return "blocked";
  // Spec em execução (mesmo sem runner ativo) é "executando".
  if (active?.status === "IN_PROGRESS") return "executing";
  if (active?.status === "DRAFT" || active?.status === "AWAITING_APPROVAL") return "planning";
  // Spec ativa concluída (COMPLETED): mesma pílula "Concluído" de quando o evento chega.
  if (active?.status === "COMPLETED") return "integrated";
  return "idle";
}

/**
 * Fase exibida: a etapa do plano (`planStage`) decide entre "Planejando", "Revisando o plano" e
 * "Aguardando sua aprovação" quando a fase é ociosa/planejando (spec 094, decisão 3);
 * spec `IN_PROGRESS` nunca aparece como ociosa/planejando (alinhado a `isExecutionMode`).
 * A fase armazenada `waiting_approval` (pedido de permissão pendente) continua visível: é espera real.
 */
export function displayPhase(phase: ProjectPhase, active: SpecView | null, architectTurnActive = false, running = false): ProjectPhase {
  const stage = planStage({ running }, active, architectTurnActive);
  if (stage !== null && (phase === "planning" || phase === "idle")) return STAGE_PHASE[stage];
  if (active?.status === "IN_PROGRESS" && (phase === "idle" || phase === "planning")) {
    return "executing";
  }
  return phase;
}

/** Spec ativa concluída cujo relatório final do Arquiteto ainda está sendo escrito. */
function isReportFinalizing(state: ProjectState | null): boolean {
  if (state === null || state.report_pending !== true) return false;
  return pickActiveSpec(state.specs)?.status === "COMPLETED";
}

/**
 * Fase exibida de um projeto: ÚNICA fonte para a pílula, o seletor de espaços e o layout. A fase armazenada (evento
 * SSE) prevalece quando existe; senão deriva do estado carregado. Sem estado carregado e sem
 * evento → "idle" (projeto não aberto não dispara GET de estado). `architectTurnActive` vem de
 * `architectTurns` do store (SSE e `architect_turn` do estado): enquanto ativo, o plano não aparece como aguardando aprovação.
 */
export function phaseForProject(
  stored: ProjectPhase | undefined,
  state: ProjectState | null,
  architectTurnActive = false,
): ProjectPhase {
  if (state === null) return stored ?? "idle";
  if (isReportFinalizing(state)) return "finalizing";
  if (isStalledRun(state, pickActiveSpec(state.specs))) return "stalled";
  return displayPhase(stored ?? derivePhase(state), pickActiveSpec(state.specs), architectTurnActive, state.running);
}

/**
 * Destaque de integração da pílula de execução: durante `INTEGRATED_FLASH_MS` depois de a fase
 * virar "integrated", devolve "integrated" (a pílula ganha `data-flash`); depois, "idle". O rótulo
 * continua "Concluído". Ao abrir um espaço já integrado, o destaque também aparece uma vez.
 */
export function useIntegratedFlash(phase: ProjectPhase): ProjectPhase {
  const [flashing, setFlashing] = useState(false);
  useEffect(() => {
    if (phase !== "integrated") return;
    setFlashing(true);
    const timer = window.setTimeout(() => setFlashing(false), INTEGRATED_FLASH_MS);
    return () => {
      window.clearTimeout(timer);
      setFlashing(false);
    };
  }, [phase]);
  return phase === "integrated" && !flashing ? "idle" : phase;
}
