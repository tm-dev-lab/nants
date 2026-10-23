/*
 * Janela Atividade (`DEFAULT_APPS.activity.Content`, em `memo`):
 * - aba "Ao vivo": uma linha por agente (ponto, nome e ação em `--muted`, 095/T05), com o estado e a ação ao vivo (inclusive "gerando
 *   imagem", "ajustando o ritmo" e "repetindo"), a verificação final em destaque e as últimas
 *   ações da tarefa ativa (trilha `tools.jsonl`);
 * - aba "Histórico": os informativos ("Revisor aprovou", "Imagem gerada"), fora da contagem do
 *   sino. Imagem só entra com a linha gravada do manifesto da spec (evidência de sucesso).
 *   A aba escolhida fica no ui-state (`selection.activityTab`).
 *
 * Desempenho: as atividades são lidas por quadro (`useStoreByFrame`): uma rajada de eventos vira
 * no máximo um render por quadro, e só desta janela. A trilha não é buscada com a janela fora de
 * vista.
 */
import { memo, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import type { ToolActionItem } from "../../api/types";
import { Icon, type IconName } from "../../desktop/Icon";
import { useSpace } from "../../desktop/shellContext";
import { useWindowVisibility } from "../../desktop/useWindowVisibility";
import { useT } from "../../i18n/react";
import { useAppStore, type ActivityEntry, type ChatMessage } from "../../store";
import { pickActiveTask } from "../board/boardModel";
import { formatClock } from "../board/ToolActionsSheet";
import type { AppContentProps } from "../registry";
import { toolActionText } from "../shared/activityText";
import { agentLabel, agentTone } from "../shared/agents";
import { ToolLabel } from "../shared/ActivityBubble";
import { FinalVerificationNotice } from "../shared/FinalVerificationNotice";
import { useStoreByFrame } from "../shared/useFrameStore";
import { useToolTrail } from "../shared/useToolTrail";
import "../shared/apps.css";
import "./activity.css";
import { useImageManifest } from "./useImageManifest";
import { cardAgents, historyItems, idleText, latestByAgent, liveActionOf, type HistoryItem, type LiveKind } from "./activityModel";

const NO_ACTIVITIES: readonly ActivityEntry[] = [];
const NO_MESSAGES: readonly ChatMessage[] = [];
/** Últimas ações da tarefa ativa mostradas. */
const RECENT_ACTIONS = 8;
/** Passo do relógio da chamada ao modelo (o texto mostra segundos e minutos). */
const MODEL_TICK_MS = 5_000;

/** Instante atual, renovado a cada `ms` só enquanto `active` (sem relógio parado na tela). */
function useNowWhile(active: boolean, ms: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return undefined;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(timer);
  }, [active, ms]);
  return active ? now : Date.now();
}

type Tab = "live" | "history";

