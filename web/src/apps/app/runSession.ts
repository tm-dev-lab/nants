/*
 * Estado da execução do app com FONTE ÚNICA por projeto e especificação (spec 095, T03, C-03),
 * compartilhado pelas janelas App (o navegador) e Execução (o controle e o console).
 *
 * Desenho:
 * - uma sessão por chave `projectId:specId` (`sessionKey`), num mapa do módulo, lida pelas
 *   janelas com `useSyncExternalStore` (`useRunSession`). A sessão nasce na primeira janela que a
 *   monta e some quando a última a desmonta: remontar uma janela (outra chave) descarta as
 *   respostas atrasadas da sessão antiga (`alive`);
 * - a sessão reúne fase (com as portas), registro (logs), problema (com a prontidão do 412),
 *   confirmação pendente, parada em curso, avisos de porta e a partida atual;
 * - estado do servidor (`GET .../app`): UM pedido por vez. Cada janela VISÍVEL se registra como
 *   leitora (`statusViewers`); os pedidos (carga inicial, eventos, Recarregar, releitura do
 *   veredito) só aumentam `wanted`, e `pumpStatus` faz no máximo um GET em voo, só com alguma
 *   leitora visível (montada minimizada, nenhuma requisição);
 * - registro (`GET .../app/logs`): só com a Execução visível (`logViewers`), uma leitura ao ficar
 *   visível e a cada `LOGS_POLL_MS` enquanto o app está no ar. O registro e os detalhes da falha
 *   chegam limpos de sequências ANSI (`cleanComposeLog`, spec 113): a tela e o "Copiar registro"
 *   usam o mesmo texto puro;
 * - eventos `app_started`/`app_exited` (store global) são aplicados UMA vez por sessão;
 * - aviso "App no ar": na transição sem endereço → com endereço, uma vez por partida (`launch`,
 *   contada ao pedir a execução ou ao ver um `app_started` de outra aba), pelo `onLive` registrado
 *   pelas janelas (que chama `shell.showToast`). A carga inicial com o app já no ar não avisa.
 */
import { useCallback, useEffect, useRef, useSyncExternalStore } from "react";
import { getAppLogs, getAppStatus, startApp, stopApp, type ApiFailure, type AppNotice, type AppStatus } from "../../api/client";
import type { NetworkView } from "../../api/types";
import { appErrorMessage } from "../../copy";
import type { Readiness, ReadinessItem } from "../../desktop/api";
import { translateCode } from "../../i18n/serverCodes";
import { appStore } from "../../store";
import { isPrereqFailure, prereqItems, prereqMessage } from "../shared/readiness";
import { cleanComposeLog } from "../shared/terminalText";
import {
  EMBED_RETRY_LIMIT,
  EMBED_RETRY_MS,
  LOGS_POLL_MS,
  createStatusSequence,
  embedProbePending,
  firstPort,
  mergeStatus,
  phaseFromStatus,
  type RunPhase,
} from "./appModel";

interface RunProblem {
  /** Texto do momento da falha (reserva); a tela usa `runProblemText`, no idioma atual. */
  message: string;
  /** A falha da API (início ou parada), para traduzir na hora pelo `code` ou pelo `reason`. */
  failure?: ApiFailure;
  missing: readonly ReadinessItem[] | null;
  prereq: boolean;
}

/**
 * Texto do problema no idioma ativo (spec 102): as pendências do 412 refeitas a partir dos itens
 * guardados (os rótulos saem no idioma atual); erro com `code` traduzível (porta ocupada, faixa
 * esgotada) pelo dicionário; senão pelo `reason` e pelo status.
 */
export function runProblemText(problem: RunProblem): string {
  if (problem.prereq && problem.missing !== null) return prereqMessage(problem.missing);
  const failure = problem.failure;
  if (failure === undefined) return problem.message;
  if (failure.code !== undefined) {
    const text = translateCode("serverError", failure.code, failure.params);
    if (text !== null) return text;
  }
  return appErrorMessage(failure.status, failure.reason);
}

