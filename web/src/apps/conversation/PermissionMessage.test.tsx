/*
 * Pedido de permissão na janela Conversa (spec 093, T06): a chegada do pedido não move o foco,
 * o comando truncado não leva reticências e as cores vêm dos tokens.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { PermissionMessage, COMMAND_PREVIEW_CHARS } from "./PermissionMessage";
import { appStore, useAppStore } from "../../store";
import type { PendingPermission, ResolvedPermission } from "../../store";
import type { ApiEvent } from "../../api/types";
import { permissionEntries } from "../../permissionTimeline";
import { installFetch, resetStore } from "../../test-utils";
import { findForbiddenGlyphs } from "../../desktop/glyphs";

const PID = "p1";
const EMPTY: readonly PendingPermission[] = [];
const EMPTY_RESOLVED: readonly ResolvedPermission[] = [];

function req(id: string, command: string, extra: Partial<PendingPermission> = {}): PendingPermission {
  return { requestId: id, projectId: PID, taskId: "T01", agent: "developer", command, at: Date.now(), ...extra };
}

/** Mesma montagem da conversa: pendentes e resolvidos do store, em ordem cronológica. */
function Harness() {
  const pending = useAppStore((s) => s.permissions[PID] ?? EMPTY);
  const resolved = useAppStore((s) => s.resolvedPermissions[PID] ?? EMPTY_RESOLVED);
  return (
    <ul aria-label="Conversa">
      {permissionEntries(pending, resolved).map((entry) => (
        <li key={entry.requestId}>
          <PermissionMessage projectId={PID} entry={entry} />
        </li>
      ))}
    </ul>
  );
}

/** Chega pelo mesmo caminho do SSE (`applyEvent`), com o payload literal do servidor. */
function arrive(payload: Record<string, unknown>): void {
  const event = { event_type: "permission_requested", project_id: PID, payload } as unknown as ApiEvent;
  act(() => appStore.applyEvent(event));
}

function resolveByEvent(requestId: string, applied: string): void {
  const event = {
    event_type: "permission_resolved",
    project_id: PID,
    payload: { request_id: requestId, project_id: PID, applied },
  } as unknown as ApiEvent;
  act(() => appStore.applyEvent(event));
}

function seed(list: PendingPermission[]): void {
  resetStore({ permissions: { [PID]: list }, phases: { [PID]: "waiting_approval" } });
}

