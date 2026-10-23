/*
 * Store da SPA: estado mínimo sem dependências (useSyncExternalStore).
 * - Nenhuma chave de API é guardada aqui.
 * - Mensagens são texto puro (renderizadas como texto pelo React, nunca como HTML).
 * - Status do projeto é derivado dos eventos (ver `reduceEvent`).
 */
import { useSyncExternalStore } from "react";
import type { ApiFailure } from "./api/client";
import { parseCoded, type CodeParams, type Coded } from "./api/codes";
import { providerFailureText } from "./copy";
import { liveNow } from "./liveClock";
import type {
  ActivitySnapshot,
  AgentActivityPayload,
  ApiEvent,
  BlockKind,
  ChatHistoryTurn,
  ConfigView,
  DialogMessage,
  PendingPermissionView,
  PermissionDecision,
  ProjectState,
  ProjectEntry,
  RecoveryOption,
} from "./api/types";
import { pickActiveSpec } from "./phase";

/** Cap de mensagens mantidas por projeto (as mais antigas saem primeiro). */
export const MAX_MESSAGES_PER_PROJECT = 500;
/** Cap de permissões pendentes por projeto (as mais antigas saem primeiro). */
export const MAX_PENDING_PERMISSIONS = 64;
/** Cap de permissões já resolvidas mantidas por projeto, para a conversa da sessão (as mais antigas saem). */
export const MAX_RESOLVED_PERMISSIONS = 50;
/** Teto de projetos com fila de permissão no store (evicção LRU por atividade). */
const MAX_PROJECTS_WITH_PERMISSIONS = 100;
/** Cap de `turn_id` já exibidos por projeto (evicção FIFO). */
const MAX_SHOWN_TURNS = 200;

/** Fase do projeto. `reviewing_plan` só é exibida (derivada por `planStage`); nenhum evento a grava. */
export type ProjectPhase =
  | "idle"
  | "planning"
  | "executing"
  | "waiting_approval"
  | "finalizing"
  | "integrated"
  | "blocked"
  | "stalled"
  | "reviewing_plan";

type MessageRole = "user" | "architect" | "agent" | "system";

export interface ChatMessage {
  id: number;
  role: MessageRole;
  /** Nome do agente (ex.: "developer") quando a mensagem vem de um agente. */
  agent?: string;
  /** Texto puro. */
  text: string;
  /** Momento de recebimento (ms desde a época), para o horário relativo. */
  at: number;
  /**
   * `history`: linha do histórico do servidor (fonte da verdade do que está persistido).
   * `live`: recebida ao vivo, enviada pelo usuário (otimista) ou exibida após POST/SSE.
   */
  origin?: "history" | "live";
  /** `turn_id` do turno, quando conhecido. */
  turnId?: string;
  /** Mensagem do usuário: `true` depois que o POST devolveu o turno (persistida). Ausente = em voo. */
  confirmed?: boolean;
  /** Mensagem do usuário enviada pelo formulário de escolhas (ou "Decida você"). */
  fromChoices?: boolean;
  /** Imagens anexadas pelo usuário (nomes do servidor, spec 082 T05). */
  attachments?: readonly string[];
  /**
   * Ordem de chegada AO VIVO (contador monotônico do store). Linhas hidratadas do histórico não
   * têm `seq`; as ao vivo mantidas na hidratação conservam o seu. É o que a rolagem usa para contar
   * "novas" sem depender de ids ou da posição na lista.
   */
  seq?: number;
  /**
   * Seq do servidor em `dialog.jsonl` (diálogo dos agentes). Ausente = ainda não gravada (ou local).
   * Não é o `seq` local: é o que deduplica o histórico persistido com o ao vivo.
   */
  dialogSeq?: number;
  /** Mensagem do usuário recusada pelo servidor (não enviada): aviso exibido com a ação "Reenviar". */
  unsent?: string;
  /** Mensagem cujo POST desta aba ainda está em andamento (só ela recebe o `turn_id` do SSE). */
  inFlight?: boolean;
  /** Tarefa do diálogo, quando o servidor informa (ausente em mensagens antigas ou gerais). */
  taskId?: string;
  /** Tentativa da tarefa, quando o servidor informa. */
  attempt?: number;
  /**
   * Spec a que a linha do diálogo pertence. Hidratada: a spec da carga. Ao vivo (o SSE não traz
   * `spec_id`): a spec do run corrente do projeto (`run.spec_id`) ou, sem run, a spec ativa no
   * instante da chegada. Ausente = desconhecida (não filtrada).
   */
  specId?: string;
  /**
   * Linha ao vivo sem spec do servidor (servidor antigo): só aparece na spec que era ativa na
   * chegada (`arrivedUnderSpec`) e é descartada na primeira hidratação de uma spec.
   */
  unscoped?: boolean;
  arrivedUnderSpec?: string;
  /**
   * Mensagem do sistema com `code` estável (spec 102): a Conversa a mostra pelo `code` no idioma
   * atual (`systemMessageText`); `text` fica como a reserva em pt-BR.
   */
  code?: string;
  params?: CodeParams;
}

/** `code`/`params` de uma mensagem do servidor, quando houver (só o `code` válido traz `params`). */
function codeFields(code: string | undefined, params: CodeParams | undefined): Pick<ChatMessage, "code" | "params"> {
  if (code === undefined) return {};
  return params === undefined ? { code } : { code, params };
}

/** Pedido de permissão aguardando decisão do usuário. */
export interface PendingPermission {
  requestId: string;
  projectId: string;
  /** `null` quando o comando não está associado a uma tarefa. */
  taskId: string | null;
  agent: string;
  command: string;
  /** Ausente = não permitido: a UI não oferece "Sempre permitir". */
  alwaysAllowed?: boolean;
  /** Código do motivo quando `alwaysAllowed` é falso (`permission.always.<código>`; spec 115 T03). */
  alwaysDeniedCode?: string | null;
  /** `"environment"`: mudança do ambiente Docker, com `reason` e `diff` (texto puro). */
  kind?: string | null;
  reason?: string | null;
  diff?: string | null;
  /** Instante em que o pedido chegou (ms desde a época); define a posição na conversa. */
  at?: number;
}

/**
 * Pedido já resolvido nesta sessão, mantido na conversa. `applied` é o escopo realmente aplicado;
 * `null` quando não há decisão registrada aqui (respondido em outra janela, expirado ou encerrado).
 */
export interface ResolvedPermission extends PendingPermission {
  applied: PermissionDecision | null;
  /** Escolha feita nesta janela, quando diferente do aplicado ("Sempre" que virou escopo menor). */
  requested?: PermissionDecision;
  /** Instante da resolução (ms desde a época). */
  resolvedAt: number;
}

interface ActiveArchitectTurn {
  turnId: string;
  startedAt: number;
}

export interface AppState {
  config: ConfigView | null;
  /** Falha de `GET /api/config` (mostrada com `apiFailureText`, no idioma ativo); `null` sem erro. */
  configError: ApiFailure | null;
  projects: ProjectEntry[];
  activeProjectId: string | null;
  projectStates: Record<string, ProjectState>;
  messages: Record<string, ChatMessage[]>;
  /**
   * Filas de permissão por projeto. A ordem das chaves é LRU: a chave mais antiga é a
   * menos recentemente alterada (ao receber, resolver ou hidratar, a chave vai para o fim).
   */
  permissions: Record<string, PendingPermission[]>;
  /** Pedidos resolvidos nesta sessão, por projeto (mais antigos saem; teto `MAX_RESOLVED_PERMISSIONS`). */
  resolvedPermissions: Record<string, ResolvedPermission[]>;
  phases: Record<string, ProjectPhase>;
  /**
   * Contador por projeto: sobe em `task_updated`, `project_status`, `sdd_draft_updated` e `chat_turn_ended`
   * (e em todos os projetos após `lagged`). A tela recarrega `getProjectState` ao mudar.
   */
  revisions: Record<string, number>;
  /**
   * Contador "ao vivo" por projeto (086/T06): sobe no máximo 1 vez a cada `LIVE_RELOAD_MS` quando
   * chega atividade de ferramenta/comando ou fim de chamada de agente (o uso é gravado a cada
   * chamada). O estado do espaço e o uso recarregam ao mudar: `tool_counts` e o tempo e o custo
   * dos cartões do Quadro.
   * Separado de `revisions` para não recarregar a árvore de arquivos a cada ação.
   */
  liveRevisions: Record<string, number>;
  /** `turn_id` das respostas do Arquiteto já exibidas, por projeto (FIFO, `MAX_SHOWN_TURNS`). */
  shownTurns: Record<string, string[]>;
  /** Turno do Arquiteto em andamento por projeto (servidor: qualquer aba/dispositivo o vê). */
  architectTurns: Record<string, ActiveArchitectTurn>;
  /**
   * Projetos removidos do Nants nesta sessão. Respostas atrasadas (GET de estado/chat, eventos)
   * desses ids são descartadas, para não recriar estado. Só sai daqui ao cadastrar o mesmo caminho.
   */
  removedProjectIds: Record<string, true>;
  /**
   * Último evento de relatório por projeto (`report_ready`/`report_failed`). Cada evento cria um
   * objeto novo, então a tela reage a cada um pela identidade. Filtrado pela spec na tela.
   */
  reportSignals: Record<string, ReportSignal>;
  /** Último evento do app de teste por projeto (spec 021); objeto novo a cada evento. */
  appSignals: Record<string, AppSignal>;
  /** Recuperação do Arquiteto por (projeto, spec, tarefa); ver `recoveryKey`. */
  recoveries: Record<string, RecoveryEntry>;
  /** Último bloqueio por projeto (tarefa e natureza); some ao voltar a executar/integrar. */
  blockKinds: Record<string, { taskId: string; kind: BlockKind }>;
  /** Projeto bloqueado por `task_updated` com o run ainda ativo (recuperação pode retomar). */
  liveBlocked: Record<string, true>;
  /**
   * Início ou retomada mais recente da execução por projeto (spec 114 T04): `project_status`
   * executando ou a transição do estado para "executando". Uma mensagem de parada anterior não
   * explica a parada atual.
   */
  runStarts: Record<string, RunStart>;
  /** Atividade ao vivo dos agentes por projeto (efêmera, spec 024). */
  activities: Record<string, ActivityEntry[]>;
}

