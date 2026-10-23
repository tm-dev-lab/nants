/*
 * Moldura nos dois idiomas, parte da spec 102 T05: o diálogo Novo projeto (com o navegador de
 * pastas e as mensagens de `desktop/api.ts`), os avisos flutuantes de permissão, as vistas do
 * celular (abas e Alternador, com os subtítulos vindos do estado dos apps) e as dicas do lançador.
 * Em en, nada em português (salvo o que a pessoa digitou ou gravou); pt-BR igual a hoje; a troca de
 * idioma vale na hora.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { setLanguage } from "../i18n";
import { portugueseLeftovers } from "../i18n/ptLeftovers";
import { makeProject, makeProjectState } from "../test-utils";
import { createProjectByName, NAME_INVALID_MESSAGE, type Readiness } from "./api";
import { apiFailureText } from "../i18n/serverCodes";
import { READY, renderShell, settle } from "./shell-test-utils";
import { MOBILE_QUERY } from "./viewport";

let restoreMedia: (() => void) | null = null;

afterEach(() => {
  cleanup();
  restoreMedia?.();
  restoreMedia = null;
  setLanguage("pt-BR", { persist: false });
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  window.history.replaceState({}, "", "/");
});

type Lang = "pt-BR" | "en";
const P1 = makeProject("p1", "Loja");
const P2 = makeProject("p2", "Blog");

/** Celular: só o `MOBILE_QUERY` responde verdadeiro. */
function mobileViewport(): void {
  const original = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    media: query,
    matches: query === MOBILE_QUERY,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
  })) as unknown as typeof window.matchMedia;
  restoreMedia = () => {
    if (original === undefined) Reflect.deleteProperty(window, "matchMedia");
    else window.matchMedia = original;
  };
}

/** Prontidão pronta, com o preço de um modelo pendente (subtítulo "tudo pronto · preços a cadastrar"). */
const PRICING_PENDING: Readiness = {
  ...READY,
  recommended: { ok: false, missing: [{ id: "pricing:model", label: "Preço do modelo model", code: "pricing", params: { model: "model" }, settings_tab: "pricing" }] },
};

async function openNewProject(lang: Lang) {
  setLanguage(lang, { persist: false });
  const view = await renderShell({ projects: [P1] });
  const name = lang === "en" ? "New project" : "Novo projeto";
  fireEvent.click(within(document.querySelector<HTMLElement>("[data-welcome]") ?? document.body).getByRole("button", { name }));
  return { ...view, dialog: screen.getByRole("dialog", { name }) };
}

