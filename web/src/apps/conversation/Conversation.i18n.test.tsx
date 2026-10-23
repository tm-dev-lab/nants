/*
 * Conversa nos dois idiomas (spec 102, T02): em en, nenhum texto visível ou acessível da janela fica
 * em português (salvo o conteúdo gravado por agentes ou pela pessoa, que aparece como foi escrito);
 * mensagens do sistema com `code` aparecem pelo `code` no idioma atual; a troca de idioma vale na
 * hora, sem recarregar.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { parseApiEvent } from "../../api/events";
import type { SpecResult, SpecReport, SpecView } from "../../api/types";
import { setLanguage } from "../../i18n";
import { en } from "../../i18n/en";
import { ptBR } from "../../i18n/ptBR";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { systemMessageText } from "../../i18n/serverCodes";
import { appStore, type ActivityEntry, type ChatMessage, type PendingPermission, type ResolvedPermission } from "../../store";
import { makeSpec, makeTask, renderWith, resetStore } from "../../test-utils";
import { looksLikeChoicesAnswer } from "../../choices";
import { chatFailureMessage } from "../../chatFailure";
import { providerFailureMessage } from "../../copy";
import { localizeRecoveryAnalysis } from "../../recoveryAnalysis";
import { Conversation } from "./Conversation";
import { FactsError, ReportBody, ReportNotice, ResultBody } from "./ResultMessage";
import { conversationSummary } from "./summary";

vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
});

const PID = "p1";
const NOW = Date.now();

/** Bloco de perguntas com conteúdo do Arquiteto (em inglês: é conteúdo, não texto da interface). */
const CHOICES =
  "Let me check a few things:\n```nants-choices\n" +
  JSON.stringify({
    questions: [
      { id: "auth", text: "Login?", multi: false, options: ["Yes", "No"], recommended: "Yes", why: "Safer" },
      { id: "db", text: "Storage", multi: true, options: ["SQLite", "Postgres"] },
    ],
  }) +
  "\n```";

/** Análises de recuperação como `server/recovery.rs` grava no diálogo (cabeçalho e rótulos fixos em pt-BR). */
const RECOVERY_RETRY =
  "Análise de recuperação #1 (RETRY) [rodada autônoma]\n- Causa raiz: Flaky selector\n- Decisão: nova estratégia (RETRY)\n- Estratégia: Wait for the button\n- Escopo ampliado: tests/**\n- Rodada autônoma (aprovação automática) #1 de 2";
const RECOVERY_ESCALATE =
  "Análise de recuperação #2 (ESCALATE)\n- Causa raiz: limite de ciclos de recuperação atingido\n- Decisão: escalar ao humano\n- Estratégia sugerida: Free port 3000\n- Contexto:\n  - comando: npm test\n  - código: 1\n  - trecho do erro:\n      EADDRINUSE\n  - o que foi tentado: 2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas\n  - por que não resolveu: (citada pelo modelo) The port stays busy\n  - opções: (1) orientar o Desenvolvedor com uma estratégia concreta e usar Retomar; (2) revisar o objetivo/critérios da tarefa; (3) decidir o ponto de produto em aberto\n- Contagem: não conta como ciclo (escalada determinística)";

/** Mensagem do Revisor no formato gravado pelo servidor (marcas em pt-BR, conteúdo em inglês). */
const REVIEW =
  "Verificação final: 1 de 2 critérios atendidos\n\nPediu ajustes: Needs a guard\n\nApontamentos:\n- F-01 [MEDIUM] src/a.ts:10: Missing guard → Add the guard\n  Cenário de falha: empty input → crash\n\nObservações (não bloqueiam):\n- F-02 [LOW, observação (arquivo não mudou nesta rodada)] src/b.ts: Naming → Rename it\n\nCapturas avaliadas: t01-1-a.png\n\nCritérios:\n✓ Login works: test passes\n✗ Logout works: missing";

function spec(): SpecView {
  return {
    ...makeSpec("s1", "Shop", "IN_PROGRESS", [makeTask("T01", "Checkout", "IN_PROGRESS")]),
    plan_review: { status: "ok", issues: [{ task: "T01", text: "Too broad" }] } as never,
  };
}