/** Teto do reasoning acumulado na UI (mantém o FIM). */
export const MAX_ACTIVITY_REASONING = 8 * 1024;
/** Teto de atividades simultâneas por projeto. */
const MAX_ACTIVITIES_PER_PROJECT = 16;
/** Após `end` sem novo `start`, a bolha permanece por este tempo (não piscar entre passos). */
export const ACTIVITY_END_GRACE_MS = 1_500;
/** Sem eventos por este tempo, a bolha some. */
export const ACTIVITY_IDLE_MS = 120_000;

/** Atividade ao vivo de um agente (efêmera: nunca persistida nem hidratada). */
export interface ActivityEntry {
  /** Agrupamento (agente, tarefa): persiste entre passos consecutivos. */
  key: string;
  agent: string;
  taskId?: string;
  activityId: string;
  /** Snapshot do raciocínio (mantém o fim, teto `MAX_ACTIVITY_REASONING`). */
  reasoning: string;
  /** Rascunho do texto em formação (snapshot). */
  text: string;
  /** Último rótulo de ferramenta. */
  tool: string;
  /** Comando longo em curso (redigido pelo servidor) e seu início (ms, época Unix). */
  command?: string;
  commandStartedAt?: number;
  /** Passo (activity_id) que iniciou o comando; evento de outro passo ou `end` o encerra. */
  commandActivityId?: string;
  /** Último sinal do comando (command/heartbeat, ms locais); sem sinal por COMMAND_STALE_MS ele some. */
  commandLastBeat?: number;
  /**
   * Chamada ao modelo em curso (spec 114 T04): início (ms, época Unix) e os bytes da resposta já
   * recebidos (ausente = nenhum ainda). O heartbeat da chamada mantém a entrada viva; sem sinal por
   * COMMAND_STALE_MS, a chamada some da tela.
   */
  modelStartedAt?: number;
  modelBytes?: number;
  /** Rótulo da etapa (relatório final, ARCHITECTURE.md), exibido no cabeçalho da bolha. */
  stage?: string;
  /**
   * `code`/`params` da etapa, do último rótulo de ferramenta e do último texto (spec 102): a tela
   * os mostra no idioma ativo; `stage`, `tool` e `text` são a reserva em pt-BR.
   */
  stageCode?: Coded;
  toolCode?: Coded;
  textCode?: Coded;
  startedAt: number;
  lastAt: number;
  /** Instante do `end` do passo atual; ausente = passo aberto. */
  endedAt?: number;
}

/**
 * Marco de início da execução: `seq` na ordem de chegada ao vivo (a mesma das mensagens) e `at`,
 * a hora da linha do tempo, para as linhas do histórico, que não têm `seq`. A ordem vem do `seq`:
 * a hora pode empatar no mesmo milissegundo ou durante um recuo do relógio de parede.
 */
export interface RunStart {
  seq: number;
  at: number;
}

function activityKey(agent: string, taskId?: string | null): string {
  return JSON.stringify([agent, taskId ?? null]);
}

/** Margem para não apagar um comando recém-iniciado por causa de um instantâneo mais antigo. */
const COMMAND_SNAPSHOT_GUARD_MS = 5_000;

/**
 * Execução de `test_command`/checagem (agente "tests", spec 071): vive até o `end`/resultado do
 * servidor, sem heartbeat; por isso não expira por inatividade nem pelo snapshot do servidor.
 */
/** Etapa `stage` da atividade da verificação final (emitida pelo runner). */
export const FINAL_VERIFICATION_STAGE = "final_verification";

/** `true` quando há verificação final em curso (atividade com esse `stage` e sem `end`). */
export function isFinalVerificationActive(list: readonly ActivityEntry[] | undefined): boolean {
  return (list ?? []).some((a) => a.stage === FINAL_VERIFICATION_STAGE && a.endedAt === undefined);
}

function isTestsRun(a: Pick<ActivityEntry, "agent" | "command">): boolean {
  return a.agent === "tests" && a.command !== undefined;
}

/** Sem heartbeat/evento do comando por mais que isto (heartbeat a cada 20 s), o relógio some. */
export const COMMAND_STALE_MS = 60_000;

function withoutCommand(a: ActivityEntry): ActivityEntry {
  const { command: _c, commandStartedAt: _s, commandActivityId: _i, commandLastBeat: _b, ...rest } = a;
  return rest;
}

function withoutModel(a: ActivityEntry): ActivityEntry {
  const { modelStartedAt: _s, modelBytes: _b, ...rest } = a;
  return rest;
}

/** Bytes válidos de `received_bytes` (inteiro não negativo), ou `undefined`. */
function receivedBytes(value: unknown): number | undefined {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : undefined;
}

function keepTail(text: string, max: number): string {
  return text.length <= max ? text : text.slice(text.length - max);
}

/**
 * Mescla o instantâneo do servidor na lista local. Entrada local do mesmo agente/tarefa que é
 * mais nova (ou já encerrada no mesmo passo) vence; entradas locais ausentes no servidor ficam
 * como estão (as regras de vencimento atuais as tratam).
 */
export function mergeServerActivities(
  list: readonly ActivityEntry[],
  items: readonly ActivitySnapshot[],
  now: number,
): ActivityEntry[] {
  let out = list.slice();
  // O servidor é a verdade sobre comandos: um `command_end` perdido (SSE caído no celular) não pode
  // deixar o relógio preso. Entrada sem comando no servidor, ou ausente dele, encerra o comando local.
  const seen = new Set(items.map((it) => activityKey(it.agent, it.task_id)));
  out = out.map((a) => {
    if (a.command === undefined || seen.has(a.key) || isTestsRun(a)) return a;
    if (now - (a.commandStartedAt ?? now) < COMMAND_SNAPSHOT_GUARD_MS) return a;
    return withoutCommand(a);
  });
  for (const it of items) {
    const key = activityKey(it.agent, it.task_id);
    const lastAt = now - it.age_ms;
    if (now - lastAt >= ACTIVITY_IDLE_MS && it.stage !== FINAL_VERIFICATION_STAGE) continue;
    let local = out.find((a) => a.key === key);
    if (local !== undefined) {
      const serverHasNoCommand = typeof it.command !== "string" || it.command === "";
      if (
        local.command !== undefined &&
        serverHasNoCommand &&
        (local.commandActivityId !== it.activity_id || lastAt >= (local.commandStartedAt ?? 0))
      ) {
        out = out.map((a) => (a.key === key ? withoutCommand(a) : a));
        local = withoutCommand(local);
      }
      if (local.lastAt >= lastAt) continue;
      if (local.endedAt !== undefined && local.activityId === it.activity_id) continue;
    }
    const entry: ActivityEntry = {
      key,
      agent: it.agent,
      ...(it.task_id !== null ? { taskId: it.task_id } : {}),
      activityId: it.activity_id,
      reasoning: keepTail(it.reasoning, MAX_ACTIVITY_REASONING),
      text: it.text,
      tool: it.tool,
      ...(typeof it.command === "string" && it.command !== ""
        ? {
            command: it.command,
            commandStartedAt: Math.min(it.command_started_at ?? lastAt, now),
            commandActivityId: it.activity_id,
            commandLastBeat: lastAt,
          }
        : {}),
      ...(typeof it.model_started_at === "number"
        ? {
            modelStartedAt: Math.min(it.model_started_at, now),
            ...(receivedBytes(it.received_bytes) !== undefined ? { modelBytes: receivedBytes(it.received_bytes) } : {}),
          }
        : {}),
      ...(typeof it.stage === "string" && it.stage !== "" ? { stage: it.stage } : {}),
      ...codedEntry("stageCode", parseCoded(it.stage_code, undefined)),
      ...codedEntry("toolCode", parseCoded(it.tool_code, it.tool_params)),
      ...codedEntry("textCode", parseCoded(it.text_code, it.text_params)),
      startedAt: lastAt,
      lastAt,
    };
    out = [...out.filter((a) => a.key !== key), entry];
  }
  return out.slice(-MAX_ACTIVITIES_PER_PROJECT);
}

function codedEntry<K extends "stageCode" | "toolCode" | "textCode">(key: K, coded: Coded | undefined): Partial<Record<K, Coded>> {
  return coded === undefined ? {} : ({ [key]: coded } as Partial<Record<K, Coded>>);
}

/** Troca (ou apaga) o `code` de um campo da entrada. */
function setCoded(entry: ActivityEntry, key: "stageCode" | "toolCode" | "textCode", coded: Coded | undefined): void {
  if (coded === undefined) delete entry[key];
  else entry[key] = coded;
}

