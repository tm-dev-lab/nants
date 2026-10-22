/*
 * Invariantes do histórico do chat (store puro, sem React). Um PRNG com semente fixa gera
 * operações aleatórias (otimista, confirmação, resposta ao vivo, rajadas que atingem o teto,
 * SSE duplicado, carga do histórico em formas diversas, troca de projeto) e verifica as
 * propriedades após cada passo.
 */
import { describe, expect, it } from "vitest";
import { createAppStore, MAX_MESSAGES_PER_PROJECT } from "./store";
import type { ChatHistoryTurn } from "./api/types";

const PROJECTS = ["pA", "pB"] as const;
/** Linhas de histórico que a API devolve (últimos 40 turnos persistidos). */
const WINDOW = 40;

/** PRNG determinístico (mulberry32). */
function prng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function turnId(n: number): string {
  return n.toString(16).padStart(32, "0");
}

interface Persisted {
  role: "user" | "architect";
  text: string;
  ts: number;
  turn_id: string;
}

function toTurn(p: Persisted, withId: boolean): ChatHistoryTurn {
  return withId ? { role: p.role, text: p.text, ts: p.ts, turn_id: p.turn_id } : { role: p.role, text: p.text, ts: p.ts };
}

type Store = ReturnType<typeof createAppStore>;

function textsOf(store: Store, pid: string): string[] {
  return (store.getState().messages[pid] ?? []).map((m) => m.text);
}

/** Contagem por texto (uma passada; evita O(n*m) com cargas grandes). */
function countsOf(texts: readonly string[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const t of texts) counts.set(t, (counts.get(t) ?? 0) + 1);
  return counts;
}

