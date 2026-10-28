/* Regras puras do Quadro (spec 093, T07). */
import { parseReviewMessage } from "../../reviewMessage";
import { describe, expect, it } from "vitest";
import type { TaskStatus } from "../../api/types";
import { forbiddenGlyphIn } from "../../desktop/glyphs";
import type { SpaceData } from "../../desktop/useSpaceData";
import type { ChatMessage } from "../../store";
import { makeSpec, makeTask } from "../../test-utils";
import { translate } from "../../i18n";
import {
  boardSummary,
  buildColumns,
  columnFor,
  columnIndex,
  lastReviewLine,
  lastReviewMessage,
  movedCards,
  OPTIONAL_COLUMNS,
  parseBoardColumns,
  pauseReasonText,
  pickActiveTask,
  recoveryLabel,
  runStopCode,
  runStopReason,
  taskPauses,
  toggleBoardColumn,
} from "./boardModel";

const ALL: readonly TaskStatus[] = ["TODO", "IN_PROGRESS", "IN_REVIEW", "CHANGES_REQUESTED", "APPROVED", "INTEGRATING", "DONE", "BLOCKED"];

describe("colunas do Quadro", () => {
  it("padrão: A fazer, Em desenvolvimento, Em revisão e Concluídas, na ordem do fluxo", () => {
    const columns = buildColumns([makeTask("T1", "a", "TODO")], new Set());
    expect(columns.map((c) => c.label)).toEqual(["A fazer", "Em desenvolvimento", "Em revisão", "Concluídas"]);
  });

  it("estado de coluna oculta vai para a coluna do grupo; escolhida, fica na própria", () => {
    expect(columnFor("CHANGES_REQUESTED", new Set())).toBe("IN_PROGRESS");
    expect(columnFor("APPROVED", new Set())).toBe("IN_REVIEW");
    expect(columnFor("INTEGRATING", new Set())).toBe("IN_REVIEW");
    expect(columnFor("APPROVED", new Set<TaskStatus>(["APPROVED"]))).toBe("APPROVED");
    expect(columnFor("BLOCKED", new Set())).toBe("BLOCKED");
  });

  it("nenhum cartão se perde, por padrão e com tudo escolhido", () => {
    const tasks = ALL.map((s, i) => makeTask(`T${i}`, s, s));
    for (const shown of [new Set<TaskStatus>(), new Set<TaskStatus>(OPTIONAL_COLUMNS)]) {
      const total = buildColumns(tasks, shown).reduce((n, c) => n + c.tasks.length, 0);
      expect(total).toBe(tasks.length);
    }
  });

  it("Bloqueada aparece com tarefa bloqueada; status ausente, com erro ou desconhecido vai para Indefinida", () => {
    const columns = buildColumns(
      [makeTask("T1", "a", "BLOCKED"), makeTask("T2", "b", null), makeTask("T3", "c", "TODO", { error: "inválida" }), makeTask("T4", "d", "XYZ" as TaskStatus)],
      new Set(),
    );
    expect(columns.find((c) => c.key === "BLOCKED")?.kind).toBe("blocked");
    expect(columns.find((c) => c.key === "UNDEFINED")?.tasks.map((t) => t.id)).toEqual(["T2", "T3", "T4"]);
    expect(buildColumns([makeTask("T1", "a", "TODO")], new Set()).some((c) => c.key === "BLOCKED" || c.key === "UNDEFINED")).toBe(false);
  });

  it("ordem do plano dentro da coluna", () => {
    const columns = buildColumns([makeTask("T3", "c", "TODO"), makeTask("T1", "a", "TODO"), makeTask("T2", "b", "TODO")], new Set());
    expect(columns[0]?.tasks.map((t) => t.id)).toEqual(["T3", "T1", "T2"]);
  });

  it("colunas do ui-state: só as opcionais conhecidas; alternar devolve a lista na ordem do fluxo", () => {
    expect([...parseBoardColumns(["BLOCKED", "TODO", "lixo", "APPROVED"])]).toEqual(["APPROVED", "BLOCKED"]);
    expect(parseBoardColumns(undefined).size).toBe(0);
    expect(toggleBoardColumn(new Set<TaskStatus>(["BLOCKED"]), "APPROVED")).toEqual(["APPROVED", "BLOCKED"]);
    expect(toggleBoardColumn(new Set<TaskStatus>(["BLOCKED", "APPROVED"]), "BLOCKED")).toEqual(["APPROVED"]);
  });

  it("troca de coluna: só os cartões que mudaram; nada na carga inicial", () => {
    const before = columnIndex(buildColumns([makeTask("T1", "a", "TODO"), makeTask("T2", "b", "TODO")], new Set()));
    const after = columnIndex(buildColumns([makeTask("T1", "a", "IN_PROGRESS"), makeTask("T2", "b", "TODO")], new Set()));
    expect([...movedCards(before, after)]).toEqual(["T1"]);
    expect(movedCards(null, after).size).toBe(0);
    // Mudou de estado mas ficou na mesma coluna (coluna do grupo): não anima.
    const review = columnIndex(buildColumns([makeTask("T1", "a", "IN_REVIEW")], new Set()));
    const approved = columnIndex(buildColumns([makeTask("T1", "a", "APPROVED")], new Set()));
    expect(movedCards(review, approved).size).toBe(0);
  });

  it("tarefa ativa: a em andamento; senão a primeira não concluída", () => {
    expect(pickActiveTask([makeTask("T1", "a", "DONE"), makeTask("T2", "b", "TODO"), makeTask("T3", "c", "IN_REVIEW")])?.id).toBe("T3");
    expect(pickActiveTask([makeTask("T1", "a", "DONE"), makeTask("T2", "b", "TODO")])?.id).toBe("T2");
    expect(pickActiveTask([makeTask("T1", "a", "DONE")])).toBeNull();
  });

  it("selo de recuperação sem caractere no lugar de ícone", () => {
    expect(recoveryLabel({ phase: "analyzing", cycle: 2 })).toBe("Arquiteto analisando (ciclo 2)");
    expect(recoveryLabel({ phase: "retry", cycle: 1 })).toBe("Estratégia nova aplicada (ciclo 1)");
    expect(forbiddenGlyphIn(recoveryLabel({ phase: "analyzing", cycle: 2 }))).toBeNull();
  });
});