/** Aplica um `agent_activity` à lista do projeto (função pura). */
function reduceActivity(
  list: readonly ActivityEntry[],
  payload: AgentActivityPayload,
  now: number,
): ActivityEntry[] {
  const taskId = typeof payload.task_id === "string" ? payload.task_id : undefined;
  const key = activityKey(payload.agent, taskId);
  const existing = list.find((a) => a.key === key);
  const base: ActivityEntry = existing ?? {
    key,
    agent: payload.agent,
    ...(taskId === undefined ? {} : { taskId }),
    activityId: payload.activity_id,
    reasoning: "",
    text: "",
    tool: "",
    startedAt: now,
    lastAt: now,
  };
  let next: ActivityEntry;
  const text = payload.text ?? "";
  // Rede de segurança: um evento de OUTRO passo (o comando já acabou) encerra o comando exibido.
  if (
    existing?.command !== undefined &&
    payload.kind !== "command" &&
    payload.kind !== "end" &&
    existing.commandActivityId !== payload.activity_id
  ) {
    const cleared = reduceActivity(list.map((a) => (a.key === key ? withoutCommand(a) : a)), payload, now);
    return cleared;
  }
  switch (payload.kind) {
    case "start":
      next = { ...base, activityId: payload.activity_id, reasoning: "", text: "", tool: "", lastAt: now };
      delete next.toolCode;
      delete next.textCode;
      delete next.command;
      delete next.commandStartedAt;
      delete next.commandActivityId;
      delete next.commandLastBeat;
      delete next.modelStartedAt;
      delete next.modelBytes;
      delete next.endedAt;
      break;
    case "model": {
      // Spec 114 T04: começou a chamada ao modelo (o relógio da Atividade parte daqui).
      const at = typeof payload.model_started_at === "number" ? Math.min(payload.model_started_at, now) : now;
      next = { ...base, activityId: payload.activity_id, modelStartedAt: at, lastAt: now };
      delete next.modelBytes;
      delete next.endedAt;
      break;
    }
    case "model_end":
      next = { ...base, lastAt: now };
      delete next.modelStartedAt;
      delete next.modelBytes;
      break;
    case "reasoning":
      next = { ...base, activityId: payload.activity_id, reasoning: keepTail(text, MAX_ACTIVITY_REASONING), lastAt: now };
      delete next.endedAt;
      break;
    case "text":
      next = { ...base, activityId: payload.activity_id, text, lastAt: now };
      setCoded(next, "textCode", parseCoded(payload.code, payload.params));
      delete next.endedAt;
      break;
    case "tool":
      next = { ...base, activityId: payload.activity_id, tool: text, lastAt: now };
      setCoded(next, "toolCode", parseCoded(payload.code, payload.params));
      delete next.endedAt;
      break;
    case "command": {
      const at = typeof payload.command_started_at === "number" ? Math.min(payload.command_started_at, now) : now;
      next = {
        ...base,
        activityId: payload.activity_id,
        command: text,
        commandStartedAt: at,
        commandActivityId: payload.activity_id,
        commandLastBeat: now,
        lastAt: now,
      };
      delete next.endedAt;
      break;
    }
    case "heartbeat": {
      // Mantém a bolha viva (não expira por inatividade enquanto o comando ou a chamada rodam) e,
      // na chamada ao modelo, atualiza os bytes recebidos.
      const bytes = receivedBytes(payload.received_bytes);
      next = {
        ...base,
        lastAt: now,
        ...(base.command !== undefined ? { commandLastBeat: now } : {}),
        ...(base.modelStartedAt !== undefined && bytes !== undefined ? { modelBytes: bytes } : {}),
      };
      break;
    }
    case "command_end":
      next = { ...base, lastAt: now };
      delete next.command;
      delete next.commandStartedAt;
      delete next.commandActivityId;
      delete next.commandLastBeat;
      delete next.commandLastBeat;
      break;
    default:
      // `end` de um passo antigo (outro activity_id) não encerra o passo atual.
      if (existing !== undefined && existing.activityId !== payload.activity_id) return list.slice();
      next = { ...base, endedAt: now, lastAt: now };
      // O passo acabou: nenhum comando nem chamada ao modelo dele segue rodando.
      delete next.command;
      delete next.commandStartedAt;
      delete next.commandActivityId;
      delete next.commandLastBeat;
      delete next.modelStartedAt;
      delete next.modelBytes;
  }
  if (typeof payload.stage === "string" && payload.stage !== "") {
    next.stage = payload.stage;
    setCoded(next, "stageCode", parseCoded(payload.stage_code, payload.stage_params));
  } else if (payload.kind === "start") {
    delete next.stage;
    delete next.stageCode;
  }
  const rest = list.filter((a) => a.key !== key);
  return [...rest, next].slice(-MAX_ACTIVITIES_PER_PROJECT);
}

/** Remove as atividades vencidas: `end` há mais que a carência, ou sem eventos há muito tempo. */
export function pruneActivities(list: readonly ActivityEntry[], now: number): ActivityEntry[] {
  return list
    .filter((a) => (isTestsRun(a) || (a.stage === FINAL_VERIFICATION_STAGE && a.endedAt === undefined) || now - a.lastAt < ACTIVITY_IDLE_MS) && (a.endedAt === undefined || now - a.endedAt < ACTIVITY_END_GRACE_MS))
    .map((a) => (a.command !== undefined && !isTestsRun(a) && now - (a.commandLastBeat ?? a.lastAt) > COMMAND_STALE_MS ? withoutCommand(a) : a))
    .map((a) => (a.modelStartedAt !== undefined && now - a.lastAt > COMMAND_STALE_MS ? withoutModel(a) : a));
}

/** Estado de recuperação de uma tarefa: analisando, retry aplicado ou escalada ao humano. */
export interface RecoveryEntry {
  phase: "analyzing" | "retry" | "escalate";
  cycle: number;
  /** Opções clicáveis da escalada (spec 073), vindas do evento. */
  options?: RecoveryOption[];
  options_analysis?: number;
}

/** Chave (projeto, spec, tarefa): a mesma tarefa em specs diferentes nunca se mistura. */
export function recoveryKey(projectId: string, specId: string, taskId: string): string {
  return JSON.stringify([projectId, specId, taskId]);
}

/** Remove entradas `analyzing` (do projeto, ou só de uma chave); a decisão perdida não deixa selo preso. */
export function dropAnalyzing(
  recoveries: Record<string, RecoveryEntry>,
  projectId: string | null,
  onlyKey?: string,
): Record<string, RecoveryEntry> {
  const out: Record<string, RecoveryEntry> = {};
  for (const [key, entry] of Object.entries(recoveries)) {
    const hit =
      entry.phase === "analyzing" &&
      (onlyKey !== undefined ? key === onlyKey : projectId === null || key.startsWith(`["${projectId}",`));
    if (!hit) out[key] = entry;
  }
  return out;
}

/** Selo de recuperação do cartão no Quadro: só "analisando" e "estratégia nova"; escalada não tem selo. */
export function recoveryBadgeFor(
  state: AppState,
  projectId: string,
  specId: string,
  taskId: string,
): { phase: "analyzing" | "retry"; cycle: number } | null {
  const entry = state.recoveries[recoveryKey(projectId, specId, taskId)];
  if (entry === undefined || entry.phase === "escalate") return null;
  return { phase: entry.phase, cycle: entry.cycle };
}

/** Sinal de relatório da spec: pronto (recarregar) ou falhou (motivo fixo do servidor). */
/** Chave do sinal de relatório: uma por (projeto, spec), para que a falha de uma não some com a de outra. */
export function reportKey(projectId: string, specId: string): string {
  return `${projectId}:${specId}`;
}

type AppSignal =
  | { kind: "started"; specId: string; url: string | null }
  | { kind: "exited"; code: number | null; reason: string };

type ReportSignal =
  | { specId: string; kind: "ready" }
  | { specId: string; kind: "failed"; code: string };

const initialState: AppState = {
  config: null,
  configError: null,
  projects: [],
  activeProjectId: null,
  projectStates: {},
  messages: {},
  permissions: {},
  resolvedPermissions: {},
  phases: {},
  revisions: {},
  liveRevisions: {},
  shownTurns: {},
  architectTurns: {},
  removedProjectIds: {},
  reportSignals: {},
  appSignals: {},
  recoveries: {},
  blockKinds: {},
  liveBlocked: {},
  runStarts: {},
  activities: {},
};

type Listener = () => void;

