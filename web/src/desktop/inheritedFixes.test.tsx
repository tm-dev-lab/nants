/*
 * Pendências herdadas da revisão da T06, resolvidas na T07 (spec 093):
 * 1. "Aprovar o plano" no lançador respeita a re-revisão da 092 e a prontidão;
 * 2. "Continuar" da pílula confere `readiness.execute`;
 * 3. mudança de ambiente no sino e no aviso flutuante: "Ir para a conversa", não "Aprovar";
 * 4. arquivo solto fora da Conversa: o navegador não abre o arquivo;
 * 5. "Reenviar" respeita o bloqueio do chat;
 * 6. 412 com ids parametrizados (`connection_key:<id>`) aparece com o rótulo do `/api/readiness`.
 */
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiFailure } from "../api/client";
import type { ProjectState, SpecView } from "../api/types";
import { Conversation } from "../apps/conversation/Conversation";
import { prereqItems, prereqItem, relabelPrereqs, settingsTabForPrereq } from "../apps/shared/readiness";
import { approveProblem } from "../apps/shared/runProblems";
import type { ChatMessage } from "../store";
import { makeProject, makeProjectState, makeSpec, makeTask, resetStore } from "../test-utils";
import type { Readiness } from "./api";
import { applyAttention } from "./attention";
import { findForbiddenGlyphs } from "./glyphs";
import { DEFAULT_SHELL, ShellContext } from "./shellContext";
import { notReady, READY, renderShell, settle } from "./shell-test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  applyAttention(0);
});

const P1 = makeProject("p1", "Loja");

function planState(extra: Partial<SpecView> = {}): ProjectState {
  const spec = { ...makeSpec("s1", "Loja online", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]), ...extra };
  return makeProjectState({ id: "p1", name: "Loja", specs: [spec] });
}

function openLauncher(): HTMLElement {
  fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
  return screen.getByRole("dialog", { name: "Lançador" });
}

const APPROVE = /\/api\/projects\/p1\/specs\/s1\/approve$/;

describe("1. lançador: 'Aprovar o plano'", () => {
  it("revisão do plano em andamento: item desabilitado com o motivo; Enter não aprova", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planState({ plan_review: { status: "pending", issues: [] } }) } });
    const launcher = openLauncher();
    const input = within(launcher).getByRole("combobox");
    fireEvent.change(input, { target: { value: "aprovar" } });
    const item = launcher.querySelector('[data-launcher-item="approve-plan"]') as HTMLElement;
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector("[data-launcher-disabled]")?.textContent).toBe("aguardando o Revisor");
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    await settle();
    expect(server.count("POST", APPROVE)).toBe(0);
    expect(findForbiddenGlyphs(launcher)).toEqual([]);
  });

  it("sem readiness.execute: desabilitado com o que falta", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planState() }, readiness: notReady() });
    const launcher = openLauncher();
    const item = launcher.querySelector('[data-launcher-item="approve-plan"]') as HTMLElement;
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.textContent).toContain("falta configurar:");
    expect(item.textContent).toContain("Docker daemon");
  });

  it("aprovável e pronto: o item aprova", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planState() } });
    server.reply("POST", APPROVE, () => ({ status: 202, body: { ok: true, spec_id: "s1" } }));
    const launcher = openLauncher();
    const item = launcher.querySelector('[data-launcher-item="approve-plan"]') as HTMLElement;
    expect(item.getAttribute("aria-disabled")).toBeNull();
    await act(async () => {
      fireEvent.click(item);
    });
    await settle();
    expect(server.count("POST", APPROVE)).toBe(1);
  });
});

function stalledState(): ProjectState {
  const spec = makeSpec("s1", "Loja online", "IN_PROGRESS", [makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "TODO")]);
  return makeProjectState({ id: "p1", name: "Loja", specs: [spec], running: false, run: null });
}

const RESUME = /\/api\/projects\/p1\/specs\/s1\/resume$/;