describe("última revisão da tarefa", () => {
  const review = (id: number, taskId: string, text: string, specId = "s1"): ChatMessage =>
    ({ id, role: "agent", agent: "reviewer", text, at: id, taskId, specId }) as ChatMessage;

  it("a mais recente do Revisor para a tarefa, na spec, com veredito ou critérios", () => {
    const messages = [
      review(1, "T1", "Pediu ajustes — falta teste"),
      review(2, "T2", "Aprovado — outra tarefa"),
      review(3, "T1", "texto comum sem veredito"),
      review(4, "T1", "Aprovado — tudo certo", "s0"),
      review(5, "T1", "Aprovado — agora sim"),
    ];
    expect(lastReviewMessage(messages, "s1", "T1")?.id).toBe(5);
    expect(lastReviewMessage(messages.slice(0, 4), "s1", "T1")?.id).toBe(1);
    expect(lastReviewMessage(undefined, "s1", "T1")).toBeNull();
  });
});

describe("resumo do Quadro", () => {
  const space = (tasks: ReturnType<typeof makeTask>[]): SpaceData => ({ activeSpec: makeSpec("s1", "Loja", "IN_PROGRESS", tasks) }) as unknown as SpaceData;

  it("subtítulo vivo, selo das bloqueadas e vazio", () => {
    expect(boardSummary({ space: null, readiness: null, projectCount: 0 })).toEqual({});
    expect(boardSummary({ space: space([]), readiness: null, projectCount: 1 }).empty?.message).toContain("Ainda não há tarefas");
    expect(boardSummary({ space: space([makeTask("T1", "a", "DONE"), makeTask("T2", "b", "DONE"), makeTask("T3", "c", "TODO"), makeTask("T4", "d", "TODO")]), readiness: null, projectCount: 1 })).toEqual({
      subtitle: "4 tarefas · 2 concluídas",
    });
    const blocked = boardSummary({ space: space([makeTask("T1", "a", "BLOCKED")]), readiness: null, projectCount: 1 });
    expect(blocked.subtitle).toBe("1 tarefa · 0 concluídas");
    expect(blocked.badge).toEqual({ count: 1, label: "1 tarefa bloqueada" });
  });
});

