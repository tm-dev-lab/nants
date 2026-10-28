/*
 * Painel do sino "Precisa de você" (spec 093, decisão 10):
 * - só o que exige ação, agrupado por projeto, de todos os projetos (`/api/attention`);
 * - ações diretas: as decisões honestas das permissões, abrir as perguntas, abrir o plano,
 *   retomar ou continuar, as opções de escalada (com confirmação) e o setup;
 * - os detalhes de outro projeto (comando do pedido, rótulos das opções de escalada) vêm do
 *   estado dele, carregado ao abrir o painel;
 * - `role="region"` com o rótulo do painel; Esc fecha e devolve o foco ao sino;
 * - todos os textos saem do dicionário no idioma ativo (spec 102 T06).
 */
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { chooseRecovery, resumeSpec } from "../../api/client";
import type { RecoveryOption } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { useDesktopLayer } from "../../desktop/layer";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { refreshProjectState } from "../../projectState";
import { useAppStore, type PendingPermission } from "../../store";
import { agentLabel } from "../shared/agents";
import "../shared/apps.css";
import { environmentReviewHint } from "../shared/permissions";
import { PermissionDecisions } from "../shared/PermissionDecisions";
import { gateBlocked } from "../shared/readiness";
import { ProblemNotice, ReadinessNotice } from "../shared/ReadinessNotice";
import { approveProblem, recoveryProblem, type RunProblem } from "../shared/runProblems";
import { ESCALATION_KIND_LABELS } from "../conversation/PausedRunCard";
import { permissionTitle } from "../conversation/PermissionMessage";
import { attentionGroups, attentionTotal, type AttentionGroup, type AttentionItem } from "./attentionModel";
import { t } from "../../i18n";
import { ROLE_INFO } from "../../settings/rules";
import { pauseReasonOf, pauseReasonText } from "../board/boardModel";
import { roleOf } from "../shared/roleConnection";

const NO_PERMISSIONS: readonly PendingPermission[] = [];

/** Foco de volta ao sino ao fechar pelo teclado. */
function focusBell(): void {
  document.querySelector<HTMLElement>("[data-bell]")?.focus({ preventScroll: true });
}

function AttentionPanel({ onClose }: { onClose: () => void }) {
  const shell = useShell();
  const { t, tp } = useT();
  const layer = useDesktopLayer();
  const ref = useRef<HTMLElement | null>(null);
  const titleId = useId();
  const groups = useMemo(() => attentionGroups(shell.attention, shell.spaceId), [shell.attention, shell.spaceId]);
  const total = attentionTotal(shell.attention);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  // Detalhes dos outros projetos (comando do pedido, opções de escalada): o estado de cada um.
  const toLoad = groups
    .filter((g) => g.projectId !== shell.spaceId && g.items.some((i) => i.kind === "permission" || i.kind === "paused"))
    .map((g) => g.projectId)
    .join("|");
  useEffect(() => {
    if (toLoad === "") return;
    for (const id of toLoad.split("|")) void refreshProjectState(id);
  }, [toLoad]);

  // Abre com o foco no painel (foi o usuário que pediu); clique fora fecha.
  useEffect(() => {
    ref.current?.focus({ preventScroll: true });
    const onPointer = (event: PointerEvent | MouseEvent): void => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (ref.current?.contains(target) === true || target.closest("[data-bell]") !== null) return;
      // Diálogos (imagem ampliada, confirmação) ficam por cima e não fecham o painel.
      if (target.closest("[data-dialog-backdrop]") !== null) return;
      closeRef.current();
    };
    document.addEventListener("pointerdown", onPointer, true);
    return () => document.removeEventListener("pointerdown", onPointer, true);
  }, []);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLElement>): void => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    closeRef.current();
    focusBell();
  };

  const node = (
    <section ref={ref} role="region" aria-label={t("attention.title")} tabIndex={-1} className="nd-attention nd-overlay" data-attention-panel="" onKeyDown={onKeyDown}>
      <div className="nd-popover-head">
        <h2 id={titleId} className="nd-popover-title">
          {t("attention.title")}
        </h2>
        <span className="nd-muted" data-attention-total={total}>
          {total === 0 ? t("attention.nothingPending") : tp("attention.count", total)}
        </span>
        <button
          type="button"
          className="nd-wbtn"
          aria-label={t("common.close")}
          onClick={() => {
            onClose();
            focusBell();
          }}
        >
          <Icon name="close" size={14} />
        </button>
      </div>
      <div className="nd-attention-body">
        {shell.attention?.setup_incomplete === true ? (
          <div className="nd-attention-item" data-attention-item="setup">
            <p className="nd-attention-item-title" data-tone="amber">
              <Icon name="setup" size={14} />
              {t("attention.setup.title")}
            </p>
            <p className="nd-muted">{t("attention.setup.text")}</p>
            <div className="nd-actions">
              <button
                type="button"
                className="nd-btn nd-btn-sm"
                onClick={() => {
                  shell.openSetup();
                  onClose();
                }}
              >
                {t("attention.setup.open")}
              </button>
            </div>
          </div>
        ) : null}
        {groups.length === 0 && shell.attention?.setup_incomplete !== true ? <p className="nd-muted nd-attention-empty">{t("attention.empty")}</p> : null}
        {groups.map((group) => (
          <ProjectGroup key={group.projectId} group={group} active={group.projectId === shell.spaceId} onClose={onClose} />
        ))}
      </div>
    </section>
  );
  return layer === null ? node : createPortal(node, layer);
}