/** O que as janelas leem da sessão (imutável: cada mudança gera um objeto novo). */
interface RunSnapshot {
  projectId: string;
  /** Especificação concluída do espaço (a que "Iniciar" envia); `null` = nenhuma ainda. */
  specId: string | null;
  phase: RunPhase;
  logs: string | null;
  problem: RunProblem | null;
  /** Detalhe técnico da última falha (só a Execução mostra). */
  errorDetail: string | null;
  notices: readonly AppNotice[];
  /** Confirmação de início aberta (o diálogo fica na Execução). */
  confirming: boolean;
  stopping: boolean;
  /** Partida atual (0 = nenhuma vista nesta sessão). */
  launch: number;
  /** Releituras feitas com o veredito `embeddable` indeterminado (teto `EMBED_RETRY_LIMIT`). */
  embedRetries: number;
  /** URL da porta escolhida em "Abrir no App" (Execução); `null` = a primeira porta. */
  shownUrl: string | null;
  /** Porta do Nants, faixa de visualização e WSL da última resposta do app (spec 097); `null` = ainda não veio. */
  network: NetworkView | null;
}

/**
 * Host da página do Nants (095/T09): fora do loopback, App e Execução mostram a ajuda "porta pode
 * não estar liberada na rede". Os testes trocam o campo (o jsdom serve a página em localhost).
 */
export const reachEnvironment: { pageHostname: () => string } = {
  pageHostname: () => window.location.hostname,
};

/** Dependências do início (412 `prereq_missing`: os rótulos saem da prontidão relida). */
interface StartDeps {
  readiness: Readiness | null;
  reloadReadiness: () => Promise<Readiness | null>;
}

type AppSignal = NonNullable<ReturnType<typeof appStore.getState>["appSignals"][string]>;

interface Session {
  key: string;
  snap: RunSnapshot;
  listeners: Set<() => void>;
  mounts: number;
  alive: boolean;
  statusViewers: Set<symbol>;
  logViewers: Set<symbol>;
  wanted: number;
  done: number;
  inflight: boolean;
  sequence: ReturnType<typeof createStatusSequence>;
  busy: boolean;
  lastSignal: AppSignal | null;
  unsubscribe: (() => void) | null;
  logTimer: ReturnType<typeof setInterval> | null;
  logMode: string;
  probeTimer: ReturnType<typeof setTimeout> | null;
  announced: number;
  liveHandlers: Map<symbol, () => void>;
}

const sessions = new Map<string, Session>();

function sessionKey(projectId: string, specId: string | null): string {
  return `${projectId}:${specId ?? ""}`;
}

function createSession(projectId: string, specId: string | null): Session {
  return {
    key: sessionKey(projectId, specId),
    snap: {
      projectId,
      specId,
      phase: { kind: "stopped" },
      logs: null,
      problem: null,
      errorDetail: null,
      notices: [],
      confirming: false,
      stopping: false,
      launch: 0,
      embedRetries: 0,
      shownUrl: null,
      network: null,
    },
    listeners: new Set(),
    mounts: 0,
    alive: true,
    statusViewers: new Set(),
    logViewers: new Set(),
    // A carga inicial: um pedido, feito quando alguma janela estiver visível.
    wanted: 1,
    done: 0,
    inflight: false,
    sequence: createStatusSequence(),
    busy: false,
    lastSignal: null,
    unsubscribe: null,
    logTimer: null,
    logMode: "off",
    probeTimer: null,
    announced: 0,
    liveHandlers: new Map(),
  };
}

/** Sessão da chave (criada vazia, sem efeitos, se ainda não existe). */
function sessionFor(projectId: string, specId: string | null): Session {
  const key = sessionKey(projectId, specId);
  const found = sessions.get(key);
  if (found !== undefined) return found;
  const created = createSession(projectId, specId);
  sessions.set(key, created);
  return created;
}

const hasUrl = (phase: RunPhase): boolean => firstPort(phase) !== null;

