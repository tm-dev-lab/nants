/* Relatório final na janela Conversa (spec 093, T06). */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useEffect } from "react";
import { Conversation } from "./Conversation";
import { useLiveStream } from "../../liveStream";
import { pickActiveSpec } from "../../phase";
import { refreshProjectState } from "../../projectState";
import { appStore, useAppStore, type ChatMessage } from "../../store";
import type { ProjectState, SpecStatus } from "../../api/types";
import {
  installFakeEventSource,
  installFetch,
  makeConfig,
  makeProject,
  makeProjectState,
  makeSpec,
  makeTask,
  resetStore,
} from "../../test-utils";

/** Assinatura global montada como no desktop (`useDesktopRuntime`). */
function GlobalStream(): null {
  useLiveStream();
  return null;
}

const PROJECT_ID = "p1";
const SPEC_ID = "001-x";

/** JSON literal no formato real do servidor (spec_result.rs). */
function serverResult(overrides: Record<string, unknown> = {}) {
  return {
    spec_id: SPEC_ID,
    title: "Api de pagamentos",
    status: "COMPLETED",
    tasks: [
      { id: "T01", title: "Modelo", status: "DONE" },
      { id: "T02", title: "Rotas", status: "DONE" },
    ],
    commits: [
      { hash: "8d0d36f", subject: "feat(001-x): T02 - Rotas", task_id: "T02", date: "2026-10-14T13:05:00Z" },
      { hash: "1a2b3c4", subject: "feat(001-x): T01 - Modelo", task_id: "T01", date: "2026-10-14T12:00:00Z" },
    ],
    files: [
      { path: "src/app.js", change: "A" },
      { path: "README.md", change: "M" },
      { path: "old.js", change: "D" },
    ],
    files_truncated: false,
    readme: { text: "# Projeto\n\nRode npm test.", truncated: false },
    project_path: "/home/user/projetos/api",
    test_command: "node --test",
    ...overrides,
  };
}

function completedState(status: SpecStatus = "COMPLETED"): ProjectState {
  return makeProjectState({
    specs: [
      makeSpec(SPEC_ID, "Api de pagamentos", status, [
        makeTask("T01", "Modelo", "DONE"),
        makeTask("T02", "Rotas", "DONE"),
      ]),
    ],
  });
}

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

function configured(): void {
  resetStore({ config: makeConfig([makeProject(PROJECT_ID, "api-pagamentos")]) });
}

/** Rotas: `/state` e `/chat` fixos; `/result` delegado para o teste. */
function routes(
  state: () => ProjectState,
  result: (call: number) => { status?: number; body?: unknown } | Promise<{ status?: number; body?: unknown }>,
) {
  let resultCalls = 0;
  const fetchMock = installFetch(async (method, url) => {
    if (method === "GET" && url.endsWith("/state")) return { body: state() };
    if (method === "GET" && url.endsWith("/chat")) return { body: { turns: [] } };
    if (method === "GET" && url.endsWith("/result")) {
      resultCalls += 1;
      return result(resultCalls);
    }
    return { status: 404, body: { error: "projeto não encontrado", code: "project_not_found" } };
  });
  return { fetchMock, resultCalls: () => resultCalls };
}

const resultCallsIn = (fetchMock: ReturnType<typeof installFetch>) =>
  fetchMock.mock.calls.filter((call) => String(call[0]).endsWith("/result")).length;

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  // jsdom não tem execCommand: o teste de fallback do clipboard define um stub temporário.
  delete (document as { execCommand?: unknown }).execCommand;
});

beforeEach(() => {
  installFakeEventSource();
  configured();
});