describe("Novo projeto em en", () => {
  it("rótulos, dicas, validação e as falhas fixas (nome, Git, raiz, servidor) no idioma ativo", async () => {
    const { server, dialog } = await openNewProject("en");
    expect(within(dialog).getByLabelText("Project name").getAttribute("placeholder")).toBe("e.g.: Corner shop");
    expect(dialog.textContent).toContain("Nants creates the project folder, with Git, in the projects folder.");
    expect(within(dialog).getByRole("button", { name: "Use an existing folder" })).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeTruthy();
    expect(portugueseLeftovers(dialog)).toEqual([]);
    fireEvent.click(within(dialog).getByRole("button", { name: "Create project" }));
    await settle();
    expect(dialog.textContent).toContain("Enter a name.");
    expect(portugueseLeftovers(dialog)).toEqual([]);

    const replies = [
      { status: 400, body: { error: "nome inválido: use letras ou números", code: "project_name_invalid" } },
      { status: 412, body: { error: "prereq_missing", missing: ["git"] } },
      { status: 422, body: { error: "pasta de projetos inválida" } },
      { status: 409, body: { error: "identificador de projeto já existe para outro caminho", code: "project_id_taken" } },
    ];
    const expected = [
      "Invalid name: use letters or numbers.",
      "Git is not installed. Install Git and check again in Getting started.",
      "The configured projects folder is invalid. Adjust it in Settings, Projects tab.",
      "project identifier already exists for another path",
    ];
    fireEvent.change(within(dialog).getByLabelText("Project name"), { target: { value: "Loja" } });
    for (const [i, reply] of replies.entries()) {
      server.reply("POST", /^\/api\/projects$/, () => reply);
      await act(async () => {
        fireEvent.click(within(dialog).getByRole("button", { name: "Create project" }));
      });
      await settle();
      expect(within(dialog).getByRole("alert").textContent).toBe(expected[i]);
      expect(portugueseLeftovers(dialog)).toEqual([]);
    }

    server.reply("POST", /^\/api\/projects$/, () => ({ status: 409, body: { error: "a pasta do projeto já existe", code: "project_dir_exists", path: "/home/u/NantsProjects/loja" } }));
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Create project" }));
    });
    await settle();
    const callout = dialog.querySelector("[data-dir-exists]") as HTMLElement;
    expect(callout.textContent).toContain("A folder with this name already exists: /home/u/NantsProjects/loja");
    expect(within(callout).getByRole("button", { name: "Use the existing folder" })).toBeTruthy();
    expect(portugueseLeftovers(dialog)).toEqual([]);
  });

  it("navegador de pastas: rótulos, lista e o erro do servidor traduzido", async () => {
    const { server, dialog } = await openNewProject("en");
    let fail = false;
    server.reply("GET", /^\/api\/fs\/browse/, () =>
      fail
        ? { status: 403, body: { error: "sem permissão para listar o diretório", code: "list_no_permission" } }
        : { status: 200, body: { path: "/home/u", parent: "/home", truncated: true, entries: [{ name: "app", path: "/home/u/app", is_git_repo: true }] } },
    );
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Use an existing folder" }));
    });
    await settle();
    const browser = dialog.querySelector("[data-folder-browser]") as HTMLElement;
    expect(browser.getAttribute("aria-label")).toBe("Choose folder");
    expect(within(browser).getByRole("navigation", { name: "Current path" })).toBeTruthy();
    expect(within(browser).getByRole("button", { name: "Parent folder" })).toBeTruthy();
    expect(within(browser).getByLabelText("Show hidden")).toBeTruthy();
    expect(within(browser).getByRole("list", { name: "Folders" })).toBeTruthy();
    expect(within(browser).getByRole("button", { name: "Open folder app" }).textContent).toContain("Git repository");
    expect(browser.textContent).toContain("List truncated: type the path directly to reach the other folders.");
    expect(within(browser).getByRole("button", { name: "Use this folder" })).toBeTruthy();
    expect(within(dialog).getByLabelText("Project folder (absolute path)").getAttribute("placeholder")).toBe("/home/user/projects/shop");
    expect(portugueseLeftovers(dialog)).toEqual([]);
    fail = true;
    await act(async () => {
      fireEvent.click(within(browser).getByRole("button", { name: "Parent folder" }));
    });
    await settle();
    expect(within(browser).getByRole("alert").textContent).toBe("no permission to list the folder");
    expect(portugueseLeftovers(dialog)).toEqual([]);
  });

  it("mensagens de desktop/api.ts: a reserva fica em pt-BR em `message` e a tela usa a chave", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: "x" }), { status: 400, headers: { "Content-Type": "application/json" } })),
    );
    const outcome = await createProjectByName("!!");
    if (outcome.kind !== "error") throw new Error("esperava erro");
    expect(outcome.message).toBe(NAME_INVALID_MESSAGE);
    expect(outcome.key).toBe("newProject.nameInvalid");
    setLanguage("en", { persist: false });
    expect(apiFailureText({ kind: "invalid_response", status: 200, message: "resposta inesperada do servidor", messageKey: "api.invalidResponse" })).toBe(
      "unexpected response from the server",
    );
  });
});

describe("Novo projeto: pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje; a troca muda rótulos e o erro já exibido", async () => {
    const { server, dialog } = await openNewProject("pt-BR");
    server.reply("POST", /^\/api\/projects$/, () => ({ status: 400, body: { error: "x" } }));
    fireEvent.change(within(dialog).getByLabelText("Nome do projeto"), { target: { value: "Loja" } });
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
    });
    await settle();
    expect(within(dialog).getByRole("alert").textContent).toBe("Nome inválido: use letras ou números.");
    expect(within(dialog).getByLabelText("Nome do projeto").getAttribute("placeholder")).toBe("ex.: Loja de bairro");
    await act(async () => {
      setLanguage("en", { persist: false });
    });
    expect(within(dialog).getByRole("alert").textContent).toBe("Invalid name: use letters or numbers.");
    expect(within(dialog).getByLabelText("Project name")).toBeTruthy();
    expect(dialog.getAttribute("aria-labelledby")).not.toBeNull();
    expect(portugueseLeftovers(dialog)).toEqual([]);
  });
});

