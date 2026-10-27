/*
 * Estado do Console do projeto (spec 130 T03 e T05), UM por projeto, guardado no módulo: trocar
 * entre o App e o Console (ou remontar a janela) não perde a saída já mostrada nesta aba.
 *
 * - leitura (`GET .../app/exec`): o comando em curso, o último resultado, o histórico e o limite de
 *   tempo, ao montar visível e depois de cada fim de comando;
 * - eventos `app_exec_started`, `app_exec_output` e `app_exec_finished` (`api/consoleEvents.ts`):
 *   a transcrição desta aba, com a saída ao vivo (pedaço repetido pelo `seq` é ignorado);
 * - rodar: `POST .../app/exec` (202) só depois da confirmação humana na tela; parar:
 *   `POST .../app/exec/stop`.
 * Todo texto vem redigido pelo servidor e é mostrado como texto React.
 */
import { useCallback, useEffect, useRef, useSyncExternalStore } from "react";
import {
  getConsole,
  runConsoleCommand,
  stopConsoleCommand,
  toExecView,
  type ApiFailure,
  type ExecHistoryEntry,
  type ExecView,
} from "../../api/client";
import { subscribeConsoleEvents, type ConsoleEvent } from "../../api/consoleEvents";

/** Teto de comandos na transcrição desta aba. */
const MAX_TRANSCRIPT = 20;
/** Teto de texto por comando na transcrição (caracteres, contados do fim). */
const MAX_ENTRY_TEXT = 64 * 1024;

/** Um comando na transcrição: o texto ao vivo e, no fim, o resultado. */
export interface TranscriptEntry {
  execId: string;
  command: string;
  text: string;
  /** Último `seq` aplicado (pedaço repetido ou atrasado é ignorado). */
  seq: number;
  /** A saída passou do teto do servidor. */
  truncated: boolean;
  /** Resultado; `null` enquanto roda. */
  result: ExecView | null;
}

interface ConsoleSnapshot {
  loaded: boolean;
  active: ExecView | null;
  last: ExecView | null;
  history: readonly ExecHistoryEntry[];
  timeoutSecs: number;
  transcript: readonly TranscriptEntry[];
  /** POST de rodar em voo. */
  posting: boolean;
  stopping: boolean;
  /** Falha de rodar ou parar (pelo `code`, com o próximo passo). */
  problem: ApiFailure | null;
}

interface ConsoleSession {
  projectId: string;
  snap: ConsoleSnapshot;
  listeners: Set<() => void>;
  mounts: number;
  unsubscribe: (() => void) | null;
  /** Leitura em voo (uma por vez). */
  loading: boolean;
  /** Pedido de leitura que chegou com outra em voo: roda quando ela voltar. */
  reloadPending: boolean;
}

const sessions = new Map<string, ConsoleSession>();

const EMPTY: ConsoleSnapshot = {
  loaded: false,
  active: null,
  last: null,
  history: [],
  timeoutSecs: 0,
  transcript: [],
  posting: false,
  stopping: false,
  problem: null,
};

function sessionFor(projectId: string): ConsoleSession {
  const found = sessions.get(projectId);
  if (found !== undefined) return found;
  const created: ConsoleSession = { projectId, snap: EMPTY, listeners: new Set(), mounts: 0, unsubscribe: null, loading: false, reloadPending: false };
  sessions.set(projectId, created);
  return created;
}

/** Testes: esquece todas as sessões do Console. */
export function resetConsoleSessions(): void {
  for (const s of sessions.values()) s.unsubscribe?.();
  sessions.clear();
}

function update(s: ConsoleSession, patch: Partial<ConsoleSnapshot> | ((snap: ConsoleSnapshot) => Partial<ConsoleSnapshot>)): void {
  const changes = typeof patch === "function" ? patch(s.snap) : patch;
  s.snap = { ...s.snap, ...changes };
  for (const listener of [...s.listeners]) listener();
}

function cap(text: string): string {
  return text.length > MAX_ENTRY_TEXT ? text.slice(-MAX_ENTRY_TEXT) : text;
}

/** O comando já terminou nesta aba (o `app_exec_finished` chegou): nada o volta a "em curso". */
function isFinished(snap: ConsoleSnapshot, execId: string): boolean {
  return snap.last?.exec_id === execId || snap.transcript.some((e) => e.execId === execId && e.result !== null);
}

/** A transcrição com a entrada do comando (criada no fim, se ainda não existe). */
function withEntry(list: readonly TranscriptEntry[], execId: string, command: string, edit: (e: TranscriptEntry) => TranscriptEntry): TranscriptEntry[] {
  const index = list.findIndex((e) => e.execId === execId);
  if (index >= 0) {
    const next = [...list];
    const current = next[index];
    if (current !== undefined) next[index] = edit(current);
    return next;
  }
  const created = edit({ execId, command, text: "", seq: 0, truncated: false, result: null });
  return [...list, created].slice(-MAX_TRANSCRIPT);
}

