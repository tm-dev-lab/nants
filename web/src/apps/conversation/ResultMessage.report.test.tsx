/* Relatório do Arquiteto na janela Conversa (spec 093, T06). */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useEffect } from "react";
import { Conversation } from "./Conversation";
import { useLiveStream } from "../../liveStream";
import { pickActiveSpec } from "../../phase";
import { refreshProjectState } from "../../projectState";
import { appStore, useAppStore, type ChatMessage } from "../../store";
import type { ApiEvent, ProjectState, SpecStatus } from "../../api/types";
import {
  FakeEventSource,
  installFakeEventSource,
  installFetch,
  makeConfig,
  makeProject,
  makeProjectState,
  makeSpec,
  makeTask,
  resetStore,
} from "../../test-utils";
import { formatDate, hasEvidence } from "./ResultMessage";

/** Assinatura global montada como no desktop (`useDesktopRuntime`). */
function GlobalStream(): null {
  useLiveStream();
  return null;
}

const PROJECT_ID = "p1";
const SPEC_ID = "001-x";
const OTHER_SPEC_ID = "002-y";
const GENERATED_AT = "2026-10-14T13:05:00Z";

type Reply = { status?: number; body?: unknown };

function serverResult(overrides: Record<string, unknown> = {}) {
  return {
    spec_id: SPEC_ID,
    title: "Api de pagamentos",
    status: "COMPLETED",
    tasks: [
      { id: "T01", title: "Modelo", status: "DONE" },
      { id: "T02", title: "Rotas", status: "DONE" },
    ],
    commits: [{ hash: "8d0d36f", subject: "feat(001-x): T02 - Rotas", task_id: "T02", date: GENERATED_AT }],
    files: [{ path: "src/app.js", change: "A" }],
    files_truncated: false,
    readme: { text: "# README antigo", truncated: false },
    project_path: "/home/user/projetos/api",
    test_command: "node --test",
    ...overrides,
  };
}

const REPORT_MD = [
  "# Api de pagamentos",
  "",
  "Resumo do que foi entregue.",
  "",
  "## Como executar",
  "",
  "1. Instale as dependências com `npm ci`.",
  "2. Rode o servidor:",
  "   - comando `npm start`",
  "   - abra http://localhost:3000",
  "3. Pare com Ctrl+C.",
  "",
  "## Decisões",
  "",
  "- Usou SQLite.",
].join("\n");

function stateOf(status: SpecStatus, reportPending = false, specId = SPEC_ID): ProjectState {
  return makeProjectState({
    specs: [makeSpec(specId, "Api de pagamentos", status, [makeTask("T01", "Modelo", "DONE")])],
    report_pending: reportPending,
  });
}

interface Options {
  state?: () => ProjectState;
  report?: (call: number) => Reply | Promise<Reply>;
  result?: () => Reply;
}

function routes({ state = () => stateOf("COMPLETED"), report, result = () => ({ body: serverResult() }) }: Options) {
  let reportCalls = 0;
  const fetchMock = installFetch(async (method, url) => {
    if (method === "GET" && url.endsWith("/state")) return { body: state() };
    if (method === "GET" && url.endsWith("/chat")) return { body: { turns: [] } };
    if (method === "GET" && url.endsWith("/result")) return result();
    if (method === "GET" && url.endsWith("/report")) {
      reportCalls += 1;
      return report ? report(reportCalls) : { status: 404, body: { error: "relatório não encontrado", code: "report_not_found" } };
    }
    return { status: 404, body: { error: "não encontrado", code: "not_found" } };
  });
  return { fetchMock, reportCalls: () => reportCalls, reportUrls: () => fetchMock.mock.calls.map((c) => String(c[0])).filter((u) => u.endsWith("/report")) };
}

/** Entrega o evento pelo stream SSE (como o servidor envia). */
function emit(event: ApiEvent): void {
  FakeEventSource.instances[0]?.emit(event.event_type, event);
}

const reportReady = (specId = SPEC_ID): ApiEvent => ({
  event_type: "report_ready",
  project_id: PROJECT_ID,
  payload: { spec_id: specId },
});

const reportFailed = (code: string, specId = SPEC_ID): ApiEvent => ({
  event_type: "report_failed",
  project_id: PROJECT_ID,
  payload: { spec_id: specId, code },
});

const NO_MESSAGES: readonly ChatMessage[] = [];