interface AppStore {
  getState(): AppState;
  setState(patch: Partial<AppState> | ((state: AppState) => Partial<AppState>)): void;
  subscribe(listener: Listener): () => void;
  /** Aplica um evento do stream (função pura sobre o estado + publicação). */
  applyEvent(event: ApiEvent): void;
  /** Acrescenta mensagem ao projeto, respeitando o cap. */
  appendMessage(projectId: string, message: Omit<ChatMessage, "id" | "at">): number;
  /** Marca a mensagem otimista do usuário como confirmada, com o `turn_id` devolvido pelo POST. */
  confirmUserMessage(projectId: string, messageId: number, turnId: string): void;
  /** Marca a mensagem do usuário como não enviada (com aviso) ou, com `null`, volta a em voo. */
  setUserMessageUnsent(projectId: string, messageId: number, notice: string | null): void;
  /**
   * Início de turno do Arquiteto (SSE ou estado do projeto). Mostra a mensagem do usuário se ainda
   * não está na conversa (dedupe por `turn_id`; a mensagem em voo desta aba só ganha o `turn_id`).
   */
  beginArchitectTurn(projectId: string, turn: { turnId: string; startedAt: number; userText?: string }): void;
  /** Fim de turno (ou resposta já exibida): some a bolha "pensando". Sem `turnId` encerra qualquer um. */
  endArchitectTurn(projectId: string, turnId?: string): void;
  /** Reconcilia com `architect_turn` do estado do projeto (reidratação após refresh). */
  syncArchitectTurn(
    projectId: string,
    turn: { turn_id: string; user_text: string; started_at: number } | null,
    clockAtRequest?: number,
  ): void;
  /** O POST desta aba terminou (qualquer resultado): a mensagem deixa de estar em voo. */
  settleUserMessage(projectId: string, messageId: number): void;
  /**
   * Mostra a resposta do Arquiteto uma única vez por `turnId` (POST ou SSE, o que chegar
   * primeiro). Sem `turnId` (compatibilidade) não há dedupe. Devolve `false` se já estava exibida.
   */
  showArchitectReply(projectId: string, text: string, turnId?: string): boolean;
  /**
   * Acrescenta o histórico carregado do servidor antes das mensagens já presentes, na ordem do
   * arquivo. Turno cujo `turn_id` já foi exibido (POST/SSE) é omitido; os demais entram inteiros
   * e os ids dos turnos já respondidos (com a linha do Arquiteto) passam a contar como exibidos; o
   * turno em andamento continua aceitando a resposta por SSE ou POST. Turnos sem id nunca são omitidos.
   * O eco do composer cuja linha já foi persistida (pelo `turn_id` ou, sem ele, pelo texto e anexos
   * de um turno ainda não respondido) sai: a mensagem do usuário aparece uma vez só (114/T03).
   */
  hydrateHistory(projectId: string, turns: readonly ChatHistoryTurn[]): void;
  /**
   * Substitui o diálogo PERSISTIDO dos agentes pelo conjunto lido do servidor (fonte da verdade).
   * Mantém ao vivo: sem `dialogSeq`, ou com seq posterior ao maior lido. Descarta ao vivo cujo seq
   * já está no servidor (ou que é anterior ao lido e sumiu, por rotação). Não toca no chat.
   */
  hydrateDialog(projectId: string, specId: string, rows: readonly DialogMessage[]): void;
  /** Recuperação: acrescenta só as linhas cujo `dialogSeq` ainda não está no diálogo do projeto. */
  mergeDialog(projectId: string, specId: string, rows: readonly DialogMessage[]): void;
  /** Remove o pedido de permissão (após decisão enviada com sucesso). */
  clearPermission(projectId: string, requestId: string): void;
  /** Remove uma mensagem da conversa pelo id (ex.: otimista que o histórico já trouxe). */
  removeMessage(projectId: string, messageId: number): void;
  /**
   * Move o pedido pendente para os resolvidos da sessão (após decisão enviada, ou 404). Idempotente:
   * sem pendente com esse id, não faz nada. `applied` `null` = sem decisão registrada aqui.
   */
  settlePermission(
    projectId: string,
    requestId: string,
    applied: PermissionDecision | null,
    requested?: PermissionDecision,
  ): void;
  /**
   * Reconcilia a fila com `pending_permissions` do servidor (merge por request_id). Remove o
   * que sumiu do servidor, exceto pedidos que chegaram depois de `startedAt` (ver
   * `arrivalClockNow`).
   */
  hydratePermissions(projectId: string, server: readonly PendingPermissionView[], startedAt: number): void;
  /** Marca o início ou a retomada da execução do projeto agora (estado lido com run ativo). */
  markRunStarted(projectId: string): void;
  /**
   * Remove o projeto do estado do cliente: lista, config, mensagens, shownTurns, pendentes, fase,
   * contador e estado. Marca o id como removido na sessão. Idempotente.
   */
  forgetProject(projectId: string): void;
  /** Mescla o instantâneo de atividade do servidor (reidratação). */
  mergeActivities(projectId: string, items: readonly ActivitySnapshot[], now: number): void;
  /** Descarta atividades vencidas (carência pós-`end`, inatividade). */
  pruneActivities(projectId: string, now: number): void;
  /** Descarta as atividades do Arquiteto (turno respondido ou terminado). */
  clearArchitectActivity(projectId: string): void;
  /** Libera um id removido (o mesmo caminho foi cadastrado de novo). */
  readmitProject(projectId: string): void;
}

/** `true` quando o projeto foi removido do Nants nesta sessão. */
export function isProjectRemoved(state: AppState, projectId: string): boolean {
  return state.removedProjectIds[projectId] === true;
}

/** Relógio de chegada de pedidos de permissão (ordem de chegada pelo stream). */
/** Sobe a cada `chat_turn_started` aplicado: um `null` de snapshot anterior a ele é obsoleto. */
let turnStartClock = 0;
export function turnStartClockNow(): number {
  return turnStartClock;
}
const MAX_ENDED_TURNS = 32;
let arrivalClock = 0;
const arrivals = new Map<string, number>();

function arrivalKey(projectId: string, requestId: string): string {
  return `${projectId}\u0000${requestId}`;
}

/** Esquece os relógios de chegada de um projeto (ao removê-lo). */
function clearArrivals(projectId: string): void {
  const prefix = `${projectId}\u0000`;
  for (const key of [...arrivals.keys()]) {
    if (key.startsWith(prefix)) arrivals.delete(key);
  }
}

/**
 * Ordem de chegada ao vivo de um pedido de permissão (0 = sem registro, como o hidratado do GET).
 * Desempata pedidos com a mesma hora: com o relógio de parede recuando, `liveNow()` repete a última
 * hora, e a ordem por projeto deixaria de ser a de chegada (114/T04).
 */
export function permissionArrival(projectId: string, requestId: string): number {
  return arrivals.get(arrivalKey(projectId, requestId)) ?? 0;
}

/** Valor atual do relógio de chegada: tire antes de um GET para saber o que chegou depois. */
export function arrivalClockNow(): number {
  return arrivalClock;
}

/**
 * Limita as mensagens ao cap, descartando as mais antigas. O teto vale por família: o chat
 * (Arquiteto e usuário) e o diálogo dos agentes (`role === "agent"`) têm cada um até o cap, para
 * que o diálogo carregado não empurre o histórico do Arquiteto para fora (e vice-versa).
 */
function capMessages(list: ChatMessage[]): ChatMessage[] {
  let agents = 0;
  for (const m of list) if (m.role === "agent") agents += 1;
  const others = list.length - agents;
  if (agents <= MAX_MESSAGES_PER_PROJECT && others <= MAX_MESSAGES_PER_PROJECT) return list;
  let keepAgents = MAX_MESSAGES_PER_PROJECT;
  let keepOthers = MAX_MESSAGES_PER_PROJECT;
  const kept: ChatMessage[] = [];
  for (const m of [...list].reverse()) {
    if (m.role === "agent") {
      if (keepAgents > 0) {
        keepAgents -= 1;
        kept.push(m);
      }
    } else if (keepOthers > 0) {
      keepOthers -= 1;
      kept.push(m);
    }
  }
  return kept.reverse();
}

/** Diálogo dos agentes na ordem certa: persistidas por `dialogSeq`, depois as sem seq (ao vivo, não gravadas). */
function sortAgentRows(rows: readonly ChatMessage[]): ChatMessage[] {
  const persisted = rows
    .filter((m) => m.dialogSeq !== undefined)
    .sort((a, b) => (a.dialogSeq ?? 0) - (b.dialogSeq ?? 0));
  const local = rows.filter((m) => m.dialogSeq === undefined);
  return [...persisted, ...local];
}

/** `true` quando a linha pertence à spec (ou é de spec desconhecida, que conta como da spec). */
function inSpec(m: ChatMessage, specId: string): boolean {
  return m.specId === undefined || m.specId === specId;
}

/** Maior `dialogSeq` da spec no diálogo do projeto (0 sem nenhum). Base da recuperação após reconexão. */
export function maxDialogSeq(state: AppState, projectId: string, specId: string): number {
  let max = 0;
  for (const m of state.messages[projectId] ?? []) {
    if (m.role === "agent" && m.dialogSeq !== undefined && inSpec(m, specId) && m.dialogSeq > max) {
      max = m.dialogSeq;
    }
  }
  return max;
}

/** Identidade de uma linha do diálogo: spec + seq (o mesmo seq pode existir em specs diferentes). */
function dialogKey(specId: string | undefined, seq: number): string {
  return `${specId ?? ""}:${seq}`;
}

/** Spec ativa do projeto no store (`undefined` sem estado). Só marca a chegada de linha sem spec. */
function activeSpecOf(state: AppState, projectId: string): string | undefined {
  const project = state.projectStates[projectId];
  return project === undefined ? undefined : pickActiveSpec(project.specs)?.id;
}

/**
 * `true` quando a linha do diálogo deve aparecer na spec. Linha com spec própria: só a dela.
 * Linha sem spec (servidor antigo): só na spec que era ativa no instante da chegada.
 */
export function belongsToSpec(m: ChatMessage, specId: string): boolean {
  if (m.role !== "agent") return false;
  if (m.unscoped === true) return m.arrivedUnderSpec === specId;
  return m.specId === undefined || m.specId === specId;
}

/** Linha do diálogo persistido vira mensagem de agente (mesma tradução do tempo real). */
function dialogRowToMessage(id: number, row: DialogMessage, specId: string): ChatMessage {
  return {
    id,
    role: "agent",
    agent: row.agent,
    text: agentMessageText(row.agent, row.text),
    at: row.ts * 1000,
    origin: "history",
    dialogSeq: row.seq,
    specId,
    ...(row.task_id === null ? {} : { taskId: row.task_id }),
    ...(row.attempt === null ? {} : { attempt: row.attempt }),
    ...codeFields(row.code, row.params),
  };
}

/** Limita a lista de permissões pendentes por projeto, descartando as mais antigas. */
function capPermissions(list: PendingPermission[]): PendingPermission[] {
  return list.length > MAX_PENDING_PERMISSIONS
    ? list.slice(list.length - MAX_PENDING_PERMISSIONS)
    : list;
}

/**
 * Acrescenta pedidos resolvidos ao projeto: um pedido com o mesmo `request_id` sai antes (sem
 * duplicata) e o teto descarta os mais antigos.
 */
function withResolved(
  map: Record<string, ResolvedPermission[]>,
  projectId: string,
  entries: readonly ResolvedPermission[],
): Record<string, ResolvedPermission[]> {
  let list = map[projectId] ?? [];
  for (const entry of entries) {
    list = [...list.filter((r) => r.requestId !== entry.requestId), entry];
  }
  if (list.length > MAX_RESOLVED_PERMISSIONS) list = list.slice(list.length - MAX_RESOLVED_PERMISSIONS);
  return { ...map, [projectId]: list };
}

/** Pedido pendente convertido em resolvido (`applied` `null` = sem decisão registrada aqui). */
function toResolved(
  pending: PendingPermission,
  applied: PermissionDecision | null,
  requested: PermissionDecision | undefined,
  resolvedAt: number,
): ResolvedPermission {
  return { ...pending, applied, ...(requested === undefined ? {} : { requested }), resolvedAt };
}

/** Folga entre a hora do eco (relógio da aba) e a do servidor ao casar o eco com a linha persistida. */
const ECHO_MATCH_SLACK_MS = 5 * 60_000;

/**
 * Eco do composer (em voo e ainda sem `turn_id`, porque o `chat_turn_started` se perdeu numa queda
 * do stream) cuja linha já está no histórico: mesmo texto, mesmos anexos, turno ainda não
 * respondido nesta aba e gravado depois do envio (com folga de relógio). Devolve o id do eco, ou
 * `null`. Só há um turno por projeto: no máximo um eco em voo.
 */