function ProjectGroup({ group, active, onClose }: { group: AttentionGroup; active: boolean; onClose: () => void }) {
  const shell = useShell();
  const { t } = useT();
  const headingId = useId();
  return (
    <section aria-labelledby={headingId} className="nd-attention-group" data-attention-project={group.projectId}>
      <div className="nd-row-between">
        <h3 id={headingId} className="nd-attention-project">
          <Icon name="project" size={14} />
          {group.name}
          {active ? <span className="nd-muted"> {t("attention.thisSpace")}</span> : null}
        </h3>
        {!active ? (
          <button
            type="button"
            className="nd-btn nd-btn-sm"
            data-variant="ghost"
            aria-label={t("attention.openProject", { name: group.name })}
            onClick={() => {
              shell.switchSpace(group.projectId);
              onClose();
            }}
          >
            {t("attention.open")}
          </button>
        ) : null}
      </div>
      <ul className="nd-attention-list">
        {group.items.map((item) => (
          <li key={item.key}>
            <AttentionItemView projectId={group.projectId} item={item} onClose={onClose} />
          </li>
        ))}
      </ul>
    </section>
  );
}

function AttentionItemView({ projectId, item, onClose }: { projectId: string; item: AttentionItem; onClose: () => void }) {
  const shell = useShell();
  const { t } = useT();
  const goTo = (kind: "conversation" | "plan"): void => {
    shell.goTo(projectId, kind);
    onClose();
  };
  switch (item.kind) {
    case "permission":
      return <PermissionItem projectId={projectId} item={item} onOpen={() => goTo("conversation")} />;
    case "choices":
      return (
        <div className="nd-attention-item" data-attention-item="choices">
          <p className="nd-attention-item-title" data-tone="violet">
            <Icon name="conversation" size={14} />
            {t("attention.choices.title")}
          </p>
          <p className="nd-muted">{t("attention.choices.text")}</p>
          <div className="nd-actions">
            <button type="button" className="nd-btn nd-btn-sm" onClick={() => goTo("conversation")}>
              {t("attention.choices.answer")}
            </button>
          </div>
        </div>
      );
    case "plan":
      return (
        <div className="nd-attention-item" data-attention-item="plan">
          <p className="nd-attention-item-title" data-tone="violet">
            <Icon name="plan" size={14} />
            {t("attention.plan.title")}
          </p>
          <div className="nd-actions">
            <button type="button" className="nd-btn nd-btn-sm" onClick={() => goTo("plan")}>
              {t("attention.plan.open")}
            </button>
          </div>
        </div>
      );
    case "paused":
    case "stalled":
      return <RunItem projectId={projectId} item={item} onOpen={() => goTo("conversation")} />;
  }
}

function PermissionItem({ projectId, item, onOpen }: { projectId: string; item: Extract<AttentionItem, { kind: "permission" }>; onOpen: () => void }) {
  const { t } = useT();
  const request = useAppStore((s) => (s.permissions[projectId] ?? NO_PERMISSIONS).find((p) => p.requestId === item.requestId));
  const title =
    item.taskId !== null
      ? t("attention.permission.titleTask", { agent: agentLabel(item.agent), task: item.taskId })
      : t("attention.permission.title", { agent: agentLabel(item.agent) });
  // Mudança de ambiente: o diff precisa ser visto na Conversa antes de aprovar (sem "Aprovar" aqui).
  const environment = item.permissionKind === "environment" || request?.kind === "environment";
  return (
    <div className="nd-attention-item" data-attention-item="permission" data-request-id={item.requestId}>
      <p className="nd-attention-item-title" data-tone="amber">
        <Icon name="alert" size={14} />
        {title}
      </p>
      <p className="nd-muted">{permissionTitle({ kind: item.permissionKind })}</p>
      {environment ? (
        <p className="nd-muted" data-environment-review="">
          {environmentReviewHint()}
        </p>
      ) : request !== undefined ? (
        <>
          <pre className="nd-command nd-mono nd-command-clip">{request.command}</pre>
          <PermissionDecisions projectId={projectId} request={request} compact />
        </>
      ) : (
        <p role="status" className="nd-muted">
          {t("attention.permission.loading")}
        </p>
      )}
      <div className="nd-actions">
        <button type="button" className="nd-btn nd-btn-sm" data-variant={environment ? undefined : "ghost"} onClick={onOpen}>
          <Icon name="conversation" size={14} />
          {t("permissionToasts.goToConversation")}
        </button>
      </div>
    </div>
  );
}