/** Conversa do projeto com o estado do servidor (recarregado a cada aviso), como o shell monta. */
function ConversationScreen() {
  const state = useAppStore((s) => s.projectStates[PROJECT_ID]);
  const revision = useAppStore((s) => s.revisions[PROJECT_ID] ?? 0);
  useEffect(() => {
    void refreshProjectState(PROJECT_ID);
  }, [revision]);
  const spec = state === undefined ? null : pickActiveSpec(state.specs);
  return state === undefined ? null : (
    <Conversation
      projectId={PROJECT_ID}
      spec={spec}
      messages={NO_MESSAGES}
      onDraftUpdated={async () => {
        await refreshProjectState(PROJECT_ID);
      }}
      reportPending={state.report_pending === true}
    />
  );
}

function Screen() {
  return (
    <>
      <GlobalStream />
      <ConversationScreen />
    </>
  );
}

/** Aguarda o estado final (relatório carregado), não só o painel de fatos. */
async function findReportRegion(): Promise<HTMLElement> {
  const region = await screen.findByRole("region", { name: "Relatório final" });
  await within(region).findByRole("heading", { name: "Relatório final" });
  return region;
}

const okReport = (markdown = REPORT_MD) => ({ body: { markdown, generated_at: GENERATED_AT, source: "architect" } });

afterEach(() => {
  cleanup();
});

beforeEach(() => {
  installFakeEventSource();
  resetStore({ config: makeConfig([makeProject(PROJECT_ID, "api-pagamentos")]) });
});

describe("relatório: exibição", () => {
  it("mostra cabeçalho, data, 'Como executar' destacado e 'Evidências' recolhida", async () => {
    routes({ report: () => okReport() });
    render(<Screen />);
    const region = await findReportRegion();
    expect(within(region).getByRole("heading", { name: "Relatório final" })).not.toBeNull();
    expect(within(region).getByText(`Gerado pelo Arquiteto em ${formatDate(GENERATED_AT)}`)).not.toBeNull();

    const howTo = within(region).getByRole("region", { name: "Como executar" });
    expect(howTo.textContent).toContain("Instale as dependências com npm ci.");
    expect(howTo.querySelectorAll("ol > li")).toHaveLength(3);
    expect(within(howTo).getByText("npm start", { exact: false })).not.toBeNull();

    expect(within(region).getByRole("heading", { name: "Decisões" })).not.toBeNull();
    expect(within(region).getByText("Usou SQLite.")).not.toBeNull();

    const evidence = within(region).getByText("Evidências").closest("details");
    expect(evidence).not.toBeNull();
    expect(within(region).getByText("Evidências").querySelector(".nd-details-chevron")).not.toBeNull();
    expect(evidence?.open).toBe(false);
    expect(within(evidence as HTMLElement).getByText("Histórico de integração")).not.toBeNull();
    expect(within(evidence as HTMLElement).getByText("Testes: node --test")).not.toBeNull();
  });

  it("A-10 (094/T08): 'Evidências' tem o chevron de recolhível; sem evidência nenhuma, o bloco não aparece", async () => {
    const { fetchMock } = routes({ report: () => okReport(), result: () => ({ body: serverResult({ tasks: [], commits: [], files: [], test_command: null }) }) });
    render(<Screen />);
    const region = await findReportRegion();
    // Os fatos (GET /result) já chegaram: o bloco some por falta de evidência, não por carregar.
    await waitFor(() => expect(fetchMock.mock.calls.some((c) => String(c[0]).endsWith("/result"))).toBe(true));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(within(region).queryByText("Evidências")).toBeNull();
    expect(region.querySelector("[data-evidence]")).toBeNull();
  });

  it("A-10 (094/T08): hasEvidence pede tarefa, arquivo, commit ou comando de testes", () => {
    const empty = { ...serverResult({ tasks: [], commits: [], files: [], test_command: null }) } as Parameters<typeof hasEvidence>[0];
    expect(hasEvidence(empty)).toBe(false);
    expect(hasEvidence({ ...empty, test_command: "" })).toBe(false);
    expect(hasEvidence({ ...empty, test_command: "npm test" })).toBe(true);
    expect(hasEvidence({ ...empty, files: [{ path: "a", change: "A" }] } as Parameters<typeof hasEvidence>[0])).toBe(true);
    expect(hasEvidence(serverResult() as Parameters<typeof hasEvidence>[0])).toBe(true);
  });

  it("não mostra o README antigo quando há relatório (o trecho de 'Como executar' é o do relatório)", async () => {
    routes({ report: () => okReport() });
    render(<Screen />);
    const region = await findReportRegion();
    expect(within(region).queryByText("# README antigo")).toBeNull();
    expect(within(region).queryByText("Trecho do início do README")).toBeNull();
  });

  it("sem seção 'Como executar' no relatório, não há a seção destacada", async () => {
    routes({ report: () => okReport("# Só resumo\n\nTexto.") });
    render(<Screen />);
    const region = await findReportRegion();
    expect(within(region).getByText("Texto.")).not.toBeNull();
    expect(within(region).queryByRole("region", { name: "Como executar" })).toBeNull();
  });

  it("XSS no relatório vira texto: sem script, img, onerror nem link javascript:", async () => {
    const hostile = [
      "<script>alert(1)</script>",
      "",
      "<img src=x onerror=alert(2)>",
      "",
      "[x](javascript:alert(3))",
      "",
      "![i](http://x)",
      "",
      "```html",
      "<script>alert(4)</script>",
      "```",
    ].join("\n");
    routes({ report: () => okReport(hostile) });
    const { container } = render(<Screen />);
    const region = await findReportRegion();
    expect(container.querySelector("script, img, iframe, [onerror]")).toBeNull();
    expect(region.querySelector("a[href^='javascript']")).toBeNull();
    expect(region.textContent).toContain("<script>alert(1)</script>");
    expect(region.querySelector("pre code")?.textContent).toBe("<script>alert(4)</script>");
  });

  it("relatório muito longo (acima de 64 KiB) renderiza", async () => {
    const big = `${REPORT_MD}\n\n${"Parágrafo de apoio com `código` e **ênfase**.\n\n".repeat(2000)}`;
    expect(big.length).toBeGreaterThan(64 * 1024);
    routes({ report: () => okReport(big) });
    render(<Screen />);
    const region = await findReportRegion();
    expect(within(region).getByRole("region", { name: "Como executar" })).not.toBeNull();
  });
});