function persistedEcho(current: readonly ChatMessage[], turns: readonly ChatHistoryTurn[], shown: readonly string[]): number | null {
  const echo = current.find((m) => m.role === "user" && m.origin !== "history" && m.inFlight === true && m.turnId === undefined);
  if (echo === undefined) return null;
  const claimed = new Set<string>(shown);
  for (const m of current) if (m.origin !== "history" && m.turnId !== undefined) claimed.add(m.turnId);
  const same = (a: readonly string[] | undefined, b: readonly string[] | undefined): boolean => {
    const x = a ?? [];
    const y = b ?? [];
    return x.length === y.length && x.every((v, i) => v === y[i]);
  };
  const match = turns.some(
    (t) =>
      t.role === "user" &&
      t.turn_id !== undefined &&
      !claimed.has(t.turn_id) &&
      t.text === echo.text &&
      same(t.attachments, echo.attachments) &&
      t.ts * 1000 >= echo.at - ECHO_MATCH_SLACK_MS,
  );
  return match ? echo.id : null;
}

/** Limita a lista de `turn_id` exibidos (FIFO). */
function capTurns(list: string[]): string[] {
  return list.length > MAX_SHOWN_TURNS ? list.slice(list.length - MAX_SHOWN_TURNS) : list;
}

/**
 * Põe a fila do projeto no fim da ordem LRU (remove a chave e a recoloca). Usado sempre que a
 * fila é recebida, alterada ou hidratada.
 */
function withQueue(
  map: Record<string, PendingPermission[]>,
  projectId: string,
  list: PendingPermission[],
): Record<string, PendingPermission[]> {
  const next = { ...map };
  delete next[projectId];
  next[projectId] = list;
  return next;
}

/**
 * Teto de projetos com fila. Ordem de evicção:
 * 1. filas vazias, da menos recente para a mais recente;
 * 2. só se ainda houver excesso, filas com pedido pendente, da menos recente para a mais recente.
 * Uma fila com pedido pendente nunca é descartada enquanto houver fila vazia para descartar.
 */
function capProjectQueues(
  map: Record<string, PendingPermission[]>,
): Record<string, PendingPermission[]> {
  const keys = Object.keys(map);
  let excess = keys.length - MAX_PROJECTS_WITH_PERMISSIONS;
  if (excess <= 0) return map;
  const drop = new Set<string>();
  for (const key of keys) {
    if (excess <= 0) break;
    if ((map[key] ?? []).length === 0) {
      drop.add(key);
      excess -= 1;
    }
  }
  for (const key of keys) {
    if (excess <= 0) break;
    if (!drop.has(key)) {
      drop.add(key);
      excess -= 1;
    }
  }
  const kept: Record<string, PendingPermission[]> = {};
  for (const key of keys) {
    if (!drop.has(key)) kept[key] = map[key] ?? [];
  }
  return kept;
}

/** Janela da recarga ao vivo (086/T06): no máximo 1 recarga por projeto a cada 5 s. */
export const LIVE_RELOAD_MS = 5_000;

/**
 * Atividades que agendam a recarga ao vivo: ação de ferramenta/comando (nova linha em `tools.jsonl`)
 * e fim da chamada do agente (uso gravado em `usage.jsonl`; não há evento de uso próprio).
 */
const LIVE_RELOAD_KINDS: ReadonlySet<string> = new Set(["tool", "command", "end"]);

/** Soma 1 ao contador de revisão de cada projeto informado (ou de todos, se omitido). */
export function bumpRevisions(projectIds?: readonly string[]): void {
  appStore.setState((s) => {
    const ids = new Set<string>(projectIds ?? []);
    if (projectIds === undefined) {
      for (const id of Object.keys(s.projectStates)) ids.add(id);
      for (const project of s.config?.projects ?? []) ids.add(project.id);
      for (const project of s.projects) ids.add(project.id);
    }
    const revisions = { ...s.revisions };
    for (const id of ids) revisions[id] = (revisions[id] ?? 0) + 1;
    // Reconexão/lagged (todos os projetos): decisões podem ter se perdido.
    return projectIds === undefined ? { revisions, recoveries: dropAnalyzing(s.recoveries, null) } : { revisions };
  });
}

/** Fases que só mudam por `project_status` (nunca por rascunho, permissão ou tarefa). */
const SUSTAINED_PHASES: ReadonlySet<ProjectPhase> = new Set<ProjectPhase>(["blocked", "integrated"]);

/**
 * Redutor de eventos. Regras de status:
 * - `project_status` define a fase (`executing`, `integrated`, `blocked`, `idle`); fora de
 *   `executing`, limpa as permissões pendentes do projeto.
 * - `blocked` e `integrated` não são sobrescritos por rascunho nem por permissão, e
 *   `task_updated` fora de `BLOCKED` não os altera. Saem deles apenas por `project_status`.
 *   Atenção: `task_updated` com status `BLOCKED` PODE levar a fase a `blocked` (regra abaixo).
 * - `sdd_draft_updated` → `planning` (exceto durante execução ou aprovação pendente).
 * - `permission_requested` → `waiting_approval` (deduplica por `request_id`, com teto).
 * - `permission_resolved` → remove o pedido; sem pedidos restantes, sai de `waiting_approval`.
 * - `task_updated` com status `BLOCKED` → `blocked`; fora disso, retoma `executing`
 *   só quando estava aguardando permissão.
 * Eventos globais (`project_id` nulo) não alteram estado de projeto.
 */
export function reduceEvent(
  state: AppState,
  event: ApiEvent,
  nextId: number,
  seq?: number,
  at: number = liveNow(),
): Partial<AppState> {
  const patch = reduceEventBody(state, event, nextId, seq, at);
  const projectId = event.project_id;
  if (
    projectId !== null &&
    (event.event_type === "task_updated" ||
      event.event_type === "project_status" ||
      event.event_type === "sdd_draft_updated")
  ) {
    return {
      ...patch,
      revisions: { ...state.revisions, [projectId]: (state.revisions[projectId] ?? 0) + 1 },
    };
  }
  return patch;
}

/**
 * Texto de uma mensagem de agente. Causa fixa do provedor emitida pelo sistema (ex.:
 * `provider_http_400` ou `provider_bad_response (vazia)`) vira a mensagem pt-BR do catálogo, com
 * o detalhe entre parênteses; qualquer outro texto segue como veio.
 */
function agentMessageText(agent: string, text: string): string {
  if (agent !== "system") return text;
  return providerFailureText(text, null) ?? text;
}