describe("2. pílula: 'Continuar' confere a prontidão", () => {
  it("sem readiness.execute: desabilitado, com a lista do que falta", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: stalledState() }, readiness: notReady() });
    const button = document.querySelector<HTMLButtonElement>("[data-resume-run]");
    expect(button?.textContent).toContain("Continuar");
    expect(button?.disabled).toBe(true);
    const missing = document.querySelector("[data-resume-blocked]");
    expect(missing?.textContent).toContain("Docker daemon");
    expect(button?.getAttribute("aria-describedby")).toBe(missing?.id);
    await act(async () => {
      fireEvent.click(button as HTMLButtonElement);
    });
    expect(server.count("POST", RESUME)).toBe(0);
    // O lançador também mostra o item indisponível, com o motivo.
    const item = openLauncher().querySelector('[data-launcher-item="resume-run"]') as HTMLElement;
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.textContent).toContain("Docker daemon");
  });

  it("pronto: habilitado e chama o resume", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: stalledState() } });
    server.reply("POST", RESUME, () => ({ status: 202, body: { ok: true, spec_id: "s1" } }));
    const button = document.querySelector<HTMLButtonElement>("[data-resume-run]");
    expect(button?.disabled).toBe(false);
    expect(document.querySelector("[data-resume-blocked]")).toBeNull();
    await act(async () => {
      fireEvent.click(button as HTMLButtonElement);
    });
    await settle();
    expect(server.count("POST", RESUME)).toBe(1);
  });
});

describe("3. mudança de ambiente fora da Conversa", () => {
  it("sino e aviso flutuante: 'Ir para a conversa' em vez de 'Aprovar mudança'", async () => {
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      attention: {
        setup_incomplete: false,
        projects: [
          {
            project_id: "p1",
            name: "Loja",
            pending_permissions: [{ request_id: "e1", agent: "developer", kind: "environment", task_id: "T01", always_allowed: false }],
            pending_choices: false,
            plan_awaiting_approval: false,
            paused_or_stalled: null,
          },
        ],
      },
    });
    await act(async () => {
      stream().emit("permission_requested", {
        event_type: "permission_requested",
        project_id: "p1",
        payload: { request_id: "e1", project_id: "p1", task_id: "T01", agent: "developer", command: "docker compose", kind: "environment", reason: "nova porta", diff: "+ porta 8080", always_allowed: false },
      });
    });
    await settle();
    const toast = document.querySelector<HTMLElement>('[data-permission-toast="e1"]') as HTMLElement;
    expect(within(toast).queryByRole("button", { name: "Aprovar mudança" })).toBeNull();
    // 095/T02 (decisão 9): botão secundário do conjunto único (a primária fica para a ação principal da janela).
    expect(within(toast).getByRole("button", { name: "Ir para a conversa" }).getAttribute("data-variant")).toBeNull();
    expect(toast.querySelector("[data-environment-review]")).not.toBeNull();
    await act(async () => {
      fireEvent.click(document.querySelector("[data-bell]") as HTMLElement);
    });
    const panel = screen.getByRole("region", { name: "Precisa de você" });
    const item = panel.querySelector('[data-request-id="e1"]') as HTMLElement;
    expect(within(item).queryByRole("button", { name: "Aprovar mudança" })).toBeNull();
    expect(within(item).getByRole("button", { name: "Ir para a conversa" })).toBeTruthy();
    expect(item.querySelector("[data-environment-review]")).not.toBeNull();
    expect(findForbiddenGlyphs(panel)).toEqual([]);
  });

  it("comando comum continua com as decisões no aviso", async () => {
    const { stream } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    await act(async () => {
      stream().emit("permission_requested", {
        event_type: "permission_requested",
        project_id: "p1",
        payload: { request_id: "c1", project_id: "p1", task_id: "T01", agent: "developer", command: "npm i", always_allowed: false },
      });
    });
    const toast = document.querySelector<HTMLElement>('[data-permission-toast="c1"]') as HTMLElement;
    expect(within(toast).getByRole("button", { name: "Permitir uma vez" })).toBeTruthy();
  });
});