function messages(): ChatMessage[] {
  let id = 0;
  const at = (offset: number) => NOW - 60_000 + offset;
  const m = (extra: Omit<ChatMessage, "id" | "at">, offset: number): ChatMessage => ({ id: ++id, at: at(offset), ...extra });
  return [
    m({ role: "user", text: "Build a shop", attachments: ["a1.png"], unsent: en["chat.turnBusy"] }, 1),
    m({ role: "architect", text: CHOICES }, 2),
    m({ role: "agent", agent: "developer", text: "Implemented the API", taskId: "T01", attempt: 2, specId: "s1" }, 3),
    m({ role: "agent", agent: "developer", text: "aprovada automaticamente: npm test", taskId: "T01", specId: "s1" }, 4),
    m({ role: "agent", agent: "reviewer", text: REVIEW, taskId: "T01", specId: "s1" }, 5),
    m(
      {
        role: "agent",
        agent: "system",
        text: "Tarefa T01 bloqueada: a worktree /w/T01 tem alterações não commitadas. Commite ou descarte-as e clique em Continuar.",
        code: "task.worktree_dirty",
        params: { task: "T01", path: "/w/T01" },
        taskId: "T01",
        specId: "s1",
      },
      6,
    ),
    m({ role: "system", text: en["chat.fallback"] }, 7),
    m({ role: "agent", agent: "architect", text: RECOVERY_RETRY, taskId: "T01", specId: "s1" }, 0.5),
    m({ role: "agent", agent: "architect", text: RECOVERY_ESCALATE, taskId: "T01", specId: "s1" }, 0.6),
    m({ role: "architect", text: "Recovery #1 (RETRY) [rodada autônoma]\nI decided to go on" }, 0),
  ];
}

const PENDING: PendingPermission[] = [
  {
    requestId: "r1",
    projectId: PID,
    taskId: "T01",
    agent: "developer",
    command: `npm install ${"x".repeat(200)}`,
    alwaysAllowed: false,
    alwaysDeniedCode: "arbitrary",
    at: NOW - 30_000,
  },
  { requestId: "r2", projectId: PID, taskId: "T01", agent: "developer", command: "compose.yaml", kind: "environment", reason: "Needs redis", diff: "+ redis", at: NOW - 20_000 },
];

const RESOLVED: ResolvedPermission[] = [
  { requestId: "r0", projectId: PID, taskId: "T01", agent: "reviewer", command: "npm test", applied: "once", requested: "always", resolvedAt: NOW - 40_000, at: NOW - 45_000 },
];

const ACTIVITIES = [
  { key: "developer:T01", agent: "developer", taskId: "T01", activityId: "a1", reasoning: "", text: "", tool: "", command: "npm test", commandStartedAt: NOW, commandLastBeat: NOW, startedAt: NOW, lastAt: NOW },
  { key: "architect:x", agent: "architect", activityId: "a2", reasoning: "", text: "", tool: "", startedAt: NOW, lastAt: NOW },
] as ActivityEntry[];

function renderConversation(lang: "pt-BR" | "en") {
  resetStore({});
  return renderWith(
    <Conversation
      projectId={PID}
      messages={messages()}
      spec={spec()}
      planReady={spec()}
      onDraftUpdated={async () => {}}
      pending={PENDING}
      resolved={RESOLVED}
      activities={ACTIVITIES}
      dialogError
      tracking
    />,
    { lang },
  );
}

function root(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-conversation]");
  if (el === null) throw new Error("conversa ausente");
  return el;
}

