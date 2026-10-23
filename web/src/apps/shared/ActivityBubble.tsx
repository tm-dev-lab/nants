/*
 * Atividade ao vivo dos agentes como evento da Conversa (095/T04, decisão 7): pílula central entre
 * duas linhas ("Desenvolvedor · T03 · gerando textura de Júpiter"), com o ponto no tom do papel, a
 * etapa, a ferramenta (ou "pensando"); abaixo, discretos, o fim do raciocínio, o comando em
 * execução com o relógio e o rascunho do texto. Nada é persistido. O resumo para leitores de tela
 * é discreto e só muda quando muda o conjunto de agentes.
 */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { COMMAND_STALE_MS, type ActivityEntry } from "../../store";
import { draftText, stageText, toolText } from "./activityText";
import { avatarStateForActivity } from "./AgentAvatar";
import { useT } from "../../i18n/react";
import { agentLabel, agentTone } from "./agents";

export type ActivityView = Pick<ActivityEntry, "key" | "agent" | "taskId" | "reasoning" | "text" | "tool"> &
  Partial<Pick<ActivityEntry, "command" | "commandStartedAt" | "commandLastBeat" | "stage" | "stageCode" | "toolCode" | "textCode" | "structured">>;

function Dots() {
  return (
    <span aria-hidden="true" className="nd-dots">
      <span>.</span>
      <span>.</span>
      <span>.</span>
    </span>
  );
}

/** Trecho que mostra sempre o FIM do texto. */
function Tail({ text, className, testId }: { text: string; className: string; testId: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (el !== null) el.scrollTop = el.scrollHeight;
  }, [text]);
  return (
    <div ref={ref} data-testid={testId} className={`nd-tail ${className}`}>
      {text}
    </div>
  );
}

/** Separador das partes da pílula de evento. */
const SEP = " · ";

const URL_RE = /https?:\/\/[^\s<>"')\]]+/g;

/** Rótulo da ferramenta: URLs http/https viram link seguro; o resto é texto. */
export function ToolLabel({ text }: { text: string }) {
  const parts: ReactNode[] = [];
  let last = 0;
  for (const m of text.matchAll(URL_RE)) {
    const url = m[0];
    const at = m.index ?? 0;
    let safe = false;
    try {
      const proto = new URL(url).protocol;
      safe = proto === "http:" || proto === "https:";
    } catch {
      safe = false;
    }
    if (!safe) continue;
    if (at > last) parts.push(text.slice(last, at));
    parts.push(
      <a key={at} href={url} target="_blank" rel="noopener noreferrer" className="nd-link">
        {url}
      </a>,
    );
    last = at + url.length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return <>{parts}</>;
}

function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

function RunningCommand({ command, startedAt, lastBeat, verb }: { command: string; startedAt: number; lastBeat?: number | undefined; verb: string }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  if (lastBeat !== undefined && now - lastBeat > COMMAND_STALE_MS) return null;
  return (
    <p data-testid="activity-command" className="nd-activity-command nd-mono">
      {verb} `{command}` · {formatElapsed(now - startedAt)}
    </p>
  );
}

function ActivityBubble({ activity }: { activity: ActivityView }) {
  const { t } = useT();
  const testsRun = activity.agent === "tests";
  const detail = activity.command !== undefined || activity.reasoning !== "" || activity.text !== "";
  return (
    <div data-testid="activity-bubble" data-agent={activity.agent} className="nd-event">
      <div className="nd-event-line">
        <span className="nd-event-pill" data-state={avatarStateForActivity(activity)}>
          <span className="nd-dot nd-dot-sm" data-tone={agentTone(activity.agent)} data-pulse="true" />
          <span className="nd-event-name">{agentLabel(activity.agent)}</span>
          {activity.taskId !== undefined ? (
            <>
              {SEP}
              <span className="nd-mono">{activity.taskId}</span>
            </>
          ) : null}
          {activity.stage !== undefined ? (
            <>
              {SEP}
              <span data-testid="activity-stage">{stageText(activity)}</span>
            </>
          ) : null}
          {SEP}
          {activity.tool !== "" ? (
            <span data-testid="activity-tool" className="nd-event-action">
              <ToolLabel text={toolText(activity)} />
            </span>
          ) : (
            <span className="nd-event-action">
              {testsRun ? t("activity.running") : t("activity.thinking")}
              <Dots />
            </span>
          )}
        </span>
      </div>
      {detail ? (
        <div className="nd-event-detail">
          {activity.reasoning !== "" ? <Tail testId="activity-reasoning" text={activity.reasoning} className="nd-activity-reasoning" /> : null}
          {activity.command !== undefined ? (
            <RunningCommand
              command={activity.command}
              startedAt={activity.commandStartedAt ?? Date.now()}
              {...(testsRun ? { verb: t("activity.running") } : { verb: t("activity.executing"), lastBeat: activity.commandLastBeat })}
            />
          ) : null}
          {activity.text !== "" ? <Tail testId="activity-text" text={draftText(activity)} className="nd-activity-text" /> : null}
        </div>
      ) : null}
    </div>
  );
}

/** Bolhas temporárias com o resumo `aria-live` discreto. */
export function ActivityBubbles({ activities: all, quiet = false }: { activities: readonly ActivityView[]; quiet?: boolean }) {
  const { t } = useT();
  const activities = all.filter((a) => a.agent !== "tests" || a.command !== undefined);
  const names = activities.map((a) => agentLabel(a.agent));
  const joined = [...new Set(names)].join(", ");
  const summary =
    quiet || names.length === 0
      ? ""
      : activities.every((a) => a.agent === "tests")
        ? t("activity.summaryRunning", { names: joined })
        : t("activity.summaryThinking", { names: joined });
  return (
    <div>
      <p aria-live="polite" className="nd-sr-only" data-testid="activity-summary">
        {summary}
      </p>
      {activities.length > 0 ? (
        <div className="nd-stack" aria-hidden="true">
          {activities.map((a) => (
            <ActivityBubble key={a.key} activity={a} />
          ))}
        </div>
      ) : null}
    </div>
  );
}
