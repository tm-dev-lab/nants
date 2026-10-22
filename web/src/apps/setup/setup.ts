/*
 * Etapas dos Primeiros passos (spec 093, decisão 15), derivadas do `/api/readiness`:
 * 1. Sua máquina (Git, Docker, Compose); 2. Conexão; 3. Agentes; 4. Primeiro projeto.
 * Funções puras.
 */
import type { PrereqItem } from "../../api/types";
import { requiredMissing, type Readiness, type ReadinessItem } from "../../desktop/api";
import { localized, t, tp } from "../../i18n";

export type SetupStep = "machine" | "connection" | "agents" | "project";

export const SETUP_STEPS: readonly SetupStep[] = ["machine", "connection", "agents", "project"];

export const SETUP_STEP_LABELS: Readonly<Record<SetupStep, string>> = localized({
  machine: "setup.step.machine",
  connection: "setup.step.connection",
  agents: "setup.step.agents",
  project: "setup.step.project",
});

const MACHINE_IDS: ReadonlySet<string> = new Set(["git", "docker_cli", "docker_daemon", "docker_permission", "docker_compose"]);

/** Pendências de cada etapa (a etapa de projeto não tem pendência da prontidão). */
export function stepMissing(readiness: Readiness, step: SetupStep): ReadinessItem[] {
  const all = requiredMissing(readiness);
  switch (step) {
    case "machine":
      return all.filter((i) => MACHINE_IDS.has(i.id) || i.settings_tab === "machine");
    case "connection":
      return all.filter((i) => i.id === "connection");
    case "agents":
      return all.filter((i) => i.id.startsWith("agent_"));
    case "project":
      return [];
  }
}

/** Etapa concluída: sem pendências (projeto: ao menos um projeto registrado). */
export function stepDone(readiness: Readiness, step: SetupStep, projectCount: number): boolean {
  if (step === "project") return projectCount > 0;
  return stepMissing(readiness, step).length === 0;
}

/** Primeira etapa pendente (todas concluídas: a última). */
export function firstPendingStep(readiness: Readiness | null, projectCount: number): SetupStep {
  if (readiness === null) return "machine";
  return SETUP_STEPS.find((step) => !stepDone(readiness, step, projectCount)) ?? "project";
}

/** Item do diagnóstico da máquina (`GET /api/system/prereqs`), com o marcador informativo da spec 103. */
export type DiagnosisItem = PrereqItem & { info?: boolean };

type DiagnosisStatus = "ok" | "fail" | "info";

/** Estado de uma linha do diagnóstico: o mesmo OK, FALHA e INFO do `nants doctor`. */
export function diagnosisStatus(item: DiagnosisItem): DiagnosisStatus {
  if (!item.ok) return "fail";
  return item.info === true ? "info" : "ok";
}

/** Texto para leitor de tela de cada estado. */
export const DIAGNOSIS_STATUS_LABELS: Readonly<Record<DiagnosisStatus, string>> = localized({
  ok: "setup.status.ok",
  fail: "setup.status.fail",
  info: "setup.status.info",
});

/** Subtítulo vivo da janela. */
export function setupSubtitle(readiness: Readiness | null, projectCount: number): string {
  if (readiness === null) return t("setup.subtitle.checking");
  const pending = SETUP_STEPS.filter((step) => !stepDone(readiness, step, projectCount)).length;
  if (pending === 0) return t("setup.subtitle.ready");
  return tp("setup.subtitle.pending", pending);
}