function update(s: Session, patch: Partial<RunSnapshot> | ((snap: RunSnapshot) => Partial<RunSnapshot>)): void {
  if (!s.alive) return;
  const before = s.snap;
  const changes = typeof patch === "function" ? patch(before) : patch;
  const next = { ...before, ...changes };
  const changed = (Object.keys(changes) as (keyof RunSnapshot)[]).some((k) => before[k] !== next[k]);
  if (!changed) return;
  s.snap = next;
  // Aviso "App no ar": sem endereço → com endereço, uma vez por partida.
  if (!hasUrl(before.phase) && hasUrl(next.phase) && next.launch > 0 && s.announced !== next.launch) {
    s.announced = next.launch;
    const handler = [...s.liveHandlers.values()].at(-1);
    handler?.();
  }
  for (const listener of [...s.listeners]) listener();
  syncLogs(s);
  syncProbe(s);
}

function sameNetwork(a: NetworkView | null, b: NetworkView): boolean {
  return (
    a !== null &&
    a.nants_port === b.nants_port &&
    a.wsl === b.wsl &&
    a.lan_view_ports?.start === b.lan_view_ports?.start &&
    a.lan_view_ports?.end === b.lan_view_ports?.end
  );
}

// --- Estado do servidor: um GET por vez, só com alguma janela visível ---

function applyStatus(s: Session, status: AppStatus, issued: number): void {
  if (!s.sequence.accept(issued)) return;
  const next = phaseFromStatus(status, s.snap.specId);
  update(s, (snap) => ({
    ...(next.kind === "running" ? { notices: status.notices } : {}),
    ...(status.network !== null ? { network: sameNetwork(snap.network, status.network) ? snap.network : status.network } : {}),
    phase: mergeStatus(snap.phase, next),
  }));
}

function pumpStatus(s: Session): void {
  if (!s.alive || s.inflight || s.statusViewers.size === 0 || s.done >= s.wanted) return;
  s.inflight = true;
  const wanted = s.wanted;
  const issued = s.sequence.issue();
  void getAppStatus(s.snap.projectId).then((r) => {
    s.inflight = false;
    if (!s.alive) return;
    s.done = Math.max(s.done, wanted);
    if (r.ok) applyStatus(s, r.data, issued);
    pumpStatus(s);
  });
}

function requestStatus(s: Session): void {
  s.wanted += 1;
  pumpStatus(s);
}

// --- Registro: só com a Execução visível ---

function refreshLogs(s: Session): void {
  if (!s.alive || s.logViewers.size === 0) return;
  void getAppLogs(s.snap.projectId).then((r) => {
    if (s.alive && r.ok) update(s, { logs: cleanComposeLog(r.data) });
  });
}

const watching = (phase: RunPhase): boolean => phase.kind === "running" || phase.kind === "other";

/** Liga, religa ou desliga a consulta do registro conforme as leitoras e a fase. */
function syncLogs(s: Session): void {
  const mode = !s.alive || s.logViewers.size === 0 ? "off" : watching(s.snap.phase) ? "poll" : "once";
  if (mode === s.logMode) return;
  s.logMode = mode;
  if (s.logTimer !== null) {
    clearInterval(s.logTimer);
    s.logTimer = null;
  }
  if (mode === "off") return;
  refreshLogs(s);
  if (mode === "poll") s.logTimer = setInterval(() => refreshLogs(s), LOGS_POLL_MS);
}

// --- Releitura do veredito `embeddable` indeterminado ---

function syncProbe(s: Session): void {
  const probing = s.alive && s.statusViewers.size > 0 && embedProbePending(s.snap.phase) && s.snap.embedRetries < EMBED_RETRY_LIMIT;
  if (!probing) {
    if (s.probeTimer !== null) {
      clearTimeout(s.probeTimer);
      s.probeTimer = null;
    }
    return;
  }
  if (s.probeTimer !== null) return;
  s.probeTimer = setTimeout(() => {
    s.probeTimer = null;
    if (!s.alive) return;
    update(s, (snap) => ({ embedRetries: snap.embedRetries + 1 }));
    requestStatus(s);
    syncProbe(s);
  }, EMBED_RETRY_MS);
}

// --- Eventos do app (uma vez por sessão) ---