describe("4. arquivo solto fora da Conversa", () => {
  it("dragover e drop com arquivo no desktop têm preventDefault; arrastar texto não", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    const root = document.querySelector(".nd-root") as HTMLElement;
    const files = { types: ["Files"], files: [], dropEffect: "copy" };
    expect(fireEvent.dragOver(root, { dataTransfer: files })).toBe(false);
    expect(fireEvent.drop(root, { dataTransfer: files })).toBe(false);
    expect(files.dropEffect).toBe("none");
    // Sobre uma janela qualquer (o Plano), o evento sobe até a raiz e também é barrado.
    const plan = screen.getByRole("region", { name: "Plano" });
    expect(fireEvent.drop(plan, { dataTransfer: { types: ["Files"], files: [] } })).toBe(false);
    expect(fireEvent.dragOver(root, { dataTransfer: { types: ["text/plain"] } })).toBe(true);
  });
});

describe("5. 'Reenviar' respeita o bloqueio do chat", () => {
  it("chat bloqueado: o botão fica desabilitado e não envia", async () => {
    resetStore();
    const post = vi.fn(async () => new Response(JSON.stringify({ reply: "", draft_updated: false, turn_id: "t" }), { status: 200 }));
    vi.stubGlobal("fetch", post);
    const message = { id: 1, role: "user", text: "oi", at: 1, unsent: "não enviada" } as ChatMessage;
    const view = (readiness: Readiness) =>
      render(
        <ShellContext.Provider value={{ ...DEFAULT_SHELL, readiness }}>
          <Conversation projectId="p1" messages={[message]} onDraftUpdated={async () => undefined} />
        </ShellContext.Provider>,
      );
    const blocked = view(notReady());
    const resend = within(blocked.container).getByRole("button", { name: "Reenviar" }) as HTMLButtonElement;
    expect(resend.disabled).toBe(true);
    fireEvent.click(resend);
    expect(post).not.toHaveBeenCalled();
    blocked.unmount();
    const ready = view(READY);
    expect((within(ready.container).getByRole("button", { name: "Reenviar" }) as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("6. 412 com ids parametrizados", () => {
  const KEY_ITEM = { id: "connection_key:c1", label: "Chave da conexão Principal", settings_tab: "connections" as const };
  const withKey: Readiness = { ...READY, execute: { ok: false, missing: [KEY_ITEM] } };
  const failure: ApiFailure = { kind: "http", status: 412, message: "Falta configurar: pré-requisitos do Nants (veja as Configurações).", reason: "prereq_missing", missing: ["connection_key:c1", "docker_daemon"] };

  it("regra pura: rótulo e aba do item da prontidão; sem ele, um rótulo com o parâmetro (nunca o genérico)", () => {
    expect(prereqItems(failure, withKey)).toEqual([KEY_ITEM, { id: "docker_daemon", label: "Docker daemon", settings_tab: "machine" }]);
    expect(prereqItem("connection_key:c1", null)).toEqual({ id: "connection_key:c1", label: "Chave da conexão c1", settings_tab: "connections" });
    expect(prereqItem("connection_url:c2", null).label).toBe("Endereço da conexão c2");
    expect(prereqItem("pricing:gpt-6-luna", null)).toMatchObject({ label: "Preço do modelo gpt-6-luna", settings_tab: "pricing" });
    expect(settingsTabForPrereq("connection_url:c2")).toBe("connections");
    expect(relabelPrereqs([prereqItem("connection_key:c1", null)], withKey)).toEqual([KEY_ITEM]);
    const problem = approveProblem(failure, withKey);
    expect(problem.message).toBe("Falta configurar: Chave da conexão Principal, Docker daemon.");
    expect(problem.prereq).toBe(true);
  });

  it("Plano: o 412 com connection_key:c1 mostra o rótulo do /api/readiness relido e 'Configurar' em Conexões", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planState() } });
    server.reply("POST", APPROVE, () => {
      server.model.readiness = withKey;
      return { status: 412, body: { error: "prereq_missing", missing: ["connection_key:c1"] } };
    });
    const plan = screen.getByRole("region", { name: "Plano" });
    fireEvent.click(within(plan).getByRole("button", { name: "Aprovar e iniciar" }));
    await act(async () => {
      fireEvent.click(within(plan).getByRole("button", { name: "Confirmar início" }));
    });
    await settle();
    await waitFor(() => expect(within(plan).getByRole("alert").textContent).toContain("Chave da conexão Principal"));
    const alert = within(plan).getByRole("alert");
    expect(alert.textContent).not.toContain("pré-requisitos do Nants");
    expect(within(alert).getByRole("button", { name: "Configurar: Chave da conexão Principal" }).getAttribute("data-settings-tab")).toBe("connections");
  });
});

describe("F-02: todo 412 com ids parametrizados relê a prontidão e mostra o rótulo dela", () => {
  const KEY_ITEM = { id: "connection_key:c1", label: "Chave da conexão Principal", settings_tab: "connections" as const };
  const withKey: Readiness = {
    chat: { ok: false, missing: [KEY_ITEM] },
    create_project: { ok: false, missing: [KEY_ITEM] },
    execute: { ok: false, missing: [KEY_ITEM] },
    recommended: { ok: true, missing: [] },
  };
  const reply412 = (server: Awaited<ReturnType<typeof renderShell>>["server"]) => () => {
    server.model.readiness = withKey;
    return { status: 412, body: { error: "prereq_missing", missing: ["connection_key:c1"] } };
  };
  const EXPECTED = "Falta configurar: Chave da conexão Principal.";

  it("pílula: 'Continuar' com 412 mostra o rótulo e, relida a prontidão, fica desabilitada com a lista", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: stalledState() } });
    server.reply("POST", RESUME, reply412(server));
    const before = server.count("GET", /^\/api\/readiness$/);
    await act(async () => {
      fireEvent.click(document.querySelector("[data-resume-run]") as HTMLElement);
    });
    await settle();
    expect(document.querySelector('[data-bar-alert="run"]')?.textContent).toContain(EXPECTED);
    expect(server.count("GET", /^\/api\/readiness$/)).toBeGreaterThan(before);
    expect(document.querySelector<HTMLButtonElement>("[data-resume-run]")?.disabled).toBe(true);
    expect(document.querySelector("[data-resume-blocked]")?.textContent).toContain("Chave da conexão Principal");
  });

  it("lançador: 'Aprovar o plano' com 412 avisa com o rótulo e relê a prontidão", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planState() } });
    server.reply("POST", APPROVE, reply412(server));
    const launcher = openLauncher();
    await act(async () => {
      fireEvent.click(launcher.querySelector('[data-launcher-item="approve-plan"]') as HTMLElement);
    });
    await settle();
    expect(document.querySelector("[data-toast]")?.textContent).toContain(EXPECTED);
    // Relida, o item passa a mostrar o que falta.
    const again = openLauncher().querySelector('[data-launcher-item="approve-plan"]') as HTMLElement;
    expect(again.getAttribute("aria-disabled")).toBe("true");
    expect(again.textContent).toContain("Chave da conexão Principal");
  });

  it("Conversa: a mensagem de sistema do 412 do chat usa o rótulo da prontidão relida", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    server.reply("POST", /^\/api\/projects\/p1\/chat$/, reply412(server));
    const conversation = screen.getByRole("region", { name: "Conversa" });
    fireEvent.change(within(conversation).getByLabelText("Mensagem para o Arquiteto"), { target: { value: "oi" } });
    await act(async () => {
      fireEvent.click(within(conversation).getByRole("button", { name: "Enviar" }));
    });
    await settle();
    expect(conversation.textContent).toContain(EXPECTED);
    expect(conversation.textContent).not.toContain("pré-requisitos do Nants");
  });

  it("Novo projeto: o 412 (fora o do controle de versão) usa o rótulo da prontidão relida", async () => {
    const { server } = await renderShell({ projects: [P1] });
    server.reply("POST", /^\/api\/projects$/, reply412(server));
    fireEvent.click(within(document.querySelector<HTMLElement>("[data-welcome]") ?? document.body).getByRole("button", { name: "Novo projeto" }));
    const dialog = screen.getByRole("dialog", { name: "Novo projeto" });
    fireEvent.change(within(dialog).getByLabelText("Nome do projeto"), { target: { value: "Blog" } });
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
    });
    await settle();
    expect(within(dialog).getByRole("alert").textContent).toBe(EXPECTED);
  });
});