describe("invariantes do histórico (PRNG, semente fixa)", () => {
  it("I1-I5 valem após cada uma de 400 operações aleatórias, com o teto atingido várias vezes", () => {
    const rand = prng(20261014);
    const pick = <T>(xs: readonly T[]): T => xs[Math.floor(rand() * xs.length)] as T;
    const store = createAppStore();
    const log: Record<string, Persisted[]> = { pA: [], pB: [] };
    const inflight: Record<string, { id: number; text: string }[]> = { pA: [], pB: [] };
    /** Respostas ao vivo criadas desde a última carga com ids (ordem de criação). */
    const sinceHydrate: Record<string, { text: string }[]> = { pA: [], pB: [] };
    let seq = 0;
    let turns = 0;
    let capHits = 0;

    const userOp = (pid: string): void => {
      seq += 1;
      const text = `${pid}-u${seq}`;
      const id = store.appendMessage(pid, { role: "user", text });
      inflight[pid]?.push({ id, text });
    };

    const turnOp = (pid: string, step: number): void => {
      seq += 1;
      turns += 1;
      const turn = turnId(turns);
      const reply = `${pid}-r${seq}`;
      const pending = inflight[pid]?.shift();
      const userText = pending?.text ?? `${pid}-x${seq}`;
      log[pid]?.push(
        { role: "user", text: userText, ts: step, turn_id: turn },
        { role: "architect", text: reply, ts: step, turn_id: turn },
      );
      if (pending !== undefined) store.confirmUserMessage(pid, pending.id, turn);
      store.showArchitectReply(pid, reply, turn);
      sinceHydrate[pid]?.push({ text: reply });
    };

    for (let step = 0; step < 400; step += 1) {
      const pid = pick(PROJECTS);
      const op = rand();

      if (op < 0.15) {
        // Otimista do usuário (POST em voo).
        userOp(pid);
      } else if (op < 0.35) {
        // Turno completo: o POST devolve o turno e o Arquiteto responde ao vivo.
        turnOp(pid, step);
      } else if (op < 0.45) {
        // SSE/POST duplicado de um turno recente já exibido: não pode acrescentar nada.
        const recent = (log[pid] ?? []).slice(-WINDOW).filter((p) => p.role === "architect");
        const shown = store.getState().shownTurns[pid] ?? [];
        const candidates = recent.filter((p) => shown.includes(p.turn_id));
        if (candidates.length > 0) {
          const target = pick(candidates);
          const before = textsOf(store, pid);
          expect(store.showArchitectReply(pid, target.text, target.turn_id)).toBe(false);
          expect(textsOf(store, pid)).toEqual(before);
        }
      } else if (op < 0.75) {
        // Carga do histórico: com ids, legada (sem ids), vazia (falha transitória) ou maior que o teto.
        const mode = rand();
        const window = (log[pid] ?? []).slice(-WINDOW);
        let h: ChatHistoryTurn[];
        if (mode < 0.5) {
          h = window.map((p) => toTurn(p, true));
        } else if (mode < 0.7) {
          h = window.map((p) => toTurn(p, false));
        } else if (mode < 0.8) {
          h = [];
        } else {
          // H maior que o teto: 520 linhas persistidas com ids próprios.
          h = Array.from({ length: MAX_MESSAGES_PER_PROJECT + 20 }, (_, k) => {
            seq += 1;
            return {
              role: k % 2 === 0 ? ("user" as const) : ("architect" as const),
              text: `${pid}-h${seq}`,
              ts: seq,
              turn_id: turnId(1_000_000 + seq),
            };
          });
        }
        store.hydrateHistory(pid, h);
        if (h.length > 0) {
          // I4: mesmo histórico, segunda carga é idempotente.
          const once = textsOf(store, pid);
          store.hydrateHistory(pid, h);
          expect(textsOf(store, pid)).toEqual(once);
          sinceHydrate[pid] = [];
        }
        // I1: com ids, cada linha do histórico (até o teto) aparece exatamente uma vez.
        if (mode < 0.5 || mode >= 0.8) {
          const counts = countsOf(textsOf(store, pid));
          for (const row of h.slice(-MAX_MESSAGES_PER_PROJECT)) expect(counts.get(row.text)).toBe(1);
        }
      } else {
        // Rajada: 80 a 200 operações ao vivo seguidas (otimistas e respostas com turnId novo).
        const burst = 80 + Math.floor(rand() * 121);
        for (let i = 0; i < burst; i += 1) {
          if (rand() < 0.5) userOp(pid);
          else turnOp(pid, step);
        }
      }

      // Após cada passo, para os dois projetos.
      for (const p of PROJECTS) {
        const texts = textsOf(store, p);
        // I3: teto.
        expect(texts.length).toBeLessThanOrEqual(MAX_MESSAGES_PER_PROJECT);
        if (texts.length === MAX_MESSAGES_PER_PROJECT) capHits += 1;
        // I2: só o teto remove respostas ao vivo, e remove as MAIS ANTIGAS. As que cabem
        // (as mais recentes) permanecem. Ausência só pode ser um prefixo da lista de criação.
        const counts = countsOf(texts);
        let sawPresent = false;
        let anyMissing = false;
        for (const live of sinceHydrate[p] ?? []) {
          const present = counts.get(live.text) === 1;
          if (present) sawPresent = true;
          else {
            anyMissing = true;
            expect(sawPresent).toBe(false);
          }
        }
        if (anyMissing) expect(texts.length).toBe(MAX_MESSAGES_PER_PROJECT);
        // I5: nenhuma mensagem de outro projeto.
        for (const t of texts) expect(t.startsWith(`${p}-`)).toBe(true);
      }
    }

    // A pressão precisa atingir o teto várias vezes; se alguém reduzir a carga, a suíte falha.
    expect(capHits).toBeGreaterThan(3);
  });

  it("F-05: 500 mensagens ao vivo não persistidas não expulsam as 40 linhas do histórico", () => {
    const store = createAppStore();
    for (let i = 0; i < MAX_MESSAGES_PER_PROJECT; i += 1) {
      store.appendMessage("pA", { role: "user", text: `viva-${i}` });
    }
    const rows: ChatHistoryTurn[] = Array.from({ length: WINDOW }, (_, i) => ({
      role: i % 2 === 0 ? ("user" as const) : ("architect" as const),
      text: `hist-${i}`,
      ts: i,
      turn_id: turnId(i + 1),
    }));
    store.hydrateHistory("pA", rows);
    const texts = textsOf(store, "pA");
    for (const row of rows) expect(texts.filter((t) => t === row.text)).toHaveLength(1);
    expect(texts.length).toBeLessThanOrEqual(MAX_MESSAGES_PER_PROJECT);
  });

  it("F-06: GET com turns [] não apaga a conversa já exibida", () => {
    const store = createAppStore();
    const rows: ChatHistoryTurn[] = [
      { role: "user", text: "Quero X", ts: 1, turn_id: turnId(1) },
      { role: "architect", text: "Entendido.", ts: 2, turn_id: turnId(1) },
    ];
    store.hydrateHistory("pA", rows);
    const before = textsOf(store, "pA");
    store.hydrateHistory("pA", []);
    expect(textsOf(store, "pA")).toEqual(before);
    expect(before).toEqual(["Quero X", "Entendido."]);
  });
});