function ActivityContentView(_props: AppContentProps) {
  const space = useSpace();
  // Assina o idioma: o conteúdo (em `memo`) re-renderiza na troca.
  const { t } = useT();
  const tabsId = useId();
  const liveRef = useRef<HTMLButtonElement>(null);
  const historyRef = useRef<HTMLButtonElement>(null);
  if (space === null) return null;
  const tab: Tab = space.selection.activityTab ?? "live";
  const pick = (next: Tab): void => {
    if (next !== tab) space.setSelection({ activityTab: next });
  };
  const onTabKey = (event: ReactKeyboardEvent<HTMLButtonElement>): void => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight" && event.key !== "Home" && event.key !== "End") return;
    event.preventDefault();
    const next: Tab = event.key === "Home" ? "live" : event.key === "End" ? "history" : tab === "live" ? "history" : "live";
    pick(next);
    (next === "live" ? liveRef : historyRef).current?.focus();
  };
  const data = space.data;
  return (
    <div className="nd-activity-root" data-activity-window="">
      <div role="tablist" aria-label={t("window.activity")} className="nd-tabs">
        <button
          ref={liveRef}
          type="button"
          role="tab"
          id={`${tabsId}-live`}
          aria-selected={tab === "live"}
          aria-controls={`${tabsId}-panel`}
          tabIndex={tab === "live" ? 0 : -1}
          className="nd-tab"
          onClick={() => pick("live")}
          onKeyDown={onTabKey}
        >
          <Icon name="activity" size={14} />
          {t("activity.tab.live")}
        </button>
        <button
          ref={historyRef}
          type="button"
          role="tab"
          id={`${tabsId}-history`}
          aria-selected={tab === "history"}
          aria-controls={`${tabsId}-panel`}
          tabIndex={tab === "history" ? 0 : -1}
          className="nd-tab"
          onClick={() => pick("history")}
          onKeyDown={onTabKey}
        >
          <Icon name="info" size={14} />
          {t("activity.tab.history")}
        </button>
      </div>
      <div role="tabpanel" id={`${tabsId}-panel`} aria-labelledby={`${tabsId}-${tab}`} className="nd-activity-panel nd-app-scroll">
        {tab === "live" ? (
          <LiveTab projectId={data.projectId} specId={data.activeSpec?.id ?? null} activeTaskId={data.activeSpec === null ? null : (pickActiveTask(data.activeSpec.tasks)?.id ?? null)} running={data.running} />
        ) : (
          <HistoryTab projectId={data.projectId} specId={data.activeSpec?.id ?? null} />
        )}
      </div>
    </div>
  );
}

export const ActivityContent = memo(ActivityContentView);

const LiveTab = memo(function LiveTab({ projectId, specId, activeTaskId, running }: { projectId: string; specId: string | null; activeTaskId: string | null; running: boolean }) {
  const { t } = useT();
  const activities = useStoreByFrame(projectId, (s) => s.activities[projectId] ?? NO_ACTIVITIES);
  const visible = useWindowVisibility("activity");
  const trail = useToolTrail({ projectId, specId, task: activeTaskId, limit: RECENT_ACTIONS, enabled: visible && activeTaskId !== null });
  const latest = useMemo(() => latestByAgent(activities), [activities]);
  const agents = cardAgents(latest);
  const names = [...latest.keys()].map(agentLabel);
  return (
    <div className="nd-activity-live">
      <p aria-live="polite" className="nd-sr-only" data-testid="activity-summary">
        {names.length === 0 ? "" : t("activity.summaryActive", { names: names.join(", ") })}
      </p>
      <FinalVerificationNotice projectId={projectId} />
      <ul className="nd-agent-rows" aria-label={t("activity.agents")}>
        {agents.map((agent) => (
          <AgentRow key={agent} agent={agent} activity={latest.get(agent)} running={running} />
        ))}
      </ul>
      <RecentActions taskId={activeTaskId} items={trail} />
    </div>
  );
});

/** Tom do ponto de estado: o do agente ao vivo, âmbar quando espera o provedor, apagado parado. */
function dotTone(agent: string, live: { kind: LiveKind } | null): string {
  if (live === null) return "idle";
  return live.kind === "pacing" || live.kind === "retry" ? "amber" : agentTone(agent);
}

/**
 * Uma linha por agente (095/T05, decisão 7): ponto de estado, nome em 600 e a ação atual em
 * `--muted` ("gerando imagem web/img/gato.webp"), com a tarefa em mono e a etapa abaixo, quando há.
 */