function load(s: ConsoleSession): void {
  if (s.loading) {
    // Uma leitura em voo pode ter saído antes do fim do comando: a seguinte traz o fim.
    s.reloadPending = true;
    return;
  }
  s.loading = true;
  void getConsole(s.projectId).then((r) => {
    s.loading = false;
    if (s.reloadPending) {
      s.reloadPending = false;
      load(s);
    }
    if (!r.ok) {
      update(s, { loaded: true });
      return;
    }
    const view = r.data;
    update(s, (snap) => {
      let transcript: readonly TranscriptEntry[] = snap.transcript;
      // Primeira leitura nesta aba: o último resultado guardado no servidor abre a transcrição.
      if (transcript.length === 0 && view.last !== null && view.active === null) {
        const last = view.last;
        transcript = [{ execId: last.exec_id, command: last.command, text: cap(last.output ?? ""), seq: 0, truncated: last.output_truncated, result: last }];
      }
      // Uma leitura que saiu antes do fim não traz de volta um comando já terminado.
      const active = view.active !== null && isFinished(snap, view.active.exec_id) ? null : view.active;
      const last = view.last !== null || snap.last === null ? view.last : snap.last;
      if (active !== null) {
        transcript = withEntry(transcript, active.exec_id, active.command, (e) => ({ ...e, command: e.command === "" ? active.command : e.command }));
      }
      // Um comando que acabou enquanto a aba não via os eventos: o resultado do servidor completa.
      if (view.last !== null) {
        const last = view.last;
        transcript = transcript.map((e) =>
          e.execId === last.exec_id && e.result === null ? { ...e, text: e.text === "" ? cap(last.output ?? "") : e.text, result: last } : e,
        );
      }
      return { loaded: true, active, last, history: view.history, timeoutSecs: view.timeoutSecs, transcript };
    });
  });
}

function applyEvent(s: ConsoleSession, event: ConsoleEvent): void {
  if (event.project_id !== s.projectId) return;
  if (event.event_type === "app_exec_started") {
    const p = event.payload;
    update(s, (snap) => ({
      active:
        snap.active?.exec_id === p.exec_id || isFinished(snap, p.exec_id)
          ? snap.active
          : (toExecView({ ...p, status: "running" }) ?? snap.active),
      transcript: withEntry(snap.transcript, p.exec_id, p.command, (e) => ({ ...e, command: p.command })),
    }));
    return;
  }
  if (event.event_type === "app_exec_output") {
    const p = event.payload;
    update(s, (snap) => ({
      transcript: withEntry(snap.transcript, p.exec_id, "", (e) =>
        p.seq <= e.seq ? e : { ...e, seq: p.seq, text: cap(e.text + p.text), truncated: e.truncated || p.truncated },
      ),
    }));
    return;
  }
  const p = event.payload;
  update(s, (snap) => {
    const base = snap.active?.exec_id === p.exec_id ? snap.active : null;
    const result = toExecView({ ...(base ?? {}), ...p, ...(base === null ? {} : { command: base.command, spec_id: base.spec_id }) });
    return {
      active: snap.active?.exec_id === p.exec_id ? null : snap.active,
      stopping: false,
      last: result ?? snap.last,
      transcript: withEntry(snap.transcript, p.exec_id, result?.command ?? "", (e) => ({ ...e, result })),
    };
  });
  // O histórico gravado (com o fim) e a saída final vêm da releitura.
  load(s);
}

function attach(s: ConsoleSession): void {
  s.mounts += 1;
  if (s.unsubscribe === null) s.unsubscribe = subscribeConsoleEvents((event) => applyEvent(s, event));
}

function detach(s: ConsoleSession): void {
  s.mounts -= 1;
  // Os eventos seguem assinados: a transcrição desta aba continua ao voltar ao Console.
}

function run(s: ConsoleSession, specId: string, command: string): void {
  const text = command.trim();
  if (s.snap.posting || text === "") return;
  update(s, { posting: true, problem: null });
  void runConsoleCommand(s.projectId, specId, text).then((r) => {
    if (!r.ok) {
      update(s, { posting: false, problem: r.error });
      return;
    }
    const view = r.data;
    update(s, (snap) => ({
      posting: false,
      // O `app_exec_finished` pode chegar antes da resposta 202: o comando já terminou.
      active: snap.active?.exec_id === view.exec_id || isFinished(snap, view.exec_id) ? snap.active : view,
      transcript: withEntry(snap.transcript, view.exec_id, view.command, (e) => ({ ...e, command: view.command })),
    }));
  });
}

function stop(s: ConsoleSession): void {
  if (s.snap.stopping || s.snap.active === null) return;
  update(s, { stopping: true, problem: null });
  void stopConsoleCommand(s.projectId).then((r) => {
    update(s, { stopping: false, ...(r.ok ? {} : { problem: r.error }) });
    load(s);
  });
}

interface ConsoleActions {
  /** Só depois da confirmação humana. */
  run: (command: string) => void;
  stop: () => void;
  clearProblem: () => void;
}

/** Console do projeto para a janela; lê o servidor quando visível. */
export function useConsoleSession(projectId: string, specId: string | null, visible: boolean): { snap: ConsoleSnapshot; actions: ConsoleActions } {
  const session = sessionFor(projectId);
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
  useEffect(() => {
    if (visible) load(session);
  }, [session, visible]);
  const specRef = useRef(specId);
  specRef.current = specId;
  const actionsRef = useRef<{ session: ConsoleSession; actions: ConsoleActions } | null>(null);
  if (actionsRef.current?.session !== session) {
    const s = session;
    actionsRef.current = {
      session: s,
      actions: {
        run: (command) => {
          const spec = specRef.current;
          if (spec !== null) run(s, spec, command);
        },
        stop: () => stop(s),
        clearProblem: () => update(s, { problem: null }),
      },
    };
  }
  return { snap, actions: actionsRef.current.actions };
}