function permissionEvent(projectId: string, requestId: string, command: string, kind?: string) {
  return {
    event_type: "permission_requested",
    project_id: projectId,
    payload: { request_id: requestId, project_id: projectId, task_id: "T01", agent: "developer", command, always_allowed: false, ...(kind !== undefined ? { kind } : {}) },
  };
}

describe("Avisos flutuantes de permissão", () => {
  it("en: '+N', ações e a mudança de ambiente; pt-BR igual a hoje e a troca ao vivo", async () => {
    setLanguage("en", { persist: false });
    const { stream } = await renderShell({ url: "/?space=p1", projects: [P1, P2], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    // A hora da linha do tempo nunca recua (114/T01): os instantes partem da hora atual.
    let clock = Date.now();
    vi.spyOn(Date, "now").mockImplementation(() => (clock += 10));
    await act(async () => {
      stream().emit("permission_requested", permissionEvent("p1", "r1", "cargo test"));
      stream().emit("permission_requested", permissionEvent("p2", "r2", "npm install"));
      stream().emit("permission_requested", permissionEvent("p1", "r3", "ls -la"));
      stream().emit("permission_requested", permissionEvent("p2", "r4", "cat .nants/compose.yaml", "environment"));
    });
    await settle();
    const toasts = document.querySelector("[data-permission-toasts]") as HTMLElement;
    expect(toasts.querySelector("[data-toast-more]")?.textContent).toBe("1 more request");
    expect(within(toasts).getAllByRole("button", { name: "Go to the conversation" })).toHaveLength(3);
    expect(toasts.querySelector("[data-environment-review]")?.textContent).toBe("Environment change: see what changes in Conversation before approving.");
    expect(toasts.textContent).toContain("Developer · T01 · Blog");
    expect(portugueseLeftovers(toasts)).toEqual([]);

    await act(async () => {
      setLanguage("pt-BR", { persist: false });
    });
    expect(toasts.querySelector("[data-toast-more]")?.textContent).toBe("1 outro pedido");
    expect(within(toasts).getAllByRole("button", { name: "Ir para a conversa" })).toHaveLength(3);
    expect(toasts.querySelector("[data-environment-review]")?.textContent).toBe("Mudança de ambiente: veja o que muda na Conversa antes de aprovar.");
    expect(portugueseLeftovers(toasts).length).toBeGreaterThan(0);
  });
});

describe("Celular e lançador: subtítulos vindos do estado dos apps", () => {
  it("en: abas e Alternador sem português (nomes e subtítulos de Configurações, Projeto e as demais)", async () => {
    mobileViewport();
    setLanguage("en", { persist: false });
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) }, readiness: PRICING_PENDING });
    const tabs = screen.getByRole("navigation", { name: "Tabs" });
    expect(portugueseLeftovers(tabs)).toEqual([]);
    await act(async () => {
      fireEvent.click(within(tabs).getByRole("button", { name: "Switcher" }));
    });
    await settle();
    const switcher = screen.getByRole("region", { name: "Windows" });
    expect(within(switcher).getByRole("button", { name: /^Settings/ }).textContent).toContain("all set · prices to add");
    expect(within(switcher).getByRole("button", { name: /^Project/ }).textContent).toContain("standard mode · 5 attempts");
    expect(portugueseLeftovers(switcher)).toEqual([]);
    expect(portugueseLeftovers(document.querySelector(".nd-root") as HTMLElement)).toEqual([]);

    await act(async () => {
      setLanguage("pt-BR", { persist: false });
    });
    const pt = screen.getByRole("region", { name: "Janelas" });
    expect(within(pt).getByRole("button", { name: /^Configurações/ }).textContent).toContain("tudo pronto · preços a cadastrar");
    expect(within(pt).getByRole("button", { name: /^Projeto/ }).textContent).toContain("modo padrão · 5 tentativas");
  });

  it("en: as dicas do lançador (subtítulos das janelas) no idioma ativo", async () => {
    setLanguage("en", { persist: false });
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) }, readiness: PRICING_PENDING });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    const launcher = screen.getByRole("dialog", { name: "Launcher" });
    expect(launcher.textContent).toContain("all set · prices to add");
    expect(launcher.textContent).toContain("standard mode · 5 attempts");
    expect(portugueseLeftovers(launcher)).toEqual([]);
  });
});
