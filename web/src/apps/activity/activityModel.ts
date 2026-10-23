/*
 * Regras puras da janela Atividade (atividade ao vivo e verificação final):
 * - um cartão por agente, com o estado e a ação ao vivo. Os avisos fixos do servidor viram
 *   estados próprios: "gerando imagem" (ferramenta `generate_image`), "ajustando o ritmo"
 *   (limitador da 089) e "repetindo" (nova tentativa da 088);
 * - o histórico de informativos ("Revisor aprovou", "Imagem gerada"), que nunca entra na
 *   contagem do sino (`INFO_KINDS`/`countsTowardAttention`, da T06).
 *
 * Spec 102: os estados, as etapas e os avisos fixos do servidor saem do dicionário no idioma ativo,
 * pelo `code` que o servidor manda junto do texto de reserva (`../shared/activityText`). Só a nova
 * tentativa (088) ainda é reconhecida pela marca do texto. O que o agente escreve aparece como veio.
 */
import { t } from "../../i18n";
import { parseReviewMessage } from "../../reviewMessage";
import { belongsToSpec, type ActivityEntry, type ChatMessage } from "../../store";
import { isPacing, RETRY_MARK, retryDetail, stageText, stepText, toolText } from "../shared/activityText";
import type { InfoKind } from "../attention/attentionModel";
import { formatBytes } from "../files/filesModel";
import type { AppSummary, AppSummaryInput } from "../registry";

/** Cartões fixos, nesta ordem; outros papéis (como os testes) entram só quando ativos. */
const ACTIVITY_AGENTS: readonly string[] = ["developer", "reviewer", "architect"];

export type LiveKind = "image" | "pacing" | "retry" | "tests" | "command" | "tool" | "writing" | "thinking" | "model";

interface LiveAction {
  kind: LiveKind;
  /** Estado curto ("gerando imagem", "ajustando o ritmo", "repetindo", "executando"...). */
  state: string;
  /** O que está acontecendo agora (ferramenta, comando, aviso ou o fim do texto). */
  detail: string;
  /** Etapa (relatório, verificação final), quando há. */
  stage: string | null;
  /**
   * Chamada ao modelo em curso exibida à parte (spec 114 T04), quando a linha mostra outra coisa
   * (o raciocínio ou o texto que já chegou): "recebendo a resposta há 2 min · 48 KB recebidos".
   */
  model: string | null;
}

const DETAIL_MAX = 160;

/** O fim do texto numa linha (o que o agente acabou de escrever). */
export function tailLine(text: string, max = DETAIL_MAX): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length <= max ? flat : flat.slice(flat.length - max).trimStart();
}

type LiveSource = Pick<ActivityEntry, "agent" | "reasoning" | "text" | "tool"> &
  Partial<Pick<ActivityEntry, "command" | "stage" | "stageCode" | "toolCode" | "textCode" | "modelStartedAt" | "modelBytes">>;

/** Tempo decorrido curto: "45 s", "2 min", "1 h 5 min" (sem inventar precisão). */
export function elapsedText(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return t("activity.model.seconds", { seconds });
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t("activity.model.minutes", { minutes });
  return t("activity.model.hours", { hours: Math.floor(minutes / 60), minutes: minutes % 60 });
}

/**
 * Chamada ao modelo em curso (spec 114 T04): o estado ("aguardando o modelo" antes do primeiro
 * byte, "recebendo a resposta" depois) e o progresso real que o cliente conhece: o tempo desde o
 * início e o tamanho já recebido. `null` sem chamada em curso.
 */
function modelProgress(activity: Pick<LiveSource, "modelStartedAt" | "modelBytes">, now: number): { state: string; detail: string } | null {
  if (activity.modelStartedAt === undefined) return null;
  const receiving = activity.modelBytes !== undefined && activity.modelBytes > 0;
  const time = t("activity.model.for", { time: elapsedText(now - activity.modelStartedAt) });
  const detail = receiving ? `${time} · ${t("activity.model.received", { size: formatBytes(activity.modelBytes ?? 0) })}` : time;
  return { state: t(receiving ? "activity.state.receiving" : "activity.state.waitingModel"), detail };
}

/** Estado e ação ao vivo de uma atividade (`now`: o relógio do tempo da chamada ao modelo). */
export function liveActionOf(activity: LiveSource, now: number = Date.now()): LiveAction {
  const stage = stageText(activity);
  const tool = activity.tool.trim();
  const text = activity.text.trim();
  const call = modelProgress(activity, now);
  const model = call === null ? null : `${call.state} ${call.detail}`;
  if (isPacing(activity)) return { kind: "pacing", state: t("activity.state.pacing"), detail: tailLine(stepText(activity)), stage, model: null };
  const retry = [text, tool].find((t) => t.includes(RETRY_MARK));
  if (retry !== undefined) return { kind: "retry", state: t("activity.state.retry"), detail: tailLine(retryDetail(retry)), stage, model: null };
  if (activity.toolCode?.code === "tool.generate_image") {
    const path = activity.toolCode.params.path;
    return { kind: "image", state: t("activity.state.image"), detail: typeof path === "string" && path !== "" ? path : toolText(activity), stage, model: null };
  }
  if (activity.agent === "tests" && activity.command !== undefined) return { kind: "tests", state: t("activity.state.tests"), detail: activity.command, stage, model: null };
  if (activity.command !== undefined) return { kind: "command", state: t("activity.executing"), detail: activity.command, stage, model: null };
  if (tool !== "") return { kind: "tool", state: t("activity.state.working"), detail: toolText(activity), stage, model };
  if (text !== "") return { kind: "writing", state: t("activity.state.writing"), detail: tailLine(stepText(activity)), stage, model };
  if (activity.reasoning.trim() !== "" || call === null) return { kind: "thinking", state: t("activity.thinking"), detail: tailLine(activity.reasoning), stage, model };
  return { kind: "model", state: call.state, detail: call.detail, stage, model: null };
}