describe("linha recolhida da última revisão (094/T05)", () => {
  it("'Última revisão · T02 · Pediu ajustes'; aprovada; sem veredito, só a tarefa", () => {
    expect(lastReviewLine("T02", "changes")).toBe("Última revisão · T02 · Pediu ajustes");
    expect(lastReviewLine("T02", "approved")).toBe("Última revisão · T02 · Aprovado");
    expect(lastReviewLine("T02", null)).toBe("Última revisão · T02");
    expect(forbiddenGlyphIn(lastReviewLine("T02", "changes"))).toBeNull();
  });

  it("spec 124 T01: resumo dos estados dos apontamentos depois do veredito", () => {
    const review = parseReviewMessage(
      "Pediu ajustes: x\n\nApontamentos:\n- F-02 [HIGH, aberto há 2 revisões] a:1: d\n- F-03 [HIGH, novo] b:1: e\n\nResolvidos:\n- F-01 [HIGH] c:1: f",
    );
    expect(review).not.toBeNull();
    if (review === null) return;
    expect(lastReviewLine("T02", review.verdict, review)).toBe("Última revisão · T02 · Pediu ajustes · 1 novo · 1 ainda aberto · 1 resolvido");
    const old = parseReviewMessage("Pediu ajustes: x\n\nApontamentos:\n- F-01 [HIGH] a:1: d");
    if (old === null) return;
    expect(lastReviewLine("T02", old.verdict, old)).toBe("Última revisão · T02 · Pediu ajustes");
  });
});