/**
 * Título da execução parada: com a parada pelo provedor (spec 133 T02), o papel e o motivo
 * ("Desenvolvedor pausado: sem crédito no provedor"); sem papel, "Execução parada".
 */
function stalledTitle(item: Extract<AttentionItem, { kind: "stalled" }>): string {
  const role = roleOf(item.role);
  const reason = item.stopCode === null ? null : pauseReasonOf(item.stopCode);
  if (role === null || reason === null) return t("attention.run.stalled");
  return t("attention.run.rolePaused", { role: ROLE_INFO[role].label, reason: pauseReasonText(reason) });
}

function RunItem({ projectId, item, onOpen }: { projectId: string; item: Extract<AttentionItem, { kind: "paused" | "stalled" }>; onOpen: () => void }) {
  const shell = useShell();
  const { t } = useT();
  const state = useAppStore((s) => s.projectStates[projectId]);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<RunProblem | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const blocked = gateBlocked(shell.readiness, "execute");
  const stalled = item.kind === "stalled";
  const spec = state?.specs.find((s) => s.id === item.specId);
  const task = item.kind === "paused" ? spec?.tasks.find((t) => t.id === item.taskId) : undefined;
  const wanted = item.kind === "paused" ? new Set(item.options.map((o) => o.id)) : new Set<string>();
  const options: RecoveryOption[] = (task?.recovery_options ?? []).filter((o) => wanted.size === 0 || wanted.has(o.id));
  const analysis = task?.recovery_options_analysis;

  async function run(action: () => ReturnType<typeof resumeSpec>, toProblem: typeof approveProblem): Promise<void> {
    if (busy) return;
    setBusy(true);
    setProblem(null);
    const result = await action();
    setBusy(false);
    if (result.ok) {
      setConfirming(null);
      shell.refreshAttention();
      void refreshProjectState(projectId);
      return;
    }
    const next = toProblem(result.error, shell.readiness);
    setProblem(next);
    if (next.missing !== null) void shell.reloadReadiness();
  }

  return (
    <div className="nd-attention-item" data-attention-item={item.kind}>
      <p className="nd-attention-item-title" data-tone="amber">
        <Icon name={stalled ? "stop" : "alert"} size={14} />
        {stalled ? stalledTitle(item) : t("attention.run.paused", { task: item.taskId })}
      </p>
      {blocked && shell.readiness !== null ? <ReadinessNotice title={t("attention.run.missing")} missing={shell.readiness.execute.missing} /> : null}
      {options.length > 0 && analysis !== undefined && task !== undefined ? (
        <ul className="nd-attention-options" aria-label={t("attention.run.options")}>
          {options.map((o) => (
            <li key={o.id} className="nd-stack-xs">
              <span className="nd-strong">{o.label}</span>
              <span className="nd-muted">{ESCALATION_KIND_LABELS[o.kind] ?? o.kind}</span>
              {confirming === o.id ? (
                <div className="nd-actions">
                  <button
                    type="button"
                    className="nd-btn nd-btn-sm"
                   
                    disabled={busy || blocked}
                    onClick={() => void run(() => chooseRecovery(projectId, item.specId, task.id, { option_id: o.id, analysis }), recoveryProblem)}
                  >
                    {t("recovery.confirm")}
                  </button>
                  <button type="button" className="nd-btn nd-btn-sm" disabled={busy} onClick={() => setConfirming(null)}>
                    {t("common.cancel")}
                  </button>
                </div>
              ) : (
                <button type="button" className="nd-btn nd-btn-sm" disabled={busy || blocked} onClick={() => setConfirming(o.id)}>
                  {t("attention.run.choose", { label: o.label })}
                </button>
              )}
            </li>
          ))}
        </ul>
      ) : null}
      <div className="nd-actions">
        <button
          type="button"
          className="nd-btn nd-btn-sm"
         
          disabled={busy || blocked}
          onClick={() => void run(() => resumeSpec(projectId, item.specId), approveProblem)}
        >
          <Icon name="play" size={14} />
          {stalled ? t("attention.run.continue") : t("attention.run.resume")}
        </button>
        <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" onClick={onOpen}>
          <Icon name="conversation" size={14} />
          {t("attention.run.view")}
        </button>
      </div>
      {problem !== null ? <ProblemNotice message={problem.message} missing={problem.missing} prereq={problem.prereq} /> : null}
    </div>
  );
}

/** `renderAttentionPanel` padrão do shell. */
export function renderAttentionPanel(props: { onClose: () => void }) {
  return <AttentionPanel onClose={props.onClose} />;
}
