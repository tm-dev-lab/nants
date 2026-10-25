import { afterEach, describe, expect, it } from "vitest";
import { setLanguage } from "./i18n";
import {
  barOperationText,
  boardOperationText,
  currentOperation,
  isLongNow,
  LONG_COMMAND_MS,
  longOperationsKey,
  longOperationsOf,
  nearLimit,
  parseLongOperations,
  type LongOperation,
} from "./longOperation";
import type { ActivityEntry } from "./store";

const NOW = 50_000_000;

function entry(extra: Partial<ActivityEntry>): ActivityEntry {
  return { key: JSON.stringify([extra.agent ?? "developer", extra.taskId ?? null]), agent: "developer", activityId: "a", reasoning: "", text: "", tool: "", startedAt: 0, lastAt: 0, ...extra };
}

afterEach(() => setLanguage("pt-BR", { persist: false }));

describe("operações longas em curso (spec 123 T02)", () => {
  it("só entradas com comando viram operação; o tipo vem do agente e da etapa", () => {
    const ops = longOperationsOf([
      entry({ agent: "developer", taskId: "T02", command: "cargo build", commandStartedAt: NOW - 30_000 }),
      entry({ agent: "tests", taskId: "T02", command: "npm test", commandStartedAt: NOW - 420_000, commandLimitMs: 600_000, stageCode: { code: "tests", params: {} } }),
      entry({ agent: "tests", taskId: "T03", command: "npm run build", commandStartedAt: NOW - 5_000, stageCode: { code: "check_base", params: { check: "build" } } }),
      entry({ agent: "reviewer", taskId: "T02", reasoning: "lendo" }),
    ]);
    expect(ops).toEqual([
      { kind: "tests", taskId: "T02", startedAt: NOW - 420_000, limitMs: 600_000 },
      { kind: "command", taskId: "T02", startedAt: NOW - 30_000, limitMs: null },
      { kind: "check", taskId: "T03", startedAt: NOW - 5_000, limitMs: null },
    ]);
  });

  it("a assinatura é estável e volta à mesma lista; vazia sem operação", () => {
    const list = [entry({ agent: "tests", taskId: "T02", command: "npm test", commandStartedAt: NOW - 1_000, commandLimitMs: 600_000, commandBytes: 10 })];
    const key = longOperationsKey(list);
    // O progresso (bytes, linha) não muda a assinatura: o Quadro e a barra não re-renderizam a cada sinal.
    expect(longOperationsKey([{ ...list[0]!, commandBytes: 99, commandLine: "x" }])).toBe(key);
    expect(parseLongOperations(key)).toEqual(longOperationsOf(list));
    expect(longOperationsKey([entry({})])).toBe("");
    expect(parseLongOperations("")).toEqual([]);
    expect(parseLongOperations("{quebrado")).toEqual([]);
  });

  it("comando de agente só conta depois de LONG_COMMAND_MS; a rodada de testes conta sempre", () => {
    const cmd: LongOperation = { kind: "command", taskId: "T02", startedAt: NOW - LONG_COMMAND_MS + 1, limitMs: null };
    expect(isLongNow(cmd, NOW)).toBe(false);
    expect(isLongNow({ ...cmd, startedAt: NOW - LONG_COMMAND_MS }, NOW)).toBe(true);
    expect(isLongNow({ ...cmd, kind: "tests", startedAt: NOW }, NOW)).toBe(true);
  });

  it("perto do limite a partir de 80%; sem limite, nunca", () => {
    expect(nearLimit({ startedAt: NOW - 479_999, limitMs: 600_000 }, NOW)).toBe(false);
    expect(nearLimit({ startedAt: NOW - 480_000, limitMs: 600_000 }, NOW)).toBe(true);
    expect(nearLimit({ startedAt: NOW - 9_999_999, limitMs: null }, NOW)).toBe(false);
  });

  it("a fase da execução: os testes vencem o comando; filtra por tarefa", () => {
    const ops: LongOperation[] = [
      { kind: "command", taskId: "T02", startedAt: NOW - 600_000, limitMs: null },
      { kind: "tests", taskId: "T02", startedAt: NOW - 60_000, limitMs: 600_000 },
      { kind: "command", taskId: "T03", startedAt: NOW - 20_000, limitMs: null },
    ];
    expect(currentOperation(ops, NOW)?.kind).toBe("tests");
    expect(currentOperation(ops, NOW, "T03")).toMatchObject({ kind: "command", taskId: "T03" });
    expect(currentOperation(ops, NOW, "T09")).toBeNull();
    expect(currentOperation([], NOW)).toBeNull();
  });

  it("textos curtos da barra e do Quadro, nos dois idiomas", () => {
    const op: LongOperation = { kind: "tests", taskId: "T02", startedAt: NOW - 420_000, limitMs: 600_000 };
    expect(barOperationText(op, NOW)).toBe("Testes · 7 min");
    expect(boardOperationText(op, NOW)).toBe("testes rodando · 7 min");
    expect(barOperationText({ ...op, kind: "check" }, NOW)).toBe("Checagem · 7 min");
    expect(boardOperationText({ ...op, kind: "command" }, NOW)).toBe("comando rodando · 7 min");
    setLanguage("en", { persist: false });
    expect(barOperationText(op, NOW)).toBe("Tests · 7 min");
    expect(boardOperationText(op, NOW)).toBe("tests running · 7 min");
    expect(barOperationText({ ...op, kind: "command", startedAt: NOW - 45_000 }, NOW)).toBe("Command · 45 s");
  });
});