function AgentRow({ agent, activity, running }: { agent: string; activity: ActivityEntry | undefined; running: boolean }) {
  // Spec 114 T04: durante a chamada ao modelo, o tempo segue andando entre os sinais do servidor.
  const now = useNowWhile(activity?.modelStartedAt !== undefined, MODEL_TICK_MS);
  const live = activity === undefined ? null : liveActionOf(activity, now);
  const tone = dotTone(agent, live);
  return (
    <li className="nd-agent-row" data-agent-card={agent} data-live={live?.kind} data-tone={agentTone(agent)}>
      <span aria-hidden="true" className="nd-agent-dot" data-dot={tone} />
      <span className="nd-agent-row-name">{agentLabel(agent)}</span>
      <span className="nd-agent-row-body">
        <span className="nd-agent-row-action nd-break">
          {live === null ? (
            <span className="nd-agent-card-idle" data-agent-idle="">
              {idleText(agent, running)}
            </span>
          ) : (
            <>
              <span className="nd-agent-row-state" data-live-state={live.kind} data-tone={tone}>
                {live.state}
              </span>{" "}
              <span className="nd-agent-card-detail" data-live-detail="">
                {live.kind === "tool" ? <ToolLabel text={live.detail} /> : live.detail}
              </span>
            </>
          )}
          {activity?.taskId !== undefined ? (
            <>
              {" "}
              <span className="nd-agent-row-task nd-mono">{activity.taskId}</span>
            </>
          ) : null}
        </span>
        {live !== null && live.stage !== null ? (
          <span className="nd-agent-row-stage" data-live-stage="">
            {live.stage}
          </span>
        ) : null}
        {live !== null && live.model !== null ? (
          <span className="nd-agent-row-stage" data-live-model="">
            {live.model}
          </span>
        ) : null}
      </span>
    </li>
  );
}

function RecentActions({ taskId, items }: { taskId: string | null; items: readonly ToolActionItem[] }) {
  const headingId = useId();
  const { t } = useT();
  if (taskId === null) return null;
  const recent = [...items].reverse();
  return (
    <section aria-labelledby={headingId} className="nd-recent" data-recent-actions={taskId}>
      <h3 id={headingId} className="nd-section-label">
        {t("activity.recent", { task: taskId })}
      </h3>
      {recent.length === 0 ? (
        <p className="nd-muted">{t("activity.recentEmpty")}</p>
      ) : (
        <ol className="nd-recent-list">
          {recent.map((item) => (
            <li key={item.seq} className="nd-recent-item">
              <time dateTime={new Date(item.ts * 1000).toISOString()} className="nd-muted">
                {formatClock(item.ts)}
              </time>
              <span className={`nd-break${item.kind === "command" ? " nd-mono" : ""}`}>{toolActionText(item)}</span>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

const HISTORY_ICON: Readonly<Record<HistoryItem["kind"], IconName>> = { reviewer_approved: "check", image_generated: "image" };

const HistoryTab = memo(function HistoryTab({ projectId, specId }: { projectId: string; specId: string | null }) {
  const { t, lang } = useT();
  const messages = useAppStore((s) => s.messages[projectId] ?? NO_MESSAGES);
  const visible = useWindowVisibility("activity");
  const images = useImageManifest(projectId, specId, visible);
  // Os títulos seguem o idioma: ele entra nas dependências.
  const items = useMemo(() => historyItems(messages, images, specId), [messages, images, specId, lang]);
  return (
    <section className="nd-history" aria-label={t("activity.tab.history")} data-history="">
      <p className="nd-muted">{t("activity.historyIntro")}</p>
      {items.length === 0 ? (
        <p className="nd-muted" data-history-empty="">
          {t("activity.historyEmpty")}
        </p>
      ) : (
        <ul className="nd-history-list">
          {items.map((item) => (
            <li key={item.key} className="nd-history-item" data-history-kind={item.kind}>
              <Icon name={HISTORY_ICON[item.kind]} size={14} className="nd-history-icon" />
              <div className="nd-stack-xs">
                <span className="nd-strong">{item.title}</span>
                {item.detail !== "" ? <span className="nd-muted nd-break">{item.detail}</span> : null}
              </div>
              <time dateTime={new Date(item.at).toISOString()} className="nd-muted">
                {formatClock(Math.floor(item.at / 1000))}
              </time>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
});
