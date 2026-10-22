/*
 * Pílula de execução da barra superior (spec 093, decisão 8): fase (com destaque ao integrar), título da spec, tarefa, tempo e custo; "Parar" com
 * confirmação; "Retomar" (execução pausada) ou "Continuar" (execução parada).
 */
import { useEffect, useId, useRef, useState } from "react";
import { getUsage, resumeSpec, stopSpec, type ApiFailure } from "../../api/client";
import type { SpecView, TaskView } from "../../api/types";
import { APPROVE_FALLBACK_MESSAGE, APPROVE_REASON_MESSAGES } from "../../copy";
import { PHASE_LABELS, PULSING_PHASES, useIntegratedFlash } from "../../phase";
import { useAppStore } from "../../store";
import { formatActive, formatUsd } from "../../usageFormat";
import { gateBlocked, gateLevel, isPrereqFailure, prereqItems, prereqMessage, readinessItemLabel } from "../../apps/shared/readiness";
import { t } from "../../i18n";
import { useT } from "../../i18n/react";
import { Dialog } from "../Dialog";
import { Icon } from "../Icon";
import { PHASE_TONE } from "../theme/tokens";
import { useShell } from "../shellContext";
import type { SpaceData } from "../useSpaceData";
import { BarAlert } from "./BarAlert";

/** Estados de tarefa "em andamento" (a tarefa mostrada na pílula). */
const WORKING: ReadonlySet<string> = new Set(["IN_PROGRESS", "IN_REVIEW", "CHANGES_REQUESTED", "APPROVED", "INTEGRATING"]);

/** Tarefa em foco da execução: a primeira em andamento; senão a primeira bloqueada. */
function currentTask(spec: SpecView | null): TaskView | null {
  if (spec === null) return null;
  return (
    spec.tasks.find((t) => t.status !== null && WORKING.has(t.status)) ??
    spec.tasks.find((t) => t.status === "BLOCKED") ??
    null
  );
}

/** "T03 de 4" (tarefa em foco e o total). */
function taskProgress(spec: SpecView | null): string | null {
  const task = currentTask(spec);
  if (spec === null || task === null) return null;
  return t("run.taskProgress", { task: task.id, total: spec.tasks.length });
}

/** Mensagem fixa de falha ao retomar/continuar (mesmo mapeamento do "Aprovar"). */
export function resumeProblem(failure: ApiFailure): string {
  if (failure.status === 409 && failure.reason !== undefined) {
    return APPROVE_REASON_MESSAGES[failure.reason] ?? (failure.message || APPROVE_FALLBACK_MESSAGE);
  }
  return failure.message || APPROVE_FALLBACK_MESSAGE;
}