describe("Conversa em en", () => {
  it("nenhum texto da interface em português (mensagens, cartões, escolhas, permissões, composer)", () => {
    renderConversation("en");
    // O comando longo começa truncado: expandir também é texto da interface.
    fireEvent.click(screen.getByRole("button", { name: "Expand command" }));
    expect(portugueseLeftovers(root())).toEqual([]);
    // Amostras do que a tela mostra em en.
    expect(screen.getByRole("textbox", { name: "Message to the Architect" }).getAttribute("placeholder")).toBe("Message the Architect");
    expect(screen.getByRole("button", { name: "Send" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Attach image" })).toBeTruthy();
    expect(screen.getByText(en["chat.tracking"])).toBeTruthy();
    expect(screen.getByRole("button", { name: "Send answers" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "You decide" })).toBeTruthy();
    expect(screen.getAllByText("recommended").length).toBeGreaterThan(0);
    expect(screen.getByText("approved automatically")).toBeTruthy();
    expect(screen.getByText(/attempt 2/)).toBeTruthy();
    expect(screen.getByText("Final verification: 1 of 2 criteria met")).toBeTruthy();
    expect(screen.getByText("Asked for changes")).toBeTruthy();
    expect(screen.getByText("Plan ready")).toBeTruthy();
    expect(screen.getByText("1 task · 1 finding from the Reviewer")).toBeTruthy();
    expect(screen.getByText("Allow a command in the project?")).toBeTruthy();
    expect(screen.getByText("Change the project's Docker environment?")).toBeTruthy();
    expect(screen.getByText("this command runs arbitrary code")).toBeTruthy();
    expect(screen.getByText(/Could not allow it permanently; allowed this time only/)).toBeTruthy();
    expect(screen.getAllByText("Today").length).toBeGreaterThan(0);
    expect(root().textContent).toContain("[autonomous round]");
  });

  it("mensagem do sistema com code aparece pelo code no idioma atual; o conteúdo dos agentes fica como foi escrito", () => {
    renderConversation("en");
    expect(screen.getByText("Task T01 blocked: the worktree /w/T01 has uncommitted changes. Commit or discard them and click Continue.")).toBeTruthy();
    expect(screen.queryByText(/Tarefa T01 bloqueada/)).toBeNull();
    // Conteúdo do agente (e da pessoa): sem tradução.
    expect(screen.getByText("Implemented the API")).toBeTruthy();
    expect(screen.getByText("Build a shop")).toBeTruthy();
  });

  it("pt-BR mostra o mesmo texto de hoje (código traduzido igual à reserva)", () => {
    renderConversation("pt-BR");
    expect(screen.getByText("Tarefa T01 bloqueada: a worktree /w/T01 tem alterações não commitadas. Commite ou descarte-as e clique em Continuar.")).toBeTruthy();
    expect(screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" }).getAttribute("placeholder")).toBe("Mensagem ao Arquiteto");
    expect(screen.getByText("Verificação final: 1 de 2 critérios atendidos")).toBeTruthy();
    expect(screen.getByText("aprovada automaticamente")).toBeTruthy();
    expect(screen.getByText("Plano pronto")).toBeTruthy();
    expect(screen.getByText("1 tarefa · 1 apontamento do Revisor")).toBeTruthy();
    expect(screen.getAllByText("Hoje").length).toBeGreaterThan(0);
    // A varredura enxerga o português (controle do teste em en).
    expect(portugueseLeftovers(root()).length).toBeGreaterThan(0);
  });

  it("a troca de idioma vale na hora para as mensagens já na tela (bolhas em memo)", () => {
    renderConversation("pt-BR");
    expect(screen.getByText("Pediu ajustes")).toBeTruthy();
    act(() => setLanguage("en", { persist: false }));
    expect(screen.getByText("Asked for changes")).toBeTruthy();
    expect(screen.getByText("Task T01 blocked: the worktree /w/T01 has uncommitted changes. Commit or discard them and click Continue.")).toBeTruthy();
    expect(portugueseLeftovers(root())).toEqual([]);
  });

  it("estado vazio e falha do diálogo em en", () => {
    resetStore({});
    renderWith(<Conversation projectId={PID} messages={[]} onDraftUpdated={async () => {}} dialogError />, { lang: "en" });
    expect(screen.getByText(en["conversation.empty"])).toBeTruthy();
    expect(screen.getByText("Could not load the dialog.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
    expect(portugueseLeftovers(root())).toEqual([]);
  });
});

describe("avisos do runner com code (spec 102 T02)", () => {
  it("chegam pelo SSE com code, params e tarefa, e aparecem no idioma ativo", () => {
    resetStore({});
    const event = parseApiEvent({
      event_type: "agent_message",
      project_id: PID,
      payload: {
        agent: "system",
        text: "Aviso: não consegui apagar a worktree /w/T02: busy. A tarefa está integrada; remova a pasta manualmente (git worktree remove --force) quando possível",
        task_id: "T02",
        attempt: null,
        seq: 5,
        spec_id: "s1",
        code: "task.cleanup_failed",
        params: { path: "/w/T02", error: "busy" },
      },
    });
    if (event === null) throw new Error("evento inválido");
    act(() => appStore.applyEvent(event));
    const message = appStore.getState().messages[PID]?.[0];
    expect(message?.code).toBe("task.cleanup_failed");
    expect(message?.params).toEqual({ path: "/w/T02", error: "busy" });
    expect(message?.taskId).toBe("T02");
    if (message === undefined) throw new Error("mensagem ausente");
    expect(systemMessageText(message, "en")).toBe(
      "Warning: could not delete the worktree /w/T02: busy. The task is integrated; remove the folder manually (git worktree remove --force) when possible",
    );
    expect(systemMessageText(message, "pt-BR")).toBe(message.text);
  });

  it("os três codes do runner têm texto nos dois idiomas com os mesmos parâmetros da reserva", () => {
    const params = { task: "T03", path: "/w/T03", error: "EPERM" };
    for (const code of ["task.worktree_dirty", "task.ownership_failed", "task.cleanup_failed"]) {
      const pt = systemMessageText({ text: "x", code, params }, "pt-BR");
      const english = systemMessageText({ text: "x", code, params }, "en");
      expect(pt, code).not.toBe("x");
      expect(english, code).not.toBe("x");
      expect(portugueseLeftovers(Object.assign(document.createElement("p"), { textContent: english }))).toEqual([]);
    }
    expect(systemMessageText({ text: "x", code: "task.ownership_failed", params }, "pt-BR")).toBe(
      "Aviso: não consegui devolver a posse dos arquivos de /w/T03: EPERM",
    );
  });
});

describe("relatório final na Conversa em en", () => {
  const RESULT: SpecResult = {
    spec_id: "s1",
    title: "Shop",
    status: "COMPLETED",
    tasks: [{ id: "T01", title: "Checkout", status: "DONE" } as never, { id: "T02", title: "Cart", status: "DONE", error: "bad" } as never],
    commits: [{ hash: "abc123", subject: "feat: checkout", date: "2026-10-20T10:00:00Z" } as never],
    files: [{ change: "A", path: "src/a.ts" } as never],
    files_truncated: true,
    readme: { text: "# Shop", truncated: true } as never,
    project_path: "/home/dev/shop",
    test_command: "npm test",
  };
  const REPORT: SpecReport = { markdown: "## How to run\n\nnpm start\n\n## Notes\n\nAll good", generated_at: "2026-10-20T10:00:00Z", source: "architect" };

  it("fatos, relatório, avisos e datas no idioma e no formato en", () => {
    renderWith(
      <div data-testid="result">
        <ResultBody result={RESULT} />
        <ReportBody report={REPORT} facts={{ status: "ready", result: RESULT }} onRetryFacts={() => {}} />
        <ReportNotice pending={false} failureCode="timeout" absent failed={false} onRetry={() => {}} />
        <ReportNotice pending={false} failureCode={null} absent={false} failed onRetry={() => {}} />
        <ReportNotice pending failureCode={null} absent={false} failed={false} onRetry={() => {}} />
        <FactsError onRetry={() => {}} />
      </div>,
      { lang: "en" },
    );
    const box = screen.getByTestId("result");
    for (const details of box.querySelectorAll("details")) details.open = true;
    expect(portugueseLeftovers(box)).toEqual([]);
    expect(within(box).getByText("Done: 2 tasks integrated")).toBeTruthy();
    expect(within(box).getByText("The Architect took too long to write the report.")).toBeTruthy();
    // Data pelo formatador da 101 (en: mês/dia/ano), sem pt-BR fixo.
    expect(within(box).getByText(/^Generated by the Architect on 10\/20\/26/)).toBeTruthy();
  });

  it("pt-BR mantém o texto e a data de hoje", () => {
    renderWith(<ReportBody report={REPORT} facts={{ status: "ready", result: RESULT }} onRetryFacts={() => {}} />, { lang: "pt-BR" });
    expect(screen.getByText(/^Gerado pelo Arquiteto em 20\/10\/2026/)).toBeTruthy();
  });
});

describe("subtítulo e selo da Conversa", () => {
  const space = {
    permissions: [{}, {}],
    phase: "executing",
    architectTurnActive: false,
    planStage: null,
  } as never;

  it("em en", () => {
    setLanguage("en", { persist: false });
    expect(conversationSummary({ space, readiness: null, attention: null, conversationUnseen: 1 } as never)).toEqual({
      subtitle: "2 permission requests",
      badge: { count: 3, label: "2 pending items, 1 new message" },
    });
  });

  it("em pt-BR, como hoje", () => {
    expect(conversationSummary({ space, readiness: null, attention: null, conversationUnseen: 1 } as never)).toEqual({
      subtitle: "2 pedidos de permissão",
      badge: { count: 3, label: "2 pendências, 1 mensagem nova" },
    });
    expect(ptBR["conversation.summary.phase"]).toBe("Arquiteto · {phase}");
  });
});

describe("textos montados pelo cliente em en", () => {
  it("falhas do envio e do provedor no idioma ativo", () => {
    setLanguage("en", { persist: false });
    expect(chatFailureMessage({ kind: "http", status: 504, message: "x", reason: "draft_saved" }, null)).toBe(en["chat.timeoutDraftSaved"]);
    expect(chatFailureMessage({ kind: "timeout", message: "x" } as never, null)).toBe(en["chat.clientTimeout"]);
    expect(providerFailureMessage("provider_http_400", "chat", "bad model")).toBe(
      "The provider refused the request (HTTP 400). Check the model and the connection protocol (bad model). The model may require the Responses protocol: change it in Connections, Edit.",
    );
    expect(providerFailureMessage("provider_http_429", null)).toBe(`${en["providerMsg.throttled"]} ${en["providerMsg.throttleAdvice"]}`);
  });

  it("'Decida você' sai no idioma ativo e é reconhecido como resposta nos dois idiomas", () => {
    expect(looksLikeChoicesAnswer([], "Go with the recommendations.")).toBe(true);
    expect(looksLikeChoicesAnswer([], "Siga com as recomendações.")).toBe(true);
    resetStore({});
    renderWith(<Conversation projectId={PID} messages={[{ id: 1, at: NOW, role: "architect", text: CHOICES }]} onDraftUpdated={async () => {}} />, { lang: "en" });
    expect(screen.getByRole("button", { name: "You decide" })).toBeTruthy();
    expect(screen.getByRole("radio", { name: /Yes/ })).toBeTruthy();
    expect(screen.getByText("choose one or more")).toBeTruthy();
  });
});

describe("análise de recuperação no diálogo (role agent, agente architect)", () => {
  it("em en: cabeçalho, rótulos fixos e marca da rodada pelo dicionário; o conteúdo fica como veio", () => {
    renderConversation("en");
    const text = root().textContent ?? "";
    expect(text).toContain("Recovery analysis #1 (RETRY) [autonomous round]");
    expect(text).toContain("Recovery analysis #2 (ESCALATE)");
    expect(text).toContain("Root cause: Flaky selector");
    expect(text).toContain("Decision: escalate to the human");
    expect(text).toContain("Context:");
    expect(text).toContain("command: npm test");
    expect(text).toContain("what was tried: 2 of 2 recovery cycles and 0 of 2 autonomous rounds");
    expect(text).toContain("why it was not solved: (cited by the model) The port stays busy");
    expect(text).toContain("Root cause: recovery cycle limit reached");
    expect(text).toContain("Autonomous round (auto-approve) #1 of 2");
    // Selo da aprovação automática também no cartão do diálogo.
    expect(screen.getAllByTestId("yolo-badge").length).toBeGreaterThan(0);
    expect(portugueseLeftovers(root())).toEqual([]);
  });

  it("em pt-BR nada muda: o texto gravado aparece idêntico", () => {
    renderConversation("pt-BR");
    const text = root().textContent ?? "";
    expect(text).toContain("Análise de recuperação #1 (RETRY) [rodada autônoma]");
    expect(text).toContain("Análise de recuperação #2 (ESCALATE)");
    expect(text).toContain("Causa raiz: limite de ciclos de recuperação atingido");
    expect(text).toContain("o que foi tentado: 2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas");
    expect(text).toContain("Contagem: não conta como ciclo (escalada determinística)");
    expect(localizeRecoveryAnalysis(RECOVERY_ESCALATE, "pt-BR")).toBe(RECOVERY_ESCALATE);
    expect(localizeRecoveryAnalysis("Texto livre do agente", "en")).toBe("Texto livre do agente");
  });
});
