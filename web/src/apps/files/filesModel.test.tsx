/*
 * Regras puras dos Arquivos e a atualização (`useFilesRefresh`): polling de 8 s só com a janela
 * visível, debounce dos eventos e uma atualização ao voltar a ficar visível.
 */
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { parseSelection } from "../../desktop/uiState";
import type { SpaceData } from "../../desktop/useSpaceData";
import { appStore } from "../../store";
import { makeSpec, makeTask, resetStore } from "../../test-utils";
import { useFilesRefresh } from "./FilesApp";
import {
  clampSplit,
  DEFAULT_FILES_SPLIT,
  diffLineKind,
  FILES_EVENT_DEBOUNCE_MS,
  FILES_POLL_MS,
  filesArrangement,
  filesSummary,
  formatBytes,
  splitByKey,
  splitFromPointer,
  splitLines,
} from "./filesModel";

describe("Arquivos: regras puras", () => {
  it("classifica cabeçalhos, hunks, adições, remoções e contexto", () => {
    expect(["--- a/x", "+++ b/x", "@@ -1 +1 @@", "+novo", "-velho", " igual"].map(diffLineKind)).toEqual(["head", "head", "hunk", "add", "del", "ctx"]);
  });

  it("tamanhos e linhas", () => {
    // Número pelo formatador do idioma ativo (pt-BR: vírgula decimal; o en fica no teste de idiomas).
    expect([formatBytes(12), formatBytes(2048), formatBytes(1536), formatBytes(3 * 1024 * 1024)]).toEqual(["12 B", "2 KB", "1,5 KB", "3 MB"]);
    expect(splitLines("")).toEqual([]);
    expect(splitLines("a\nb\n")).toEqual(["a", "b"]);
    expect(splitLines("a\r\n\nb")).toEqual(["a", "", "b"]);
  });

  it("divisão: limites, ponteiro e teclado", () => {
    expect(clampSplit(0.01)).toBe(0.15);
    expect(clampSplit(2)).toBe(0.85);
    expect(clampSplit(Number.NaN)).toBe(DEFAULT_FILES_SPLIT);
    expect(splitFromPointer(600, 100, 1000)).toBe(0.5);
    expect(splitFromPointer(10, 0, 0)).toBe(DEFAULT_FILES_SPLIT);
    expect(splitByKey(0.5, "ArrowLeft")).toBeCloseTo(0.46);
    expect(splitByKey(0.5, "ArrowDown")).toBeCloseTo(0.54);
    expect(splitByKey(0.5, "Home")).toBe(0.15);
    expect(splitByKey(0.5, "End")).toBe(0.85);
    expect(splitByKey(0.5, "a")).toBeNull();
    expect([filesArrangement("narrow"), filesArrangement("medium"), filesArrangement("wide")]).toEqual(["stacked", "split", "split"]);
  });

  it("seleção: fonte, divisão e aba das Configurações validadas", () => {
    expect(parseSelection({ fileSource: "task", filesSplit: 0.9, settingsTab: "pricing" })).toMatchObject({ fileSource: "task", filesSplit: 0.85, settingsTab: "pricing" });
    const bad = parseSelection({ fileSource: "x", filesSplit: "1", settingsTab: "__proto__" });
    expect(bad.fileSource).toBeUndefined();
    expect(bad.filesSplit).toBeUndefined();
    expect(bad.settingsTab).toBeUndefined();
  });

  it("resumo: código integrado, com a tarefa em andamento quando há execução", () => {
    const spec = makeSpec("s1", "Loja", "IN_PROGRESS", [makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "IN_PROGRESS")]);
    const space = { activeSpec: spec, running: true } as unknown as SpaceData;
    expect(filesSummary({ space: null, readiness: null, projectCount: 0 })).toEqual({});
    expect(filesSummary({ space, readiness: null, projectCount: 1 }).subtitle).toBe("código integrado · T02 em andamento");
    expect(filesSummary({ space: { ...space, running: false }, readiness: null, projectCount: 1 }).subtitle).toBe("código integrado");
  });
});

describe("useFilesRefresh", () => {
  beforeEach(() => {
    resetStore();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("polling a cada 8 s só com a janela visível; voltar a ficar visível atualiza uma vez", () => {
    const { result, rerender } = renderHook(({ visible }) => useFilesRefresh("p1", visible), { initialProps: { visible: true } });
    expect(result.current.tick).toBe(0);
    act(() => vi.advanceTimersByTime(FILES_POLL_MS));
    expect(result.current.tick).toBe(1);
    rerender({ visible: false });
    act(() => vi.advanceTimersByTime(FILES_POLL_MS * 3));
    expect(result.current.tick).toBe(1);
    rerender({ visible: true });
    expect(result.current.tick).toBe(2);
    act(() => result.current.refreshNow());
    expect(result.current.tick).toBe(3);
  });

  it("montada invisível: inativa e sem atualização; a primeira vez visível só ativa (sem tick extra)", () => {
    const { result, rerender } = renderHook(({ visible }) => useFilesRefresh("p1", visible), { initialProps: { visible: false } });
    expect(result.current.active).toBe(false);
    act(() => vi.advanceTimersByTime(FILES_POLL_MS * 3));
    expect(result.current.tick).toBe(0);
    rerender({ visible: true });
    expect(result.current.active).toBe(true);
    expect(result.current.tick).toBe(0);
    rerender({ visible: false });
    rerender({ visible: true });
    expect(result.current.tick).toBe(1);
  });

  it("rajada de eventos vira uma atualização depois do debounce; invisível, nenhuma", () => {
    const { result, rerender } = renderHook(({ visible }) => useFilesRefresh("p1", visible), { initialProps: { visible: true } });
    const bump = (): void => {
      act(() => appStore.setState((s) => ({ revisions: { ...s.revisions, p1: (s.revisions.p1 ?? 0) + 1 } })));
    };
    bump();
    bump();
    bump();
    act(() => vi.advanceTimersByTime(FILES_EVENT_DEBOUNCE_MS - 1));
    expect(result.current.tick).toBe(0);
    act(() => vi.advanceTimersByTime(1));
    expect(result.current.tick).toBe(1);
    rerender({ visible: false });
    bump();
    act(() => vi.advanceTimersByTime(FILES_EVENT_DEBOUNCE_MS * 2));
    expect(result.current.tick).toBe(1);
  });

  it("novo passo de agente atualiza com debounce; tokens do mesmo passo não", () => {
    const { result } = renderHook(() => useFilesRefresh("p1", true));
    const step = (activityId: string, text: string): void => {
      act(() => appStore.applyEvent({ event_type: "agent_activity", project_id: "p1", payload: { agent: "developer", task_id: "T01", activity_id: activityId, kind: "text", text } } as never));
    };
    step("a1", "início");
    act(() => vi.advanceTimersByTime(FILES_EVENT_DEBOUNCE_MS));
    expect(result.current.tick).toBe(1);
    step("a1", "mais tokens do mesmo passo");
    step("a1", "e mais");
    act(() => vi.advanceTimersByTime(FILES_EVENT_DEBOUNCE_MS * 2));
    expect(result.current.tick).toBe(1);
    step("a2", "outro passo");
    act(() => vi.advanceTimersByTime(FILES_EVENT_DEBOUNCE_MS));
    expect(result.current.tick).toBe(2);
  });
});