/** "12 min · US$ 0,04" da spec (tempo ativo somado das tarefas e custo), recarregado com o estado. */
function useSpecUsageLine(projectId: string, specId: string | null, enabled: boolean): string | null {
  const revision = useAppStore((s) => s.revisions[projectId] ?? 0);
  const liveRevision = useAppStore((s) => s.liveRevisions[projectId] ?? 0);
  const [line, setLine] = useState<{ key: string; text: string } | null>(null);
  const key = `${projectId}\u0000${specId ?? ""}`;
  useEffect(() => {
    if (!enabled || specId === null) return undefined;
    let current = true;
    void getUsage(projectId, specId)
      .then((r) => {
        if (!current || !r.ok || !Array.isArray(r.data.specs)) return;
        const spec = r.data.specs.find((s) => s.spec === specId);
        if (spec === undefined) return;
        const secs = (Array.isArray(spec.tasks) ? spec.tasks : []).reduce(
          (sum, t) => sum + (typeof t.active_secs === "number" && Number.isFinite(t.active_secs) ? t.active_secs : 0),
          0,
        );
        const parts = [formatActive(secs)];
        if (spec.cost_usd !== null && Number.isFinite(spec.cost_usd)) parts.push(formatUsd(spec.cost_usd));
        setLine({ key, text: parts.join(" · ") });
      })
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, [projectId, specId, enabled, key, revision, liveRevision]);
  return enabled && line?.key === key ? line.text : null;
}

interface RunPillProps {
  space: SpaceData;
  /** Muda (incrementa) quando o lançador pede "Parar": abre a confirmação. */
  stopSignal?: number;
  /** Muda (incrementa) quando o lançador pede "Retomar"/"Continuar". */
  resumeSignal?: number;
}

/** O que falta para retomar ou continuar ("Falta: Docker daemon, Revisor."). */
function resumeBlockedText(labels: readonly string[]): string {
  return t("run.resumeBlocked", { items: labels.join(", ") });
}

export function RunPill({ space, stopSignal = 0, resumeSignal = 0 }: RunPillProps) {
  // Spec 101: a pílula re-renderiza na troca de idioma.
  useT();
  const titleId = useId();
  const textId = useId();
  const blockedId = useId();
  const { readiness, reloadReadiness } = useShell();
  // "Continuar"/"Retomar" exigem o nível `execute` da prontidão (o servidor responde 412 sem ele).
  const resumeBlocked = gateBlocked(readiness, "execute");
  const blockedLabels = resumeBlocked ? (gateLevel(readiness, "execute")?.missing ?? []).map(readinessItemLabel) : [];
  const { projectId, phase, activeSpec, state } = space;
  const flash = useIntegratedFlash(phase);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 412 do "Continuar": a mensagem sai dos rótulos da prontidão ATUAL (relida depois do 412).
  const [prereqFailure, setPrereqFailure] = useState<ApiFailure | null>(null);
  const usage = useSpecUsageLine(projectId, activeSpec?.id ?? null, space.execution);

  // A execução acabou (estado recarregado sem `run`): o "Parando" sai.
  useEffect(() => {
    if (!space.running) setStopping(false);
  }, [space.running]);

  useEffect(() => {
    if (stopSignal > 0 && space.running) {
      setError(null);
      setConfirming(true);
    }
    // Só o sinal dispara (o estado do espaço é lido no momento).
  }, [stopSignal]);
  const resumeRef = useRef<() => Promise<void>>(async () => undefined);
  useEffect(() => {
    if (resumeSignal > 0) void resumeRef.current();
  }, [resumeSignal]);

  if (state === null) return null;
  const tone = PHASE_TONE[phase];
  const progress = space.execution ? taskProgress(activeSpec) : null;
  const details = [progress, usage].filter((x): x is string => x !== null);
  const specId = activeSpec?.id ?? null;
  const specTitle = activeSpec?.title ?? t("run.noSpec");

  async function stop(): Promise<void> {
    if (specId === null) return;
    setBusy(true);
    setError(null);
    const result = await stopSpec(projectId, specId);
    setBusy(false);
    if (result.ok) {
      setStopping(true);
      setConfirming(false);
    } else {
      setError(result.error.message);
    }
  }

  async function resume(): Promise<void> {
    if (specId === null || busy || resumeBlocked) return;
    setBusy(true);
    setError(null);
    setPrereqFailure(null);
    const result = await resumeSpec(projectId, specId);
    setBusy(false);
    if (result.ok) {
      await space.reload();
      return;
    }
    if (isPrereqFailure(result.error)) {
      setPrereqFailure(result.error);
      setError(prereqMessage(prereqItems(result.error, readiness) ?? []));
      // Relida, a prontidão desabilita o botão com a lista do que falta.
      void reloadReadiness();
      return;
    }
    setError(resumeProblem(result.error));
  }

  const shownError = prereqFailure !== null ? prereqMessage(prereqItems(prereqFailure, readiness) ?? []) : error;

  resumeRef.current = resume;

  return (
    <div
      className="nd-run-pill"
      role="group"
      aria-label={`${PHASE_LABELS[phase]}: ${specTitle}`}
      data-run-pill=""
      data-tone={tone}
      data-phase={phase}
      data-flash={phase === "integrated" && flash === "integrated" ? "true" : undefined}
    >
      <span className="nd-dot nd-dot-sm" data-tone={tone} data-pulse={PULSING_PHASES.has(phase) ? "true" : undefined} aria-hidden="true" />
      <span role="status" className="nd-run-phase">
        {PHASE_LABELS[phase]}
      </span>
      {/*
        Título da spec (095/T07, A-01): o encaixe esconde o título quando cabem menos de ~12
        caracteres (nunca uma letra com reticências); o texto segue no DOM, na dica e no rótulo.
      */}
      <span className="nd-run-title-slot" data-run-title-slot="" title={specTitle}>
        <span className="nd-run-title" title={specTitle}>
          {specTitle}
        </span>
      </span>
      {details.length > 0 ? <span className="nd-run-details">{details.join(" · ")}</span> : null}
      {space.running ? (
        <button
          type="button"
          className="nd-btn nd-pill-btn"
          data-variant="danger"
          data-stop-run=""
          disabled={stopping}
          aria-busy={stopping}
          aria-haspopup="dialog"
          onClick={() => {
            setError(null);
            setConfirming(true);
          }}
        >
          <Icon name="stop" size={14} />
          {stopping ? t("run.stopping") : t("run.stop")}
        </button>
      ) : null}
      {!space.running && (space.paused || space.stalled) ? (
        <>
          <button
            type="button"
            className="nd-btn nd-pill-btn"
            data-resume-run=""
            disabled={busy || resumeBlocked}
            aria-busy={busy}
            aria-describedby={resumeBlocked ? blockedId : undefined}
            title={resumeBlocked ? resumeBlockedText(blockedLabels) : undefined}
            onClick={() => void resume()}
          >
            <Icon name="play" size={14} />
            {space.stalled ? t("run.continue") : t("run.resume")}
          </button>
          {resumeBlocked ? (
            <span id={blockedId} className="nd-run-details" data-resume-blocked="">
              {resumeBlockedText(blockedLabels)}
            </span>
          ) : null}
        </>
      ) : null}
      {shownError !== null && !confirming ? (
        <BarAlert
          source="run"
          message={shownError}
          onDismiss={() => {
            setError(null);
            setPrereqFailure(null);
          }}
        />
      ) : null}
      {confirming ? (
        <Dialog titleId={titleId} title={t("run.stopConfirmTitle")} describedById={textId} onClose={() => setConfirming(false)} width={380} alert>
          <p id={textId} className="nd-dialog-text">
            {t("run.stopConfirmText")}
          </p>
          {error !== null ? (
            <p role="alert" className="nd-dialog-error">
              {error}
            </p>
          ) : null}
          <div className="nd-dialog-actions">
            <button type="button" className="nd-btn" onClick={() => setConfirming(false)}>
              {t("common.cancel")}
            </button>
            <button type="button" className="nd-btn" data-variant="danger" disabled={busy} onClick={() => void stop()}>
              <Icon name="stop" size={14} />
              {t("run.stopConfirmAction")}
            </button>
          </div>
        </Dialog>
      ) : null}
    </div>
  );
}