describe("relatório: estados", () => {
  it("report_pending com 404 mostra 'escrevendo' (role=status) e o painel de fatos", async () => {
    routes({ state: () => stateOf("COMPLETED", true) });
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    const pending = await within(region).findByText("O Arquiteto está escrevendo o relatório…");
    expect(pending.getAttribute("role")).toBe("status");
    expect(within(region).getByText("Pronto: 2 tarefas integradas")).not.toBeNull();
  });

  it("report_ready carrega o relatório e troca o painel de fatos pelo relatório", async () => {
    const pending = { value: true };
    const { reportCalls } = routes({
      state: () => stateOf("COMPLETED", pending.value),
      report: (call) => (call === 1 ? { status: 404, body: {} } : okReport()),
    });
    render(<Screen />);
    await screen.findByText("O Arquiteto está escrevendo o relatório…");
    pending.value = false;
    emit(reportReady());
    expect(await screen.findByRole("heading", { name: "Relatório final" })).not.toBeNull();
    expect(reportCalls()).toBe(2);
  });

  it("report_failed mostra nota discreta com o motivo em pt-BR e mantém o painel de fatos", async () => {
    routes({ state: () => stateOf("COMPLETED", false) });
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    emit(reportFailed("git_unsupported"));
    expect(await within(region).findByText("Repositórios em worktree não geram relatório.")).not.toBeNull();
    expect(within(region).getByText("Pronto: 2 tarefas integradas")).not.toBeNull();
    expect(within(region).queryByRole("heading", { name: "Relatório final" })).toBeNull();
  });

  it.each([
    ["not_configured", "Configure o Arquiteto para gerar o relatório."],
    ["provider_http_400", "O modelo não respondeu ao gerar o relatório."],
    ["codigo_desconhecido", "O relatório não pôde ser gerado."],
  ])("código %s vira mensagem fixa", async (code, message) => {
    routes({ state: () => stateOf("COMPLETED", false) });
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    emit(reportFailed(code));
    expect(await within(region).findByText(message)).not.toBeNull();
  });

  it("404 sem pendência mostra nota neutra e o painel de fatos", async () => {
    routes({});
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(await within(region).findByText("O relatório não foi gerado para esta spec.")).not.toBeNull();
    expect(within(region).getByText("Pronto: 2 tarefas integradas")).not.toBeNull();
    expect(within(region).queryByRole("status")).toBeNull();
  });

  it("falha, desmontar, montar de novo: a nota com o motivo continua (sinal da sessão)", async () => {
    routes({ state: () => stateOf("COMPLETED", false) });
    const first = render(<Screen />);
    await screen.findByRole("region", { name: "Relatório final" });
    emit(reportFailed("git_unsupported"));
    expect(await screen.findByText("Repositórios em worktree não geram relatório.")).not.toBeNull();
    first.unmount();

    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(await within(region).findByText("Repositórios em worktree não geram relatório.")).not.toBeNull();
  });

  it("falha antiga não reaparece depois de um relatório pronto", async () => {
    routes({
      state: () => stateOf("COMPLETED", false),
      report: (call) => (call === 1 ? { status: 404, body: {} } : okReport()),
    });
    const first = render(<Screen />);
    await screen.findByRole("region", { name: "Relatório final" });
    emit(reportFailed("timeout"));
    expect(await screen.findByText("O Arquiteto demorou demais para escrever o relatório.")).not.toBeNull();
    emit(reportReady());
    expect(await screen.findByRole("heading", { name: "Relatório final" })).not.toBeNull();
    first.unmount();

    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(await within(region).findByRole("heading", { name: "Relatório final" })).not.toBeNull();
    expect(within(region).queryByText("O Arquiteto demorou demais para escrever o relatório.")).toBeNull();
  });

  it("falha de rede no relatório mostra 'Tentar de novo' discreto que recarrega", async () => {
    const user = userEvent.setup();
    const { reportCalls } = routes({
      report: (call) => (call === 1 ? { status: 500, body: { error: "falha" } } : okReport()),
    });
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    await user.click(await within(region).findByRole("button", { name: "Tentar de novo" }));
    expect(await within(region).findByRole("heading", { name: "Relatório final" })).not.toBeNull();
    expect(reportCalls()).toBe(2);
  });
});