function applySignal(s: Session, signal: AppSignal): void {
  s.sequence.markLocal();
  if (signal.kind === "started") {
    const specId = s.snap.specId;
    if (specId === null || signal.specId === specId) {
      update(s, (snap) => ({
        /*
         * O servidor publica o `app_started` ANTES de responder ao POST desta aba (095/T07, nota da
         * T03): com o POST pendente (`busy`) a fase segue "subindo" até a resposta trazer as portas;
         * senão o App passaria alguns milissegundos em "no ar sem endereço" (`no_address`).
         */
        phase:
          snap.phase.kind === "running" || (snap.phase.kind === "starting" && s.busy)
            ? snap.phase
            : { kind: "running", ports: [], embeddable: null },
        // Partida de outra aba (aqui ela já foi contada ao pedir).
        launch: snap.phase.kind === "starting" || s.busy ? snap.launch : snap.launch + 1,
      }));
    } else {
      update(s, { phase: { kind: "other" } });
    }
    // As portas publicadas (com o host desta página) e o `embeddable` vêm do estado do app.
    requestStatus(s);
  } else {
    update(s, (snap) => ({
      phase: snap.phase.kind === "stopped" ? snap.phase : { kind: "exited", code: signal.code, reason: signal.reason },
      stopping: false,
    }));
  }
  refreshLogs(s);
}

function attach(s: Session): void {
  s.mounts += 1;
  if (!s.alive) {
    // Remontagem da mesma sessão (efeitos refeitos): volta a valer.
    s.alive = true;
    if (!sessions.has(s.key)) sessions.set(s.key, s);
  }
  if (s.unsubscribe !== null) return;
  s.lastSignal = appStore.getState().appSignals[s.snap.projectId] ?? null;
  s.unsubscribe = appStore.subscribe(() => {
    const signal = appStore.getState().appSignals[s.snap.projectId] ?? null;
    if (signal === s.lastSignal) return;
    s.lastSignal = signal;
    if (signal !== null) applySignal(s, signal);
  });
}

function detach(s: Session): void {
  s.mounts -= 1;
  if (s.mounts > 0) return;
  s.alive = false;
  s.unsubscribe?.();
  s.unsubscribe = null;
  if (s.logTimer !== null) clearInterval(s.logTimer);
  if (s.probeTimer !== null) clearTimeout(s.probeTimer);
  s.logTimer = null;
  s.probeTimer = null;
  s.logMode = "off";
  if (sessions.get(s.key) === s) sessions.delete(s.key);
}

// --- Ações ---

function start(s: Session, deps: StartDeps): void {
  const specId = s.snap.specId;
  if (s.busy || specId === null) return;
  s.busy = true;
  s.sequence.markLocal();
  update(s, (snap) => ({
    confirming: false,
    embedRetries: 0,
    problem: null,
    errorDetail: null,
    logs: null,
    phase: { kind: "starting" },
    launch: snap.launch + 1,
    shownUrl: null,
  }));
  void startApp(s.snap.projectId, specId).then(async (r) => {
    s.busy = false;
    if (!s.alive) return;
    s.sequence.markLocal();
    if (!r.ok) {
      update(s, { phase: { kind: "stopped" } });
      if (isPrereqFailure(r.error)) {
        // Os itens com os rótulos da prontidão relida; o texto é refeito no idioma atual (`runProblemText`).
        const fresh = await deps.reloadReadiness();
        const missing = prereqItems(r.error, fresh ?? deps.readiness) ?? [];
        update(s, { problem: { message: prereqMessage(missing), missing, prereq: true } });
        return;
      }
      update(s, { problem: { message: appErrorMessage(r.error.status, r.error.reason), failure: r.error, missing: null, prereq: false }, errorDetail: typeof r.error.detail === "string" ? cleanComposeLog(r.error.detail) : null });
      return;
    }
    // O evento `app_started` pode já ter chegado; a resposta só completa o que faltar.
    const data = r.data;
    update(s, (snap) => {
      const ports = data?.ports ?? [];
      const embeddable = data?.embeddable ?? null;
      const p = snap.phase;
      const phase: RunPhase =
        p.kind === "starting" || (p.kind === "running" && p.ports.length === 0 && ports.length > 0) ? { kind: "running", ports, embeddable } : p;
      const network = data !== null && data.network !== null && !sameNetwork(snap.network, data.network) ? { network: data.network } : {};
      return { phase, ...(data !== null ? { notices: data.notices } : {}), ...network };
    });
    refreshLogs(s);
  });
}