describe("ResultMessage: conteúdo", () => {
  it("mostra cabeçalho, arquivos com A/M/D, como executar, testes, commits, caminho e botão copiar", async () => {
    const { fetchMock } = routes(completedState, () => ({ body: serverResult() }));
    render(<Screen />);

    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(within(region).getByText("Pronto: 2 tarefas integradas")).not.toBeNull();
    expect(within(region).getByText("Api de pagamentos")).not.toBeNull();

    expect(within(region).getByRole("heading", { name: "O que foi criado" })).not.toBeNull();
    const files = within(region).getByRole("list", { name: "Arquivos" });
    const items = within(files).getAllByRole("listitem");
    expect(items.map((li) => li.textContent)).toEqual([
      expect.stringMatching(/^A\s*src\/app\.js$/),
      expect.stringMatching(/^M\s*README\.md$/),
      expect.stringMatching(/^D\s*old\.js$/),
    ]);

    expect(within(region).getByRole("heading", { name: "Como executar" })).not.toBeNull();
    expect(within(region).getByText("Trecho do início do README")).not.toBeNull();
    const pre = region.querySelector("pre");
    expect(pre?.textContent).toBe("# Projeto\n\nRode npm test.");
    expect(within(region).getByText("Testes: node --test")).not.toBeNull();

    expect(within(region).getByRole("heading", { name: "Histórico de integração" })).not.toBeNull();
    const commits = within(region).getByRole("list", { name: "Commits" });
    const commitItems = within(commits).getAllByRole("listitem");
    expect(commitItems).toHaveLength(2);
    expect(commitItems[0]?.querySelector("code")?.textContent).toBe("8d0d36f");
    expect(commitItems[0]?.textContent).toContain("feat(001-x): T02 - Rotas");
    expect(commitItems[0]?.querySelector("time")?.getAttribute("dateTime")).toBe("2026-10-14T13:05:00Z");

    expect(within(region).getByRole("heading", { name: "Pasta do projeto" })).not.toBeNull();
    expect(within(region).getByText("/home/user/projetos/api")).not.toBeNull();
    expect(within(region).getByRole("button", { name: "Copiar caminho" })).not.toBeNull();

    expect(resultCallsIn(fetchMock)).toBe(1);
  });

  it("lista tarefas e marca 'arquivo inválido' ao lado do id quando a tarefa tem error", async () => {
    routes(completedState, () => ({
      body: serverResult({
        tasks: [
          { id: "T01", title: "Modelo", status: "DONE" },
          { id: "T02", title: "Rotas", status: "DONE", error: "arquivo inválido", code: "file_invalid" },
        ],
      }),
    }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    const tasks = within(region).getByRole("list", { name: "Tarefas" });
    const t02 = within(tasks).getByText("T02").closest("li");
    expect(t02?.textContent).toContain("arquivo inválido");
    expect(within(tasks).getByText("T01").closest("li")?.textContent).not.toContain("arquivo inválido");
  });

  it("mostra 'mais arquivos' quando a lista de arquivos foi truncada", async () => {
    routes(completedState, () => ({ body: serverResult({ files_truncated: true }) }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(within(region).getByText(/mais arquivos/)).not.toBeNull();
  });

  it("README truncado: avisa que o trecho foi cortado (sem caractere de reticências)", async () => {
    routes(completedState, () => ({
      body: serverResult({ readme: { text: "linha 1\nlinha 2", truncated: true } }),
    }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(region.querySelector("pre")?.textContent).toBe("linha 1\nlinha 2");
    expect(within(region).getByText("Trecho cortado: o README continua no projeto.")).not.toBeNull();
  });

  it("README null mostra a mensagem fixa e não exibe bloco de código", async () => {
    routes(completedState, () => ({ body: serverResult({ readme: null }) }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(within(region).getByText("O projeto não tem README.md na raiz.")).not.toBeNull();
    expect(region.querySelector("pre")).toBeNull();
  });

  it("sem test_command não mostra a linha de testes", async () => {
    routes(completedState, () => ({ body: serverResult({ test_command: null }) }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(within(region).queryByText(/^Testes:/)).toBeNull();
  });

  it("mostra estado de carregamento enquanto o GET não responde", async () => {
    let release: (value: { body: unknown }) => void = () => undefined;
    const pending = new Promise<{ body: unknown }>((resolve) => {
      release = resolve;
    });
    routes(completedState, () => pending);
    render(<Screen />);
    expect(await screen.findByText("Carregando resultado")).not.toBeNull();
    release({ body: serverResult() });
    expect(await screen.findByRole("region", { name: "Relatório final" })).not.toBeNull();
  });
});

describe("ResultMessage: falhas", () => {
  it("falha do GET mostra cartão discreto e 'Tentar de novo' recarrega sem quebrar a tela", async () => {
    const user = userEvent.setup();
    const { fetchMock } = routes(completedState, (call) =>
      call === 1 ? { status: 500, body: { error: "falha interna", code: "internal" } } : { body: serverResult() },
    );
    render(<Screen />);

    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(within(region).getByText("Não foi possível carregar o resultado.")).not.toBeNull();
    await user.click(within(region).getByRole("button", { name: "Tentar de novo" }));

    expect(await within(region).findByText("Pronto: 2 tarefas integradas")).not.toBeNull();
    expect(resultCallsIn(fetchMock)).toBe(2);
  });

  it("corpo inválido do GET mostra a mesma falha tratada", async () => {
    routes(completedState, () => ({ body: { spec_id: SPEC_ID } }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(within(region).getByText("Não foi possível carregar o resultado.")).not.toBeNull();
  });
});

describe("ResultMessage: visibilidade", () => {
  it.each<SpecStatus>(["IN_PROGRESS", "AWAITING_APPROVAL", "DRAFT", "BLOCKED"])(
    "não aparece nem busca o resultado em %s",
    async (status) => {
      const { fetchMock } = routes(() => completedState(status), () => ({ body: serverResult() }));
      render(<Screen />);
      await waitFor(() => expect(fetchMock.mock.calls.length).toBeGreaterThan(0));
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(screen.queryByRole("region", { name: "Relatório final" })).toBeNull();
      expect(resultCallsIn(fetchMock)).toBe(0);
    },
  );

});

describe("ResultMessage: carga única por spec", () => {
  it("não refaz o GET a cada re-render do projeto", async () => {
    const { fetchMock } = routes(completedState, () => ({ body: serverResult() }));
    render(<Screen />);
    await screen.findByRole("region", { name: "Relatório final" });
    expect(resultCallsIn(fetchMock)).toBe(1);

    // Novo objeto de estado com a mesma spec COMPLETED: re-render, sem nova carga.
    appStore.setState((s) => ({ projectStates: { ...s.projectStates, [PROJECT_ID]: completedState() } }));
    appStore.setState((s) => ({ revisions: { ...s.revisions, [PROJECT_ID]: (s.revisions[PROJECT_ID] ?? 0) + 1 } }));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(resultCallsIn(fetchMock)).toBe(1);
  });

  it("carrega ao passar a COMPLETED (não antes)", async () => {
    let current: ProjectState = completedState("IN_PROGRESS");
    const { fetchMock } = routes(
      () => current,
      () => ({ body: serverResult() }),
    );
    render(<Screen />);
    await waitFor(() => expect(fetchMock.mock.calls.length).toBeGreaterThan(0));
    expect(resultCallsIn(fetchMock)).toBe(0);

    current = completedState("COMPLETED");
    appStore.setState((s) => ({ projectStates: { ...s.projectStates, [PROJECT_ID]: current } }));
    expect(await screen.findByRole("region", { name: "Relatório final" })).not.toBeNull();
    expect(resultCallsIn(fetchMock)).toBe(1);
  });
});

describe("ResultMessage: títulos longos", () => {
  it("título sem espaços (128 caracteres) quebra linha: classe de quebra no elemento", async () => {
    const longTitle = "a".repeat(128);
    routes(completedState, () => ({ body: serverResult({ title: longTitle }) }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    const el = within(region).getByText(longTitle);
    expect(el.className).toContain("nd-break");
  });
});

describe("ResultMessage: segurança de renderização", () => {
  it("README e assuntos com HTML/script aparecem como texto", async () => {
    const payload = "<img src=x onerror=alert(1)><script>alert(2)</script>";
    routes(completedState, () => ({
      body: serverResult({
        readme: { text: payload, truncated: false },
        commits: [{ hash: "8d0d36f", subject: payload, task_id: "T02", date: "2026-10-14T13:05:00Z" }],
      }),
    }));
    const { container } = render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });
    expect(region.querySelector("pre")?.textContent).toBe(payload);
    expect(within(region).getByText(payload, { exact: false, selector: "span" })).not.toBeNull();
    expect(container.querySelector("img, script, iframe, [onerror]")).toBeNull();
  });
});

describe("ResultMessage: copiar caminho", () => {
  it("copia o caminho pela Clipboard API e mostra 'Copiado'", async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    routes(completedState, () => ({ body: serverResult() }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });

    fireEvent.click(within(region).getByRole("button", { name: "Copiar caminho" }));
    expect(writeText).toHaveBeenCalledWith("/home/user/projetos/api");
    expect(await within(region).findByRole("button", { name: "Copiado" })).not.toBeNull();
  });

  it("falha do clipboard não quebra a tela nem mostra erro intrusivo", async () => {
    const writeText = vi.fn(async () => {
      throw new Error("denied");
    });
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    Object.defineProperty(document, "execCommand", { value: vi.fn(() => false), configurable: true });
    routes(completedState, () => ({ body: serverResult() }));
    render(<Screen />);
    const region = await screen.findByRole("region", { name: "Relatório final" });

    fireEvent.click(within(region).getByRole("button", { name: "Copiar caminho" }));
    await waitFor(() => expect(writeText).toHaveBeenCalled());
    expect(within(region).getByRole("button", { name: "Copiar caminho" })).not.toBeNull();
    expect(within(region).queryByText("Copiado")).toBeNull();
  });
});