function groups(): HTMLElement[] {
  return screen.queryAllByRole("group", { name: "Decisão sobre o comando" });
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("PermissionMessage: decisões", () => {
  beforeEach(() => resetStore());

  it.each([
    ["Permitir uma vez", "once", "Liberado só desta vez"],
    ["Permitir nesta execução", "run", "Liberado nesta execução"],
    ["Negar", "deny", "Negado"],
  ])("'%s' envia decision %s e a mesma mensagem vira '%s' sem botões", async (label, decision, outcome) => {
    seed([req("r1", "cargo clippy -- -D warnings")]);
    const fetchMock = installFetch(() => ({ body: { ok: true, applied: decision } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: label }));

    expect(await screen.findByText(new RegExp(`^${outcome} ·`))).not.toBeNull();
    expect(groups()).toHaveLength(0);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(url).toBe(`/api/projects/${PID}/permissions/r1`);
    expect(init?.method).toBe("POST");
    expect(init?.body).toBe(JSON.stringify({ decision }));
    // Sem pedidos restantes, a fase armazenada sai e volta a ser a derivada.
    expect(appStore.getState().phases[PID]).toBeUndefined();
    // O pedido continua na conversa como resolvido, com o escopo aplicado.
    expect(appStore.getState().resolvedPermissions[PID]?.map((r) => [r.requestId, r.applied])).toEqual([
      ["r1", decision],
    ]);
  });

  it("404 (já resolvido em outra janela) vira resolvido sem decisão e sem erro", async () => {
    seed([req("r1", "cargo test")]);
    installFetch(() => ({ status: 404, body: { error: "pedido de permissão não encontrado" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Permitir uma vez" }));

    expect(await screen.findByText(/Não está mais pendente/)).not.toBeNull();
    expect(groups()).toHaveLength(0);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("erro do servidor mantém o pedido com botões e mostra a mensagem do erro nele", async () => {
    seed([req("r1", "cargo test")]);
    installFetch(() => ({ status: 500, body: { error: "falha interna" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Negar" }));

    expect((await screen.findByRole("alert")).textContent).toBe("falha interna");
    expect(groups()).toHaveLength(1);
    expect(appStore.getState().permissions[PID]?.map((p) => p.requestId)).toEqual(["r1"]);
    expect(appStore.getState().resolvedPermissions[PID] ?? []).toEqual([]);
  });

  it("'Sempre permitir' com always_allowed: envia always e mostra 'Liberado permanentemente'", async () => {
    arrive({ request_id: "r1", project_id: PID, task_id: "T01", agent: "developer", command: "git status", always_allowed: true, always_denied_reason: null });
    const fetchMock = installFetch(() => ({ body: { ok: true, applied: "always" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Sempre permitir" }));

    expect(fetchMock.mock.calls[0]?.[1]?.body).toBe(JSON.stringify({ decision: "always" }));
    expect(await screen.findByText(/^Liberado permanentemente ·/)).not.toBeNull();
  });

  it("'Sempre' aplicado como run mostra a nota de liberação nesta execução na mesma mensagem", async () => {
    arrive({ request_id: "r1", project_id: PID, task_id: "T01", agent: "developer", command: "git status", always_allowed: true, always_denied_reason: null });
    installFetch(() => ({ body: { ok: true, applied: "run" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Sempre permitir" }));

    expect(
      await screen.findByText(/^Não foi possível liberar permanentemente; liberado nesta execução ·/),
    ).not.toBeNull();
  });

  it("'Sempre' aplicado como once mostra a nota de liberação só desta vez", async () => {
    arrive({ request_id: "r1", project_id: PID, task_id: "T01", agent: "developer", command: "git status", always_allowed: true, always_denied_reason: null });
    installFetch(() => ({ body: { ok: true, applied: "once" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Sempre permitir" }));

    expect(
      await screen.findByText(/^Não foi possível liberar permanentemente; liberado só desta vez ·/),
    ).not.toBeNull();
  });

  it("o rótulo confirmado vem de applied, não do botão clicado", async () => {
    arrive({ request_id: "r1", project_id: PID, task_id: "T01", agent: "developer", command: "node --test", always_allowed: false });
    installFetch(() => ({ body: { ok: true, applied: "once" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Permitir nesta execução" }));

    expect(await screen.findByText(/^Liberado só desta vez ·/)).not.toBeNull();
    expect(screen.queryByText(/Liberado nesta execução/)).toBeNull();
  });

  it("resolução por outra janela (SSE com applied) muda a mensagem e tira os botões", () => {
    seed([req("r1", "node --test")]);
    render(<Harness />);
    expect(groups()).toHaveLength(1);

    resolveByEvent("r1", "run");

    expect(screen.getByText(/^Liberado nesta execução ·/)).not.toBeNull();
    expect(groups()).toHaveLength(0);
    expect(screen.queryByRole("button", { name: "Negar" })).toBeNull();
  });
});

describe("PermissionMessage: escopos honestos (JSON literal do servidor)", () => {
  beforeEach(() => resetStore());

  const NOT_ALWAYS = {
    request_id: "r1",
    project_id: PID,
    task_id: "T01",
    agent: "developer",
    command: "node --test",
    always_allowed: false,
    always_denied_reason: "este comando executa código arbitrário",
  };

  it("sem always_allowed: não há 'Sempre permitir' e a explicação fixa aparece", () => {
    arrive(NOT_ALWAYS);
    render(<Harness />);

    expect(screen.queryByRole("button", { name: "Sempre permitir" })).toBeNull();
    expect(screen.getByRole("button", { name: "Permitir nesta execução" })).not.toBeNull();
    expect(screen.getByText("este comando executa código arbitrário")).not.toBeNull();
  });

  it("campo always_allowed ausente é tratado como não permitido (sem 'Sempre')", () => {
    const { always_allowed: _omit, always_denied_reason: _reason, ...legacy } = NOT_ALWAYS;
    arrive(legacy);
    render(<Harness />);

    expect(screen.queryByRole("button", { name: "Sempre permitir" })).toBeNull();
    expect(screen.getByText("este comando não pode ser liberado permanentemente")).not.toBeNull();
  });

  it("chat do Arquiteto não oferece 'Permitir nesta execução' (run aplicaria once)", () => {
    arrive({ ...NOT_ALWAYS, agent: "architect", command: "cat x" });
    render(<Harness />);

    expect(screen.queryByRole("button", { name: "Permitir nesta execução" })).toBeNull();
    expect(screen.getByRole("button", { name: "Permitir uma vez" })).not.toBeNull();
  });

  it.each([
    ["desconhecido", "agente-novo"],
    ["ausente", undefined],
  ])("agente %s não oferece 'Permitir nesta execução' (allowlist)", (_label, agent) => {
    const { agent: _drop, ...rest } = NOT_ALWAYS;
    arrive(agent === undefined ? rest : { ...NOT_ALWAYS, agent });
    render(<Harness />);

    expect(screen.queryByRole("button", { name: "Permitir nesta execução" })).toBeNull();
    expect(screen.getByRole("button", { name: "Permitir uma vez" })).not.toBeNull();
    expect(screen.getByRole("button", { name: "Negar" })).not.toBeNull();
  });

  it("motivo genérico do servidor vira o texto genérico do cliente", () => {
    arrive({ ...NOT_ALWAYS, always_denied_reason: "não pode ser liberado permanentemente" });
    render(<Harness />);

    expect(screen.getByText("este comando não pode ser liberado permanentemente")).not.toBeNull();
  });

  it("razão desconhecida vira o texto genérico fixo (sem HTML)", () => {
    arrive({ ...NOT_ALWAYS, always_denied_reason: "<b>texto livre do agente</b>" });
    const { container } = render(<Harness />);

    expect(screen.getByText("este comando não pode ser liberado permanentemente")).not.toBeNull();
    expect(container.querySelector("b")).toBeNull();
    expect(screen.queryByText(/texto livre/)).toBeNull();
  });

  it("razão de segredo e de operadores mostram o texto fixo correspondente", () => {
    arrive({ ...NOT_ALWAYS, always_denied_reason: "este comando contém um segredo e não pode ser liberado permanentemente" });
    render(<Harness />);
    expect(
      screen.getByText("este comando contém um segredo e não pode ser liberado permanentemente"),
    ).not.toBeNull();
  });
});

describe("PermissionMessage: conversa, fila e isolamento", () => {
  beforeEach(() => resetStore());

  it("vários pendentes aparecem na ordem de chegada, cada um com as próprias opções", async () => {
    seed([req("r1", "cargo clippy"), req("r2", "cargo fmt --check")]);
    installFetch(() => ({ body: { ok: true, applied: "once" } }));
    const user = userEvent.setup();
    render(<Harness />);

    const texts = screen.getAllByRole("listitem").map((li) => li.textContent ?? "");
    expect(texts[0]).toContain("cargo clippy");
    expect(texts[1]).toContain("cargo fmt --check");
    expect(groups()).toHaveLength(2);

    await user.click(screen.getAllByRole("button", { name: "Permitir uma vez" })[1] as HTMLElement);

    expect(await screen.findByText(/^Liberado só desta vez ·/)).not.toBeNull();
    expect(groups()).toHaveLength(1);
    expect(screen.getByText("cargo clippy")).not.toBeNull();
  });

  it("confirmação de um pedido não aparece na mensagem de outro", async () => {
    seed([req("r1", "node --test"), req("r2", "cargo fmt --check")]);
    installFetch(() => ({ body: { ok: true, applied: "once" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getAllByRole("button", { name: "Permitir uma vez" })[0] as HTMLElement);
    expect(await screen.findByText(/^Liberado só desta vez ·/)).not.toBeNull();

    const second = screen.getByText("cargo fmt --check").closest("li") as HTMLElement;
    expect(second.textContent).not.toContain("Liberado");
    expect(within(second).getByRole("group", { name: "Decisão sobre o comando" })).not.toBeNull();
  });

  it("erro de POST não aparece na mensagem de outro pedido", async () => {
    seed([req("r1", "node --test"), req("r2", "cargo fmt --check")]);
    installFetch(() => ({ status: 500, body: { error: "falha interna" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getAllByRole("button", { name: "Negar" })[0] as HTMLElement);
    expect((await screen.findByRole("alert")).textContent).toBe("falha interna");

    const second = screen.getByText("cargo fmt --check").closest("li") as HTMLElement;
    expect(second.querySelector("[role=alert]")).toBeNull();
  });

  it("comando longo começa truncado e expande sob demanda", async () => {
    const long = "x".repeat(COMMAND_PREVIEW_CHARS + 50);
    seed([req("r1", long)]);
    const user = userEvent.setup();
    render(<Harness />);

    const shown = document.querySelector("pre");
    expect(shown?.textContent).toBe("x".repeat(COMMAND_PREVIEW_CHARS));
    expect(shown?.getAttribute("data-truncated")).toBe("true");
    const toggle = screen.getByRole("button", { name: "Expandir comando" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");

    await user.click(toggle);

    expect(document.querySelector("pre")?.textContent).toBe(long);
    expect(screen.getByRole("button", { name: "Recolher comando" }).getAttribute("aria-expanded")).toBe("true");
  });

  it("comando com HTML aparece como texto", () => {
    seed([req("r1", "<b>rm</b> <img src=x onerror=alert(1)>")]);
    const { container } = render(<Harness />);
    expect(container.querySelector("b")).toBeNull();
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getByText("<b>rm</b> <img src=x onerror=alert(1)>")).not.toBeNull();
  });

  it("mostra o selo da tarefa quando há tarefa e nada quando não há", () => {
    seed([req("r1", "node --test", { taskId: "T02" }), req("r2", "cat x", { taskId: null, agent: "architect" })]);
    render(<Harness />);

    expect(screen.getByText("T02")).not.toBeNull();
    expect(screen.getAllByRole("listitem")[1]?.textContent).not.toMatch(/T0\d/);
  });

  it("pedido reidratado do servidor (reload) aparece com as opções", () => {
    appStore.hydratePermissions(
      PID,
      [{ request_id: "r9", agent: "developer", command: "node --test", task_id: "T01", always_allowed: false, always_denied_reason: "este comando executa código arbitrário" }],
      0,
    );
    render(<Harness />);

    expect(groups()).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Negar" })).not.toBeNull();
  });
});

describe("PermissionMessage: foco e acessibilidade", () => {
  beforeEach(() => resetStore());

  it("a chegada de um pedido não move o foco (decisão 10: nada rouba o foco)", () => {
    const before = document.activeElement;
    seed([req("r1", "node --test")]);
    render(<Harness />);

    expect(document.activeElement).toBe(before);
  });

  it("não rouba o foco de um campo de digitação", async () => {
    const user = userEvent.setup();
    render(
      <>
        <textarea aria-label="Mensagem" />
        <Harness />
      </>,
    );
    const input = screen.getByRole("textbox", { name: "Mensagem" });
    await user.click(input);
    await user.type(input, "oi");

    arrive({ request_id: "r1", project_id: PID, task_id: "T01", agent: "developer", command: "node --test", always_allowed: false });

    expect(screen.getByRole("group", { name: "Decisão sobre o comando" })).not.toBeNull();
    expect(document.activeElement).toBe(input);
    expect((input as HTMLTextAreaElement).value).toBe("oi");
  });

  it("o conjunto de botões tem rótulo acessível e a explicação é ligada ao grupo", () => {
    arrive({ request_id: "r1", project_id: PID, task_id: "T01", agent: "developer", command: "node --test", always_allowed: false, always_denied_reason: "este comando executa código arbitrário" });
    render(<Harness />);

    const group = screen.getByRole("group", { name: "Decisão sobre o comando" });
    const describedBy = group.getAttribute("aria-describedby");
    expect(describedBy).not.toBeNull();
    expect(document.getElementById(describedBy ?? "")?.textContent).toBe("este comando executa código arbitrário");
  });

  it("cada botão aponta para a linha do efeito dele", () => {
    arrive({ request_id: "r1", project_id: PID, task_id: "T01", agent: "developer", command: "git status", always_allowed: true });
    render(<Harness />);
    for (const name of ["Permitir uma vez", "Permitir nesta execução", "Sempre permitir", "Negar"]) {
      const button = screen.getByRole("button", { name });
      const hint = document.getElementById(button.getAttribute("aria-describedby") ?? "");
      expect(hint?.textContent).not.toBe("");
    }
  });

  it("nenhum botão usa caractere no lugar de ícone", () => {
    const long = "y".repeat(COMMAND_PREVIEW_CHARS + 10);
    seed([req("r1", long), req("r2", "cargo test", { alwaysAllowed: true })]);
    const { container } = render(<Harness />);
    expect(findForbiddenGlyphs(container)).toEqual([]);
  });
});


describe("PermissionMessage: mudança de ambiente (kind environment)", () => {
  beforeEach(() => resetStore());

  const DIFF = "--- .nants/Dockerfile\n+++ .nants/Dockerfile\n FROM alpine:3\n+RUN apk add <b>curl</b>\n";
  const envPayload = {
    request_id: "e1",
    project_id: PID,
    task_id: null,
    agent: "developer",
    command: "Alterar o ambiente Docker: .nants/Dockerfile",
    kind: "environment",
    reason: "preciso do curl",
    diff: DIFF,
    always_allowed: false,
    always_denied_reason: null,
  };

  it("mostra motivo e diff como texto monoespaçado e rolável, sem HTML, e só aprovar/negar", () => {
    arrive(envPayload);
    render(<Harness />);

    expect(screen.getByText("Alterar o ambiente Docker do projeto?")).not.toBeNull();
    expect(screen.getByText(/preciso do curl/)).not.toBeNull();
    const diff = screen.getByLabelText("Diferenças propostas");
    expect(diff.tagName).toBe("PRE");
    expect(diff.textContent).toBe(DIFF);
    expect(diff.querySelector("b")).toBeNull();
    expect(diff.className).toContain("nd-mono");
    expect(diff.className).toContain("nd-diff");
    const buttons = screen.getAllByRole("button").map((b) => b.textContent);
    expect(buttons).toEqual(["Aprovar mudança", "Negar"]);
  });

  it("aprovar envia once e a mensagem vira resultado; negar envia deny", async () => {
    arrive(envPayload);
    const fetchMock = installFetch(() => ({ body: { ok: true, applied: "once" } }));
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Aprovar mudança" }));

    expect(fetchMock.mock.calls[0]?.[1]?.body).toBe(JSON.stringify({ decision: "once" }));
    expect(await screen.findByText(/^Liberado só desta vez ·/)).not.toBeNull();
    cleanup();

    resetStore();
    arrive({ ...envPayload, request_id: "e2" });
    const deny = installFetch(() => ({ body: { ok: true, applied: "deny" } }));
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "Negar" }));
    expect(deny.mock.calls[0]?.[1]?.body).toBe(JSON.stringify({ decision: "deny" }));
  });
});