function reduceEventBody(state: AppState, event: ApiEvent, nextId: number, seq: number | undefined, at: number): Partial<AppState> {
  const projectId = event.project_id;
  if (projectId === null) return {};
  const phase = state.phases[projectId] ?? "idle";

  switch (event.event_type) {
    case "project_status": {
      const status = event.payload.status;
      if (status !== "idle" && status !== "executing" && status !== "integrated" && status !== "blocked") {
        return {};
      }
      const patch: Partial<AppState> = { phases: { ...state.phases, [projectId]: status } };
      // Spec 114 T04: marca o início ou a retomada da execução (o motivo da parada vem depois dele).
      if (status === "executing") patch.runStarts = { ...state.runStarts, [projectId]: { seq: seq ?? 0, at } };
      if (state.liveBlocked[projectId]) {
        const liveBlocked = { ...state.liveBlocked };
        delete liveBlocked[projectId];
        patch.liveBlocked = liveBlocked;
      }
      if (status === "blocked") {
        const p = event.payload;
        const kind = "kind" in p ? p.kind : undefined;
        const blockKinds = { ...state.blockKinds };
        if (kind === undefined || !("task_id" in p)) delete blockKinds[projectId];
        else blockKinds[projectId] = { taskId: p.task_id, kind };
        patch.blockKinds = blockKinds;
      } else if (status === "executing" || status === "integrated") {
        const blockKinds = { ...state.blockKinds };
        delete blockKinds[projectId];
        patch.blockKinds = blockKinds;
      }
      // Status terminal do run (idle/blocked/integrated): análise sem decisão não fica presa.
      if (status !== "executing") patch.recoveries = dropAnalyzing(state.recoveries, projectId);
      if (status !== "executing") {
        // Pendentes encerrados ficam na conversa como resolvidos, sem decisão registrada.
        const pendingNow = state.permissions[projectId] ?? [];
        if (pendingNow.length > 0) {
          const now = liveNow();
          patch.resolvedPermissions = withResolved(
            state.resolvedPermissions,
            projectId,
            pendingNow.map((p) => toResolved(p, null, undefined, now)),
          );
        }
        patch.permissions = withQueue(state.permissions, projectId, []);
      }
      return patch;
    }
    case "sdd_draft_updated": {
      if (phase === "executing" || phase === "waiting_approval" || SUSTAINED_PHASES.has(phase)) return {};
      return { phases: { ...state.phases, [projectId]: "planning" } };
    }
    case "permission_requested": {
      const payload = event.payload;
      // Pedido já resolvido nesta sessão (replay do SSE): não volta a aparecer como pendente.
      if ((state.resolvedPermissions[projectId] ?? []).some((r) => r.requestId === payload.request_id)) return {};
      const list = state.permissions[projectId] ?? [];
      const index = list.findIndex((p) => p.requestId === payload.request_id);
      const req: PendingPermission = {
        requestId: payload.request_id,
        projectId,
        taskId: payload.task_id,
        agent: payload.agent,
        command: payload.command,
        alwaysAllowed: payload.always_allowed,
        alwaysDeniedCode: payload.always_denied_code,
        kind: payload.kind,
        reason: payload.reason,
        diff: payload.diff,
        // Mantém o instante da primeira chegada quando o mesmo pedido chega de novo.
        at: index === -1 ? liveNow() : (list[index]?.at ?? liveNow()),
      };
      const deduped = index === -1 ? [...list, req] : list.map((p, i) => (i === index ? req : p));
      const patch: Partial<AppState> = {
        permissions: capProjectQueues(withQueue(state.permissions, projectId, capPermissions(deduped))),
      };
      if (!SUSTAINED_PHASES.has(phase)) {
        patch.phases = { ...state.phases, [projectId]: "waiting_approval" };
      }
      return patch;
    }
    case "permission_resolved": {
      const list = state.permissions[projectId] ?? [];
      const hit = list.find((p) => p.requestId === event.payload.request_id);
      if (hit === undefined) return {};
      const rest = list.filter((p) => p.requestId !== event.payload.request_id);
      const patch: Partial<AppState> = {
        permissions: withQueue(state.permissions, projectId, rest),
        resolvedPermissions: withResolved(state.resolvedPermissions, projectId, [
          toResolved(hit, event.payload.applied, undefined, liveNow()),
        ]),
      };
      if (rest.length === 0 && phase === "waiting_approval") {
        const phases = { ...state.phases };
        delete phases[projectId];
        patch.phases = phases;
      }
      return patch;
    }
    case "recovery_started": {
      const { spec_id, task_id, cycle } = event.payload;
      return {
        recoveries: {
          ...state.recoveries,
          [recoveryKey(projectId, spec_id, task_id)]: { phase: "analyzing", cycle },
        },
      };
    }
    case "recovery_decided": {
      const { spec_id, task_id, cycle, decision, options, options_analysis } = event.payload;
      const entry: RecoveryEntry = { phase: decision === "RETRY" ? "retry" : "escalate", cycle };
      if (decision === "ESCALATE" && options !== undefined && options.length > 0 && options_analysis !== undefined) {
        entry.options = options;
        entry.options_analysis = options_analysis;
      }
      return {
        recoveries: {
          ...state.recoveries,
          [recoveryKey(projectId, spec_id, task_id)]: entry,
        },
      };
    }
    case "task_updated": {
      let recoveries: Record<string, RecoveryEntry> | undefined;
      const rKey = recoveryKey(projectId, event.payload.spec_id, event.payload.task_id);
      const rEntry = state.recoveries[rKey];
      if (rEntry?.phase === "retry" && event.payload.status !== "TODO" && event.payload.status !== "IN_PROGRESS") {
        recoveries = { ...state.recoveries };
        delete recoveries[rKey];
      } else if (rEntry?.phase === "analyzing" && event.payload.status !== "BLOCKED") {
        recoveries = dropAnalyzing(state.recoveries, projectId, rKey);
      }
      if (event.payload.status === "BLOCKED") {
        return {
          phases: { ...state.phases, [projectId]: "blocked" },
          liveBlocked: { ...state.liveBlocked, [projectId]: true },
          ...(recoveries ? { recoveries } : {}),
        };
      }
      // Run seguiu após recuperação (RETRY/REVISE): qualquer avanço tira a fase de `blocked`.
      if (phase === "blocked" && state.liveBlocked[projectId]) {
        const liveBlocked = { ...state.liveBlocked };
        delete liveBlocked[projectId];
        return { phases: { ...state.phases, [projectId]: "executing" }, liveBlocked, ...(recoveries ? { recoveries } : {}) };
      }
      if (phase === "waiting_approval") {
        return { phases: { ...state.phases, [projectId]: "executing" }, ...(recoveries ? { recoveries } : {}) };
      }
      return recoveries ? { recoveries } : {};
    }
    case "agent_message": {
      const payload = event.payload;
      const dialogSeq = payload.seq ?? undefined;
      const current = state.messages[projectId] ?? [];
      // A spec vem do servidor (destino da mensagem). Sem ela, a linha fica sem dono: não se deduz a spec.
      const payloadSpec = typeof payload.spec_id === "string" ? payload.spec_id : undefined;
      const activeSpec = activeSpecOf(state, projectId);
      // Mesma linha (mesma spec e seq) já presente, da hidratação ou de um SSE repetido: não duplica.
      if (
        dialogSeq !== undefined &&
        current.some((m) => m.role === "agent" && m.dialogSeq !== undefined && dialogKey(m.specId, m.dialogSeq) === dialogKey(payloadSpec, dialogSeq))
      ) {
        return {};
      }
      const message: ChatMessage = {
        id: nextId,
        role: "agent",
        agent: payload.agent,
        text: agentMessageText(payload.agent, payload.text),
        at,
        ...(seq === undefined ? {} : { seq }),
        ...(dialogSeq === undefined ? {} : { dialogSeq }),
        ...(payloadSpec === undefined
          ? { unscoped: true, ...(activeSpec === undefined ? {} : { arrivedUnderSpec: activeSpec }) }
          : { specId: payloadSpec }),
        ...(payload.task_id ? { taskId: payload.task_id } : {}),
        ...(typeof payload.attempt === "number" ? { attempt: payload.attempt } : {}),
        ...codeFields(payload.code, payload.params),
      };
      const next = [...current, message];
      const outOfOrder =
        dialogSeq !== undefined &&
        current.some((m) => m.role === "agent" && m.dialogSeq !== undefined && m.dialogSeq > dialogSeq);
      return { messages: { ...state.messages, [projectId]: capMessages(outOfOrder ? orderDialog(next) : next) } };
    }
    default:
      return {};
  }
}

/** Reordena o diálogo dos agentes dentro da lista (as demais mensagens mantêm a ordem). */
function orderDialog(list: readonly ChatMessage[]): ChatMessage[] {
  const others = list.filter((m) => m.role !== "agent");
  const agents = list.filter((m) => m.role === "agent");
  return [...others, ...sortAgentRows(agents)];
}