describe("tarefa pausada (spec 114 T04)", () => {
  const sys = (id: number, code: string, specId = "s1"): ChatMessage => ({ id, role: "agent", agent: "system", text: "x", at: id, specId, code }) as ChatMessage;

  it("motivo: a última mensagem de parada do run da spec; sem ela, interrompida", () => {
    expect(runStopReason(undefined, "s1")).toBe("interrupted");
    expect(runStopReason([sys(1, "run.stopped")], "s1")).toBe("stopped");
    expect(runStopReason([sys(1, "run.stopped"), sys(2, "run.provider_failed_detail")], "s1")).toBe("provider");
    expect(runStopReason([sys(1, "run.failed_cause"), sys(2, "final.unavailable")], "s1")).toBe("failed");
    // Spec 116 T03: as pausas do plano ChatGPT têm o motivo próprio, pelo código.
    expect(runStopReason([sys(1, "run.plan_limit")], "s1")).toBe("plan_limit");
    expect(runStopReason([sys(1, "run.plan_not_eligible")], "s1")).toBe("plan_not_eligible");
    expect(runStopReason([sys(1, "run.plan_needs_login")], "s1")).toBe("plan_sign_in");
    expect(pauseReasonText("plan_limit")).toBe("limite de uso do plano atingido");
    expect(pauseReasonText("plan_not_eligible")).toBe("conta não elegível ao plano");
    expect(pauseReasonText("plan_sign_in")).toBe("precisa entrar de novo no ChatGPT");
    expect(translate("en", "board.pause.planLimit")).toBe("plan usage limit reached");
    expect(translate("en", "board.pause.planNotEligible")).toBe("account not eligible for the plan");
    expect(translate("en", "board.pause.planNeedsLogin")).toBe("needs you to sign in to ChatGPT again");
    // Spec 132 T09: o teto de gasto e a cota diária pausam com o motivo próprio.
    expect(runStopReason([sys(1, "run.spend_limit")], "s1")).toBe("spend_limit");
    expect(runStopReason([sys(1, "run.daily_quota")], "s1")).toBe("daily_quota");
    expect(pauseReasonText("spend_limit")).toBe("teto de gasto atingido");
    expect(pauseReasonText("daily_quota")).toBe("cota diária esgotada");
    expect(translate("en", "board.pause.spendLimit")).toBe("spend limit reached");
    expect(translate("en", "board.pause.dailyQuota")).toBe("daily quota used up");
    // Outras mensagens do sistema e a conversa com o Arquiteto não contam como retomada.
    const chat = { id: 3, role: "architect", text: "ok", at: 3 } as ChatMessage;
    expect(runStopReason([sys(1, "run.stopped"), chat], "s1")).toBe("stopped");
    // Mensagem de outra spec não conta.
    expect(runStopReason([sys(1, "run.stopped", "s2")], "s1")).toBe("interrupted");
    expect(pauseReasonText("provider")).toBe("o provedor do modelo falhou");
    // Spec 116 T03: o código da parada (o cartão próprio das pausas do plano).
    expect(runStopCode([sys(1, "run.plan_limit"), sys(2, "final.unavailable")], "s1")).toBe("run.plan_limit");
    expect(runStopCode([sys(1, "run.stopped", "s2")], "s1")).toBeNull();
    expect(runStopCode(undefined, "s1")).toBeNull();
  });

  it("parada antiga, retomada e queda sem mensagem: o genérico, nunca o motivo antigo", () => {
    const dev = (id: number): ChatMessage => ({ id, role: "agent", agent: "developer", text: "seguindo", at: id, specId: "s1", taskId: "T02" }) as ChatMessage;
    const history = [sys(10, "run.provider_failed_detail"), dev(20)];
    // Pelo marco da retomada (project_status executando ou transição do estado).
    expect(runStopReason([sys(10, "run.provider_failed_detail")], "s1", { seq: 0, at: 15 })).toBe("interrupted");
    // Ao vivo, vale a ordem de chegada (`seq`), mesmo com a hora empatada (relógio recuando).
    const live = (seq: number): ChatMessage => ({ ...sys(15, "run.provider_failed_detail"), seq }) as ChatMessage;
    expect(runStopReason([live(4)], "s1", { seq: 5, at: 15 })).toBe("interrupted");
    expect(runStopReason([live(6)], "s1", { seq: 5, at: 15 })).toBe("provider");
    // Sem marco (página recarregada): o agente que falou depois da parada mostra a retomada.
    expect(runStopReason(history, "s1")).toBe("interrupted");
    // A parada da execução atual, depois do marco, vale.
    expect(runStopReason([...history, sys(30, "run.stopped")], "s1", { seq: 0, at: 15 })).toBe("stopped");
    expect(runStopReason([sys(10, "run.provider_failed_detail")], "s1", { seq: 0, at: 5 })).toBe("provider");
  });

  it("só tarefas com um agente; parada vence; com o run vivo, só a que espera decisão", () => {
    const tasks = [
      makeTask("T01", "a", "DONE"),
      makeTask("T02", "b", "IN_PROGRESS"),
      makeTask("T03", "c", "TODO"),
      makeTask("T04", "d", "BLOCKED"),
      makeTask("T05", "e", "INTEGRATING"),
    ];
    expect([...taskPauses(tasks, true, "provider", new Set())]).toEqual([
      ["T02", "provider"],
      ["T05", "provider"],
    ]);
    // Execução viva (ao continuar): nenhuma pausada, salvo a com pedido pendente.
    expect(taskPauses(tasks, false, "provider", new Set()).size).toBe(0);
    expect([...taskPauses(tasks, false, "interrupted", new Set(["T02", "T03"]))]).toEqual([["T02", "decision"]]);
  });
});