function stop(s: Session): void {
  if (s.snap.stopping) return;
  update(s, { stopping: true, problem: null });
  void stopApp(s.snap.projectId).then((r) => {
    if (!s.alive) return;
    update(s, {
      stopping: false,
      ...(r.ok ? {} : { problem: { message: appErrorMessage(r.error.status, r.error.reason), failure: r.error, missing: null, prereq: false } }),
    });
  });
}

/** O app pode ser iniciado agora (parado ou encerrado, com especificação concluída). */
export function canStart(snap: RunSnapshot): boolean {
  return snap.phase.kind === "stopped" || snap.phase.kind === "exited";
}

interface RunActions {
  /** Abre a confirmação de início (o diálogo fica na Execução). */
  requestStart: () => void;
  cancelStart: () => void;
  /** "Abrir no App" (Execução): o App passa a mostrar esta porta (pela URL). */
  showPort: (url: string) => void;
  /** Só depois da confirmação humana. */
  confirmStart: (deps: StartDeps) => void;
  stop: () => void;
  /** Relê o estado do servidor (Recarregar); zera as releituras do veredito. */
  reload: () => void;
}

interface UseRunOptions {
  /** A janela está visível (só então ela conta como leitora do estado). */
  visible: boolean;
  /** Execução visível: lê o registro. */
  logs?: boolean;
  /** Chamado quando o app passa a responder numa partida (o aviso "App no ar"). */
  onLive?: () => void;
}

/**
 * Sessão de execução do projeto e especificação para uma janela. As duas janelas montam com
 * `key = projectId:specId` (remontagem ao trocar), e esta chave escolhe a sessão.
 */
export function useRunSession(projectId: string, specId: string | null, options: UseRunOptions): { snap: RunSnapshot; actions: RunActions } {
  const session = sessionFor(projectId, specId);
  const subscribe = useCallback(
    (listener: () => void) => {
      session.listeners.add(listener);
      return () => {
        session.listeners.delete(listener);
      };
    },
    [session],
  );
  const snap = useSyncExternalStore(subscribe, () => session.snap);

  useEffect(() => {
    attach(session);
    return () => detach(session);
  }, [session]);

  const { visible, logs = false } = options;
  useEffect(() => {
    if (!visible) return undefined;
    const token = Symbol("viewer");
    session.statusViewers.add(token);
    if (logs) session.logViewers.add(token);
    pumpStatus(session);
    syncLogs(session);
    syncProbe(session);
    return () => {
      session.statusViewers.delete(token);
      session.logViewers.delete(token);
      syncLogs(session);
      syncProbe(session);
    };
  }, [session, visible, logs]);

  const liveRef = useRef(options.onLive);
  liveRef.current = options.onLive;
  useEffect(() => {
    const token = Symbol("live");
    session.liveHandlers.set(token, () => liveRef.current?.());
    return () => {
      session.liveHandlers.delete(token);
    };
  }, [session]);

  const actionsRef = useRef<{ session: Session; actions: RunActions } | null>(null);
  if (actionsRef.current?.session !== session) {
    const s = session;
    actionsRef.current = {
      session: s,
      actions: {
        requestStart: () => {
          if (canStart(s.snap) && s.snap.specId !== null) update(s, { confirming: true });
        },
        cancelStart: () => update(s, { confirming: false }),
        showPort: (url) => update(s, { shownUrl: url }),
        confirmStart: (deps) => start(s, deps),
        stop: () => stop(s),
        reload: () => {
          update(s, { embedRetries: 0 });
          requestStatus(s);
        },

      },
    };
  }
  return { snap, actions: actionsRef.current.actions };
}

/** Testes: quantas sessões vivas há (a última janela a desmontar descarta a sessão). */
export function liveSessionCount(): number {
  return sessions.size;
}