/** Cria uma instância isolada do store (usada no app e nos testes). */
export function createAppStore(initial: Partial<AppState> = {}): AppStore {
  let state: AppState = { ...initialState, ...initial };
  const listeners = new Set<Listener>();
  let counter = 0;
  /** Contador de chegadas ao vivo (`ChatMessage.seq`); não é afetado pelo teto nem pela hidratação. */
  let liveSeq = 0;

  const notify = (): void => {
    for (const listener of listeners) listener();
  };

  const setState: AppStore["setState"] = (patch) => {
    const partial = typeof patch === "function" ? patch(state) : patch;
    state = { ...state, ...partial };
    notify();
  };

  const endedTurns: string[] = [];
  /** Recarga ao vivo agendada por projeto: enquanto pendente, novas atividades não agendam outra. */
  const liveTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const scheduleLiveReload = (projectId: string): void => {
    if (liveTimers.has(projectId)) return;
    liveTimers.set(
      projectId,
      setTimeout(() => {
        liveTimers.delete(projectId);
        if (isProjectRemoved(state, projectId)) return;
        setState((s) => ({ liveRevisions: { ...s.liveRevisions, [projectId]: (s.liveRevisions[projectId] ?? 0) + 1 } }));
      }, LIVE_RELOAD_MS),
    );
  };
  const cancelLiveReload = (projectId: string): void => {
    const timer = liveTimers.get(projectId);
    if (timer !== undefined) clearTimeout(timer);
    liveTimers.delete(projectId);
  };
  const api: AppStore = {
    getState: () => state,
    setState,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    applyEvent(event) {
      counter += 1;
      if (event.event_type === "project_removed") {
        api.forgetProject(event.payload.project_id);
        return;
      }
      if (event.project_id !== null && isProjectRemoved(state, event.project_id)) return;
      if (event.event_type === "report_ready" || event.event_type === "report_failed") {
        // Relatório: sinal para a Conversa e a janela Relatório; o estado do projeto recarrega (`report_pending`).
        const projectId = event.project_id;
        if (projectId !== null) {
          const signal: ReportSignal =
            event.event_type === "report_ready"
              ? { specId: event.payload.spec_id, kind: "ready" }
              : { specId: event.payload.spec_id, kind: "failed", code: event.payload.code };
          // Relatório terminou (pronto ou falhou): encerra a bolha "pensando" do Arquiteto sem tarefa.
          api.clearArchitectActivity(projectId);
          setState((s) => ({
            reportSignals: { ...s.reportSignals, [reportKey(projectId, signal.specId)]: signal },
            revisions: { ...s.revisions, [projectId]: (s.revisions[projectId] ?? 0) + 1 },
          }));
        }
        return;
      }
      if (event.event_type === "finalization_done") {
        // Vaga pós-integração liberada: recarrega o estado (`report_pending` cai) sem refresh.
        const projectId = event.project_id;
        if (projectId !== null) {
          api.clearArchitectActivity(projectId);
          setState((s) => ({ revisions: { ...s.revisions, [projectId]: (s.revisions[projectId] ?? 0) + 1 } }));
        }
        return;
      }
      if (event.event_type === "app_started" || event.event_type === "app_exited") {
        const projectId = event.project_id;
        if (projectId !== null) {
          const signal: AppSignal =
            event.event_type === "app_started"
              ? { kind: "started", specId: event.payload.spec_id, url: event.payload.url }
              : { kind: "exited", code: event.payload.code, reason: event.payload.reason };
          setState((s) => ({ appSignals: { ...s.appSignals, [projectId]: signal } }));
        }
        return;
      }
      if (event.event_type === "chat_turn_started" && event.project_id !== null) {
        turnStartClock += 1;
        api.beginArchitectTurn(event.project_id, {
          turnId: event.payload.turn_id,
          startedAt: event.payload.started_at,
          ...(event.payload.user_text === undefined ? {} : { userText: event.payload.user_text }),
        });
        return;
      }
      if (event.event_type === "chat_turn_ended" && event.project_id !== null) {
        const projectId = event.project_id;
        api.endArchitectTurn(projectId, event.payload.turn_id);
        api.clearArchitectActivity(projectId);
        // Fim do turno: recarrega o estado do projeto (rascunho gravado no turno, revisão do plano
        // que começa agora), sem depender do POST do chat nem de F5 (spec 094, achado 8).
        setState((s) => ({ revisions: { ...s.revisions, [projectId]: (s.revisions[projectId] ?? 0) + 1 } }));
        return;
      }
      if (event.event_type === "agent_activity") {
        const projectId = event.project_id;
        if (projectId === null) return;
        const payload = event.payload;
        setState((s) => ({
          activities: {
            ...s.activities,
            [projectId]: reduceActivity(pruneActivities(s.activities[projectId] ?? [], Date.now()), payload, Date.now()),
          },
        }));
        if (LIVE_RELOAD_KINDS.has(payload.kind)) scheduleLiveReload(projectId);
        return;
      }
      if (event.event_type === "agent_message" && event.project_id !== null) {
        // A mensagem final do agente/tarefa substitui a bolha temporária.
        const { agent, task_id } = event.payload;
        const key = activityKey(agent, typeof task_id === "string" ? task_id : undefined);
        const list = state.activities[event.project_id];
        if (list?.some((a) => a.key === key) === true) {
          const projectId = event.project_id;
          setState((s) => ({
            activities: { ...s.activities, [projectId]: (s.activities[projectId] ?? []).filter((a) => a.key !== key) },
          }));
        }
      }
      if (event.event_type === "project_status" && event.payload.status !== "executing" && event.project_id !== null) {
        // Run terminou/pausou: nenhuma atividade de agente de execução fica presa.
        const projectId = event.project_id;
        if ((state.activities[projectId] ?? []).length > 0) {
          setState((s) => ({ activities: { ...s.activities, [projectId]: [] } }));
        }
      }
      if (event.event_type === "permission_requested" && event.project_id !== null) {
        arrivalClock += 1;
        arrivals.set(arrivalKey(event.project_id, event.payload.request_id), arrivalClock);
      }
      if (event.event_type === "permission_resolved" && event.project_id !== null) {
        arrivals.delete(arrivalKey(event.project_id, event.payload.request_id));
      }
      if (
        event.event_type === "agent_message" &&
        event.payload.agent === "architect" &&
        event.project_id !== null &&
        // Análise de recuperação (com tarefa e spec) é diálogo da tarefa, não resposta do chat.
        !(typeof event.payload.task_id === "string" && typeof event.payload.spec_id === "string")
      ) {
        // Resposta do Arquiteto: deduplicada pelo turn_id (ver `showArchitectReply`).
        api.showArchitectReply(event.project_id, event.payload.text, event.payload.turn_id);
        return;
      }
      liveSeq += 1;
      const patch = reduceEvent(state, event, counter, liveSeq, liveNow());
      if (Object.keys(patch).length > 0) setState(patch);
    },
    appendMessage(projectId, message) {
      counter += 1;
      const id = counter;
      if (isProjectRemoved(state, projectId)) return id;
      const current = state.messages[projectId] ?? [];
      liveSeq += 1;
      const next = capMessages([...current, { origin: "live", ...message, id, at: liveNow(), seq: liveSeq }]);
      setState({ messages: { ...state.messages, [projectId]: next } });
      return id;
    },
    confirmUserMessage(projectId, messageId, turnId) {
      if (isProjectRemoved(state, projectId)) return;
      const current = state.messages[projectId] ?? [];
      setState({
        messages: {
          ...state.messages,
          [projectId]: current.map((m) => (m.id === messageId ? { ...m, confirmed: true, turnId } : m)),
        },
      });
    },
    setUserMessageUnsent(projectId, messageId, notice) {
      if (isProjectRemoved(state, projectId)) return;
      const current = state.messages[projectId] ?? [];
      setState({
        messages: {
          ...state.messages,
          [projectId]: current.map((m) => {
            if (m.id !== messageId) return m;
            const { unsent: _drop, ...rest } = m;
            return notice === null ? { ...rest, inFlight: true } : { ...rest, unsent: notice, inFlight: false };
          }),
        },
      });
    },
    settleUserMessage(projectId, messageId) {
      const current = state.messages[projectId] ?? [];
      if (!current.some((m) => m.id === messageId && m.inFlight === true)) return;
      setState({
        messages: { ...state.messages, [projectId]: current.map((m) => (m.id === messageId ? { ...m, inFlight: false } : m)) },
      });
    },
    beginArchitectTurn(projectId, turn) {
      if (isProjectRemoved(state, projectId)) return;
      if (endedTurns.includes(turn.turnId)) return;
      const current = state.messages[projectId] ?? [];
      let messages = current;
      const text = turn.userText ?? "";
      if (text !== "" && !current.some((m) => m.turnId === turn.turnId)) {
        const inFlight = current.find((m) => m.role === "user" && m.inFlight === true && m.turnId === undefined);
        if (inFlight !== undefined) {
          // Esta aba enviou: a mensagem em voo é a deste turno (um turno por projeto).
          messages = current.map((m) => (m.id === inFlight.id ? { ...m, turnId: turn.turnId } : m));
        } else {
          counter += 1;
          liveSeq += 1;
          messages = capMessages([
            ...current,
            { id: counter, role: "user", text, at: liveNow(), origin: "live", confirmed: true, turnId: turn.turnId, seq: liveSeq },
          ]);
        }
      }
      const known = state.architectTurns[projectId];
      if (messages === current && known !== undefined && known.turnId === turn.turnId) return;
      setState({
        messages: messages === current ? state.messages : { ...state.messages, [projectId]: messages },
        architectTurns: { ...state.architectTurns, [projectId]: { turnId: turn.turnId, startedAt: turn.startedAt } },
      });
    },
    endArchitectTurn(projectId, turnId) {
      if (turnId !== undefined && !endedTurns.includes(turnId)) {
        endedTurns.push(turnId);
        if (endedTurns.length > MAX_ENDED_TURNS) endedTurns.shift();
      }
      const known = state.architectTurns[projectId];
      if (known === undefined || (turnId !== undefined && known.turnId !== turnId)) return;
      const { [projectId]: _gone, ...rest } = state.architectTurns;
      setState({ architectTurns: rest });
    },
    syncArchitectTurn(projectId, turn, clockAtRequest) {
      if (turn === null) {
        // Snapshot anterior a um `chat_turn_started` já aplicado é obsoleto: não apaga o turno novo.
        if (clockAtRequest !== undefined && clockAtRequest !== turnStartClock) return;
        api.endArchitectTurn(projectId);
        return;
      }
      api.beginArchitectTurn(projectId, { turnId: turn.turn_id, startedAt: turn.started_at, userText: turn.user_text });
    },
    mergeActivities(projectId, items, now) {
      const base = pruneActivities(state.activities[projectId] ?? [], now);
      setState({ activities: { ...state.activities, [projectId]: mergeServerActivities(base, items, now) } });
    },
    pruneActivities(projectId, now) {
      const list = state.activities[projectId];
      if (list === undefined) return;
      const kept = pruneActivities(list, now);
      if (kept.length === list.length && kept.every((a, i) => a === list[i])) return;
      setState({ activities: { ...state.activities, [projectId]: kept } });
    },
    clearArchitectActivity(projectId) {
      const list = state.activities[projectId];
      if (list === undefined || !list.some((a) => a.agent === "architect" && a.taskId === undefined)) return;
      setState({
        activities: { ...state.activities, [projectId]: list.filter((a) => !(a.agent === "architect" && a.taskId === undefined)) },
      });
    },
    showArchitectReply(projectId, text, turnId) {
      if (isProjectRemoved(state, projectId)) return false;
      api.clearArchitectActivity(projectId);
      if (turnId !== undefined) api.endArchitectTurn(projectId, turnId);
      const seen = state.shownTurns[projectId] ?? [];
      if (turnId !== undefined && seen.includes(turnId)) return false;
      const current = state.messages[projectId] ?? [];
      counter += 1;
      liveSeq += 1;
      const message: ChatMessage = {
        id: counter,
        role: "architect",
        agent: "architect",
        text,
        at: liveNow(),
        origin: "live",
        seq: liveSeq,
        ...(turnId === undefined ? {} : { turnId }),
      };
      setState({
        messages: { ...state.messages, [projectId]: capMessages([...current, message]) },
        shownTurns:
          turnId === undefined
            ? state.shownTurns
            : { ...state.shownTurns, [projectId]: capTurns([...seen, turnId]) },
      });
      return true;
    },
    hydrateHistory(projectId, turns) {
      // Lista vazia é ambígua (falha transitória do servidor devolve 200 []): não altera a conversa.
      if (turns.length === 0) return;
      if (isProjectRemoved(state, projectId)) return;
      // O histórico do servidor substitui as linhas `history` anteriores (não há reconciliação
      // por texto). Das mensagens ao vivo, só sobrevive o que o histórico ainda não contém.
      const persisted = new Set<string>();
      for (const turn of turns) {
        if (turn.turn_id !== undefined) persisted.add(turn.turn_id);
      }
      // Turnos com a resposta do Arquiteto já gravada. O turno em andamento (só a linha do usuário)
      // não entra: a resposta dele ainda chega por SSE ou pelo POST e precisa aparecer (114/T03).
      const answered = new Set<string>();
      for (const turn of turns) {
        if (turn.role === "architect" && turn.turn_id !== undefined) answered.add(turn.turn_id);
      }
      const current = state.messages[projectId] ?? [];
      const echo = persistedEcho(current, turns, state.shownTurns[projectId] ?? []);
      // O diálogo dos agentes (role agent) tem carga própria (`hydrateDialog`): não é tocado aqui.
      const live = current.filter((m) => {
        if (m.role === "agent") return true;
        if (m.origin === "history") return false;
        if (m.role === "user") {
          // O servidor grava a mensagem do usuário no início do turno: com a linha persistida, o eco
          // do composer sai (a linha do histórico a substitui), mesmo com o POST ainda em andamento.
          if (m.id === echo) return false;
          if (m.turnId !== undefined && persisted.has(m.turnId)) return false;
          // Em voo cujo turno ainda não está no histórico, ou sem turn_id: mantém.
          return true;
        }
        if (m.role === "architect") {
          return m.turnId === undefined || !answered.has(m.turnId);
        }
        return true;
      });
      // As linhas do histórico nunca são cortadas pelo teto da carga: o corte recai só sobre as ao vivo.
      const rows: ChatMessage[] = turns.slice(Math.max(0, turns.length - MAX_MESSAGES_PER_PROJECT)).map((turn) => {
        counter += 1;
        return {
          id: counter,
          role: turn.role,
          agent: turn.role === "architect" ? "architect" : undefined,
          text: turn.text,
          at: turn.ts * 1000,
          origin: "history",
          ...(turn.turn_id === undefined ? {} : { turnId: turn.turn_id }),
          ...(turn.attachments === undefined || turn.attachments.length === 0 ? {} : { attachments: turn.attachments }),
        };
      });
      const chatLive = live.filter((m) => m.role !== "agent");
      const dialog = live.filter((m) => m.role === "agent");
      const room = Math.max(0, MAX_MESSAGES_PER_PROJECT - rows.length);
      const keptLive = chatLive.slice(Math.max(0, chatLive.length - room));
      // Exibidos: turnos respondidos no histórico + respostas ao vivo mantidas.
      const ids = new Set<string>(answered);
      for (const m of keptLive) {
        if (m.role === "architect" && m.turnId !== undefined) ids.add(m.turnId);
      }
      setState({
        messages: { ...state.messages, [projectId]: [...rows, ...keptLive, ...dialog] },
        shownTurns: { ...state.shownTurns, [projectId]: capTurns([...ids]) },
      });
    },
    hydrateDialog(projectId, specId, dialogRows) {
      if (isProjectRemoved(state, projectId)) return;
      // Fonte da verdade da spec: o maior seq lido (0 sem linhas). Linhas de outra spec ficam intactas.
      let lastSeq = 0;
      for (const row of dialogRows) if (row.seq > lastSeq) lastSeq = row.seq;
      const serverSeqs = new Set<number>(dialogRows.map((row) => row.seq));
      const current = state.messages[projectId] ?? [];
      const others = current.filter((m) => m.role !== "agent");
      const kept = current.filter((m) => {
        if (m.role !== "agent") return false;
        if (m.unscoped === true) return false; // sem spec do servidor: a primeira hidratação a descarta
        if (!inSpec(m, specId)) return true; // outra spec: preservada na memória, não exibida
        if (m.dialogSeq === undefined) return m.origin !== "history"; // ao vivo ainda não gravado
        // Mesma spec, posterior ao snapshot e ausente dele: sobrevive, qualquer que seja a origem.
        return m.dialogSeq > lastSeq && !serverSeqs.has(m.dialogSeq);
      });
      const fresh = dialogRows.map((row) => {
        counter += 1;
        return dialogRowToMessage(counter, row, specId);
      });
      setState({
        messages: { ...state.messages, [projectId]: capMessages([...others, ...sortAgentRows([...kept, ...fresh])]) },
      });
    },
    mergeDialog(projectId, specId, dialogRows) {
      if (isProjectRemoved(state, projectId) || dialogRows.length === 0) return;
      const current = state.messages[projectId] ?? [];
      const known = new Set<string>();
      for (const m of current) if (m.role === "agent" && m.dialogSeq !== undefined) known.add(dialogKey(m.specId, m.dialogSeq));
      const fresh: ChatMessage[] = [];
      for (const row of dialogRows) {
        const key = dialogKey(specId, row.seq);
        if (known.has(key)) continue;
        known.add(key);
        counter += 1;
        fresh.push(dialogRowToMessage(counter, row, specId));
      }
      if (fresh.length === 0) return;
      const others = current.filter((m) => m.role !== "agent");
      const agents = current.filter((m) => m.role === "agent");
      setState({
        messages: { ...state.messages, [projectId]: capMessages([...others, ...sortAgentRows([...agents, ...fresh])]) },
      });
    },
    removeMessage(projectId, messageId) {
      const current = state.messages[projectId];
      if (current === undefined || !current.some((m) => m.id === messageId)) return;
      setState({ messages: { ...state.messages, [projectId]: current.filter((m) => m.id !== messageId) } });
    },
    clearPermission(projectId, requestId) {
      if (isProjectRemoved(state, projectId)) return;
      const list = state.permissions[projectId] ?? [];
      arrivals.delete(arrivalKey(projectId, requestId));
      setState({
        permissions: withQueue(
          state.permissions,
          projectId,
          list.filter((p) => p.requestId !== requestId),
        ),
      });
    },
    settlePermission(projectId, requestId, applied, requested) {
      if (isProjectRemoved(state, projectId)) return;
      const list = state.permissions[projectId] ?? [];
      const hit = list.find((p) => p.requestId === requestId);
      if (hit === undefined) {
        // Já resolvido pelo SSE: só completa a escolha feita nesta janela, se faltava.
        const done = (state.resolvedPermissions[projectId] ?? []).find((r) => r.requestId === requestId);
        if (done !== undefined && requested !== undefined && done.requested === undefined) {
          setState({
            resolvedPermissions: withResolved(state.resolvedPermissions, projectId, [{ ...done, requested }]),
          });
        }
        return;
      }
      arrivals.delete(arrivalKey(projectId, requestId));
      setState({
        permissions: withQueue(
          state.permissions,
          projectId,
          list.filter((p) => p.requestId !== requestId),
        ),
        resolvedPermissions: withResolved(state.resolvedPermissions, projectId, [
          toResolved(hit, applied, requested, liveNow()),
        ]),
      });
    },
    markRunStarted(projectId) {
      liveSeq += 1;
      setState({ runStarts: { ...state.runStarts, [projectId]: { seq: liveSeq, at: liveNow() } } });
    },
    hydratePermissions(projectId, server, startedAt) {
      if (isProjectRemoved(state, projectId)) return;
      const local = state.permissions[projectId] ?? [];
      const resolvedIds = new Set((state.resolvedPermissions[projectId] ?? []).map((r) => r.requestId));
      // Snapshot que ainda lista um pedido já resolvido aqui: a resolução local vale (não duplica).
      const liveServer = server.filter((p) => !resolvedIds.has(p.request_id));
      const serverIds = new Set(liveServer.map((p) => p.request_id));
      const now = liveNow();
      const fromServer: PendingPermission[] = liveServer.map(
        (p) =>
          local.find((l) => l.requestId === p.request_id) ?? {
            requestId: p.request_id,
            projectId,
            taskId: p.task_id,
            agent: p.agent,
            command: p.command,
            alwaysAllowed: p.always_allowed,
            alwaysDeniedCode: p.always_denied_code,
            kind: p.kind,
            reason: p.reason,
            diff: p.diff,
            at: now,
          },
      );
      // Só some o que o servidor não tem E que já estava aqui antes do GET começar.
      const recent = local.filter(
        (l) =>
          !serverIds.has(l.requestId) && (arrivals.get(arrivalKey(projectId, l.requestId)) ?? 0) > startedAt,
      );
      const gone: PendingPermission[] = [];
      for (const l of local) {
        if (!serverIds.has(l.requestId) && !recent.includes(l)) {
          arrivals.delete(arrivalKey(projectId, l.requestId));
          gone.push(l);
        }
      }
      const list = capPermissions([...fromServer, ...recent]);
      const patch: Partial<AppState> = {
        permissions: capProjectQueues(withQueue(state.permissions, projectId, list)),
      };
      if (gone.length > 0) {
        // Sumiu do servidor sem evento aqui: fica na conversa como resolvido, sem decisão registrada.
        patch.resolvedPermissions = withResolved(
          state.resolvedPermissions,
          projectId,
          gone.map((p) => toResolved(p, null, undefined, now)),
        );
      }
      const phase = state.phases[projectId];
      if (list.length > 0 && !SUSTAINED_PHASES.has(phase ?? "idle")) {
        patch.phases = { ...state.phases, [projectId]: "waiting_approval" };
      } else if (list.length === 0 && phase === "waiting_approval") {
        const phases = { ...state.phases };
        delete phases[projectId];
        patch.phases = phases;
      }
      setState(patch);
    },
    forgetProject(projectId) {
      clearArrivals(projectId);
      cancelLiveReload(projectId);
      const without = <T,>(map: Record<string, T>): Record<string, T> => {
        const next = { ...map };
        delete next[projectId];
        return next;
      };
      const s = state;
      setState({
        config: s.config === null
          ? null
          : { ...s.config, projects: s.config.projects.filter((p) => p.id !== projectId) },
        projects: s.projects.filter((p) => p.id !== projectId),
        activeProjectId: s.activeProjectId === projectId ? null : s.activeProjectId,
        projectStates: without(s.projectStates),
        messages: without(s.messages),
        permissions: without(s.permissions),
        resolvedPermissions: without(s.resolvedPermissions),
        phases: without(s.phases),
        revisions: without(s.revisions),
        liveRevisions: without(s.liveRevisions),
        shownTurns: without(s.shownTurns),
        architectTurns: without(s.architectTurns),
        reportSignals: Object.fromEntries(Object.entries(s.reportSignals).filter(([key]) => !key.startsWith(`${projectId}:`))),
        appSignals: without(s.appSignals),
        recoveries: Object.fromEntries(
          Object.entries(s.recoveries).filter(([key]) => !key.startsWith(`["${projectId}",`)),
        ),
        blockKinds: without(s.blockKinds),
        liveBlocked: without(s.liveBlocked),
        runStarts: without(s.runStarts),
        activities: without(s.activities),
        removedProjectIds: { ...s.removedProjectIds, [projectId]: true },
      });
    },
    readmitProject(projectId) {
      if (!isProjectRemoved(state, projectId)) return;
      const next = { ...state.removedProjectIds };
      delete next[projectId];
      setState({ removedProjectIds: next });
    },
  };
  return api;
}

/** Store global da aplicação. */
export const appStore: AppStore = createAppStore();

/** Hook: lê uma fatia do estado global. O seletor deve devolver referências estáveis. */
export function useAppStore<T>(selector: (state: AppState) => T): T {
  return useSyncExternalStore(appStore.subscribe, () => selector(appStore.getState()));
}