describe("relatório: sinais por spec (F-09)", () => {
  it("falha da spec A continua visível quando a spec B recebe report_ready", async () => {
    routes({ state: () => stateOf("COMPLETED", false, SPEC_ID) });
    render(<Screen />);
    await screen.findByRole("region", { name: "Relatório final" });
    emit(reportFailed("timeout", SPEC_ID));
    expect(await screen.findByText("O Arquiteto demorou demais para escrever o relatório.")).not.toBeNull();
    emit(reportReady(OTHER_SPEC_ID));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(screen.getByText("O Arquiteto demorou demais para escrever o relatório.")).not.toBeNull();
  });
});

describe("relatório: carga única por (projeto, spec)", () => {
  it("não refaz o GET do relatório a cada re-render", async () => {
    const { reportCalls } = routes({ report: () => okReport() });
    render(<Screen />);
    await screen.findByRole("heading", { name: "Relatório final" });
    expect(reportCalls()).toBe(1);
    appStore.setState((s) => ({ projectStates: { ...s.projectStates, [PROJECT_ID]: stateOf("COMPLETED") } }));
    appStore.setState((s) => ({ revisions: { ...s.revisions, [PROJECT_ID]: (s.revisions[PROJECT_ID] ?? 0) + 1 } }));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(reportCalls()).toBe(1);
  });

  it("report_ready de outra spec não recarrega esta", async () => {
    const { reportCalls } = routes({ report: () => okReport() });
    render(<Screen />);
    await screen.findByRole("heading", { name: "Relatório final" });
    emit(reportReady(OTHER_SPEC_ID));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(reportCalls()).toBe(1);
  });
});

describe("relatório: identidade de spec e respostas atrasadas", () => {
  it("resposta atrasada da spec anterior não aparece depois da troca de spec", async () => {
    let releaseOld: (reply: Reply) => void = () => undefined;
    const oldReply = new Promise<Reply>((resolve) => {
      releaseOld = resolve;
    });
    let current = stateOf("COMPLETED", false, SPEC_ID);
    routes({
      state: () => current,
      report: (call) => (call === 1 ? oldReply : okReport("# Relatório novo\n\nDa spec nova.")),
    });
    render(<Screen />);
    await waitFor(() => expect(screen.getByText("Api de pagamentos")).not.toBeNull());

    // A spec ativa muda antes da resposta de SPEC_ID chegar.
    current = stateOf("COMPLETED", false, OTHER_SPEC_ID);
    appStore.setState((s) => ({ projectStates: { ...s.projectStates, [PROJECT_ID]: current } }));
    releaseOld({ body: { markdown: "# Relatório ANTIGO", generated_at: GENERATED_AT, source: "architect" } });

    expect(await screen.findByText("Da spec nova.")).not.toBeNull();
    expect(screen.queryByText("Relatório ANTIGO")).toBeNull();
  });
});

describe("relatório: não quebra o layout", () => {
  it("a mensagem fica na rolagem da conversa e sem rolagem aninhada além dos blocos de código", async () => {
    routes({ report: () => okReport() });
    render(<Screen />);
    const region = await findReportRegion();
    expect(region.closest("[data-chat-scroll]")).not.toBeNull();
  });
});