/** A atividade mais recente de cada agente (as de tarefas diferentes do mesmo agente viram uma). */
export function latestByAgent(activities: readonly ActivityEntry[]): Map<string, ActivityEntry> {
  const out = new Map<string, ActivityEntry>();
  for (const a of activities) {
    if (a.agent === "tests" && a.command === undefined) continue;
    const seen = out.get(a.agent);
    if (seen === undefined || a.lastAt >= seen.lastAt) out.set(a.agent, a);
  }
  return out;
}

/** Agentes com cartão: os fixos e, depois, os outros que estão ativos. */
export function cardAgents(latest: ReadonlyMap<string, ActivityEntry>): string[] {
  return [...ACTIVITY_AGENTS, ...[...latest.keys()].filter((a) => !ACTIVITY_AGENTS.includes(a))];
}

/** Frase do agente parado. */
export function idleText(agent: string, running: boolean): string {
  if (agent === "architect") return t("activity.idle.architect");
  return t(running ? "activity.idle.waiting" : "activity.idle.none");
}

// --- Histórico (informativos) ---

export interface HistoryItem {
  kind: InfoKind;
  key: string;
  /** Instante (ms). */
  at: number;
  title: string;
  detail: string;
}

/**
 * Linha do manifesto de imagens da spec (`.sdd/specs/<spec>/images.jsonl`, spec 090 T02). O
 * servidor só grava a linha DEPOIS da geração: `stored: true` (padrão) quando a imagem foi gravada
 * na worktree; `stored: false` quando foi cobrada mas recusada (acima de 4 MiB, formato errado ou
 * falha de gravação). `reused`: copiada do cache, sem nova geração.
 */
export interface ManifestImage {
  path: string;
  task: string;
  /** Instante (ms), de `at` (ISO UTC). */
  at: number;
  stored: boolean;
  reused: boolean;
}

/** Caminho do manifesto da spec, lido pela API de arquivos (`source=base`). */
export function imageManifestPath(specId: string): string {
  return `.sdd/specs/${specId}/images.jsonl`;
}

/** Linhas válidas do manifesto (as inválidas são ignoradas, como no servidor). */
export function parseImageManifest(text: string): ManifestImage[] {
  const out: ManifestImage[] = [];
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (trimmed === "") continue;
    let value: unknown;
    try {
      value = JSON.parse(trimmed);
    } catch {
      continue;
    }
    if (typeof value !== "object" || value === null || Array.isArray(value)) continue;
    const v = value as Record<string, unknown>;
    if (typeof v.path !== "string" || v.path === "" || typeof v.task !== "string" || typeof v.at !== "string") continue;
    const at = Date.parse(v.at);
    out.push({ path: v.path, task: v.task, at: Number.isNaN(at) ? 0 : at, stored: v.stored !== false, reused: v.reused === true });
  }
  return out;
}

/**
 * Informativos da spec, do mais novo ao mais antigo:
 * - as aprovações do Revisor (diálogo);
 * - as imagens com EVIDÊNCIA de sucesso: só a linha do manifesto com `stored` verdadeiro. A
 *   chamada `gerando imagem <path>` da trilha é gravada antes do resultado e não conta: em curso,
 *   com falha (registrada ou não) ou recusada, não há linha gravada no manifesto.
 */
export function historyItems(messages: readonly ChatMessage[], images: readonly ManifestImage[], specId: string | null): HistoryItem[] {
  const out: HistoryItem[] = [];
  if (specId !== null) {
    for (const m of messages) {
      if (m.role !== "agent" || m.agent !== "reviewer" || !belongsToSpec(m, specId)) continue;
      if (parseReviewMessage(m.text)?.verdict !== "approved") continue;
      out.push({
        kind: "reviewer_approved",
        key: `review:${m.id}`,
        at: m.at,
        title: m.taskId !== undefined ? t("activity.reviewerApprovedTask", { task: m.taskId }) : t("activity.reviewerApproved"),
        detail: m.attempt !== undefined ? t("conversation.attempt", { attempt: m.attempt }) : "",
      });
    }
  }
  images.forEach((image, index) => {
    if (!image.stored) return;
    out.push({
      kind: "image_generated",
      key: `image:${index}`,
      at: image.at,
      title: image.task !== "" ? t("activity.imageGeneratedTask", { task: image.task }) : t("activity.imageGenerated"),
      detail: image.reused ? t("activity.imageReused", { path: image.path }) : image.path,
    });
  });
  return out.sort((a, b) => b.at - a.at);
}

// --- Resumo da janela ---

export function activitySummary({ space }: AppSummaryInput): AppSummary {
  if (space === null) return {};
  if (space.running) return { subtitle: t("activity.subtitle.live") };
  if (space.architectTurnActive) return { subtitle: t("activity.subtitle.architect") };
  if (space.paused) return { subtitle: t("activity.subtitle.paused") };
  if (space.stalled) return { subtitle: t("activity.subtitle.stalled") };
  return { subtitle: t("activity.subtitle.idle") };
}
