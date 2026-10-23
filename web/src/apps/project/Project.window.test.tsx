/*
 * Janela Projeto montada pelo shell (spec 093, T08): pasta, limites, YOLO e Internet com as
 * confirmações, "Remover do Nants" (inclusive os volumes Docker) e a remoção: o espaço removido (aqui ou em outra aba) fecha, aparece o aviso e o desktop
 * vai para outro espaço ou para as boas-vindas.
 */
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { fakeResizeObservers } from "../../desktop/test-utils";
import { appStore } from "../../store";
import { makeProject, makeProjectState } from "../../test-utils";
import { REMOVED_NOTICE } from "./projectRemoval";
import { RemoveProjectDialog } from "./RemoveProjectDialog";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  window.history.replaceState({}, "", "/");
});

const P1 = makeProject("p1", "Loja");
const P2 = makeProject("p2", "Blog");

function states(extra: Partial<ProjectState> = {}): Record<string, ProjectState> {
  return { p1: makeProjectState({ id: "p1", name: "Loja", ...extra }), p2: makeProjectState({ id: "p2", name: "Blog" }) };
}

async function openProject(options: RenderShellOptions = {}) {
  const view = await renderShell({ url: "/?space=p1", projects: [P1, P2], states: states(), ...options });
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Projeto" }));
  });
  await settle();
  return view;
}

function project(): HTMLElement {
  return screen.getByRole("region", { name: "Projeto" });
}

function toast(): HTMLElement | null {
  return document.querySelector<HTMLElement>("[data-toast]");
}

describe("Projeto: pasta e limites", () => {
  it("mostra a pasta com Copiar e os limites atuais, sem caracteres no lugar de ícones", async () => {
    await openProject();
    const win = project();
    expect(win.querySelector("[data-project-path]")?.textContent).toBe("/home/dev/Loja");
    expect(within(win).getByRole("button", { name: "Copiar o caminho da pasta" })).toBeTruthy();
    expect((within(win).getByLabelText("Tentativas por tarefa (novas)") as HTMLInputElement).value).toBe("5");
    expect((within(win).getByLabelText("Ciclos de teste por tentativa") as HTMLInputElement).value).toBe("5");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("095/T06: seções em lista com divisória (sem cartões); Remover do Nants é a única ação de perigo", async () => {
    await openProject();
    const win = project();
    const sections = [...win.querySelectorAll<HTMLElement>(".nd-project-section")];
    expect(sections.map((sec) => sec.querySelector("h3")?.textContent)).toEqual(["Pasta do projeto", "Limites", "Modos", "Remover do Nants"]);
    expect(win.querySelectorAll(".nd-card")).toHaveLength(0);
    expect(within(win).getAllByRole("button").filter((b) => b.getAttribute("data-variant") === "danger").map((b) => b.textContent)).toEqual(["Remover do Nants"]);
  });

  it("valida de 1 a 10 antes de enviar; salva e atualiza o estado; 409 vira mensagem fixa", async () => {
    const { server } = await openProject();
    let reply: { status: number; body: unknown } = { status: 409, body: { error: "já existe uma execução em andamento neste projeto", code: "run_active" } };
    server.reply("PUT", /^\/api\/projects\/p1\/settings$/, () => reply);
    const win = project();
    const form = within(win).getByRole("form", { name: "Limites do projeto" });
    fireEvent.change(within(form).getByLabelText("Tentativas por tarefa (novas)"), { target: { value: "11" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar limites" }));
    });
    expect(within(form).getByRole("alert").textContent).toBe("Informe valores inteiros entre 1 e 10.");
    expect(server.calls.some((c) => c.url.endsWith("/settings"))).toBe(false);
    fireEvent.change(within(form).getByLabelText("Tentativas por tarefa (novas)"), { target: { value: "3" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar limites" }));
    });
    await settle();
    expect(within(form).getByRole("alert").textContent).toBe("Há uma execução em andamento; tente de novo ao terminar.");
    reply = { status: 200, body: makeProjectState({ id: "p1", name: "Loja", max_attempts: 3, max_test_cycles: 5 }) };
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar limites" }));
    });
    await settle();
    expect(server.calls.filter((c) => c.url.endsWith("/settings")).at(-1)?.body).toEqual({ max_attempts: 3, max_test_cycles: 5 });
    expect(within(form).getByRole("status").textContent).toContain("Limites salvos.");
    expect(appStore.getState().projectStates.p1?.max_attempts).toBe(3);
  });

  it("sem edição pendente, reflete limites mudados de fora (outra aba); a edição pendente é preservada", async () => {
    await openProject();
    const form = within(project()).getByRole("form", { name: "Limites do projeto" });
    const attempts = (): HTMLInputElement => within(form).getByLabelText("Tentativas por tarefa (novas)") as HTMLInputElement;
    const cycles = (): HTMLInputElement => within(form).getByLabelText("Ciclos de teste por tentativa") as HTMLInputElement;
    const external = (max_attempts: number, max_test_cycles: number): void => {
      act(() => {
        appStore.setState((s) => ({
          projectStates: { ...s.projectStates, p1: makeProjectState({ id: "p1", name: "Loja", max_attempts, max_test_cycles }) },
        }));
      });
    };
    external(7, 4);
    expect(attempts().value).toBe("7");
    expect(cycles().value).toBe("4");
    fireEvent.change(attempts(), { target: { value: "2" } });
    external(9, 6);
    expect(attempts().value).toBe("2");
    expect(cycles().value).toBe("6");
  });

  it("estreita (data-size): os limites ficam empilhados", async () => {
    const observers = fakeResizeObservers();
    await openProject({ createObserver: observers.factory });
    await act(async () => observers.resize(380));
    expect(project().querySelector("[data-project]")?.getAttribute("data-size")).toBe("narrow");
  });
});

describe("Projeto: aprovação automática e Internet", () => {
  it("ligar a aprovação automática pede confirmação; confirmar envia e recarrega", async () => {
    const { server } = await openProject();
    server.reply("PUT", /^\/api\/projects\/p1\/yolo$/, () => ({ status: 200, body: makeProjectState({ id: "p1", name: "Loja", yolo: true }) }));
    const win = project();
    const yolo = within(win).getByRole("switch", { name: "Aprovação automática" });
    fireEvent.click(yolo);
    const dialog = screen.getByRole("dialog", { name: "Ligar a aprovação automática?" });
    expect(dialog.textContent).toContain("A aprovação automática não é um sandbox: ela só reduz as perguntas.");
    expect(dialog.textContent).toContain("Continua sempre pedindo a sua permissão");
    expect(win.textContent).toContain("Com a aprovação automática");
    expect(document.body.textContent).not.toContain("YOLO");
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Ligar a aprovação automática" }));
    });
    await settle();
    expect(server.calls.find((c) => c.method === "PUT" && c.url.endsWith("/yolo"))?.body).toEqual({ enabled: true });
  });

  it("ligar a Internet pede confirmação; confirmar grava o estado novo", async () => {
    const { server } = await openProject();
    server.reply("PUT", /^\/api\/projects\/p1\/internet$/, () => ({ status: 200, body: makeProjectState({ id: "p1", name: "Loja", architect_internet: true }) }));
    const win = project();
    const internet = within(win).getAllByRole("switch").find((s) => s.hasAttribute("data-internet"));
    if (internet === undefined) throw new Error("interruptor da Internet ausente");
    fireEvent.click(internet);
    const dialog = screen.getByRole("dialog");
    await act(async () => {
      fireEvent.click(within(dialog).getAllByRole("button").at(-1) as HTMLElement);
    });
    await settle();
    expect(server.calls.find((c) => c.method === "PUT" && c.url.endsWith("/internet"))?.body).toEqual({ enabled: true });
    expect(appStore.getState().projectStates.p1?.architect_internet).toBe(true);
  });
});

describe("Projeto: Remover do Nants", () => {
  it("confirmação com Cancelar em foco, volumes Docker desmarcados; Cancelar não envia nada", async () => {
    const { server } = await openProject();
    fireEvent.click(within(project()).getByRole("button", { name: "Remover do Nants" }));
    const dialog = screen.getByRole("alertdialog", { name: "Remover workspace do Nants?" });
    expect(document.activeElement).toBe(within(dialog).getByRole("button", { name: "Cancelar" }));
    expect(dialog.textContent).toContain("/home/dev/Loja permanecem no disco");
    expect((within(dialog).getByRole("checkbox", { name: /volumes de dados/ }) as HTMLInputElement).checked).toBe(false);
    expect(findForbiddenGlyphs(dialog)).toEqual([]);
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancelar" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(server.calls.some((c) => c.method === "DELETE")).toBe(false);
  });

  it("caminho desconhecido (vazio): o texto omite a pasta", () => {
    render(<RemoveProjectDialog project={{ id: "p1", name: "Loja", path: "" }} onClose={() => undefined} />);
    const dialog = screen.getByRole("alertdialog", { name: "Remover workspace do Nants?" });
    expect(dialog.textContent).toContain("Os arquivos do projeto permanecem no disco.");
    expect(dialog.textContent).not.toContain("Os arquivos em");
    expect(dialog.querySelector(".nd-mono")).toBeNull();
  });

  it("confirmar com volumes: DELETE com remove_volumes; o espaço fecha, vai para outro projeto e avisa", async () => {
    const { server, stream } = await openProject();
    server.reply("DELETE", /^\/api\/projects\/p1$/, () => ({ status: 200, body: { ok: true, removed: "p1" } }));
    fireEvent.click(within(project()).getByRole("button", { name: "Remover do Nants" }));
    const dialog = screen.getByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("checkbox", { name: /volumes de dados/ }));
    const historyLength = window.history.length;
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Remover do Nants" }));
    });
    await settle();
    expect(server.calls.find((c) => c.method === "DELETE")?.body).toEqual({ remove_volumes: true });
    expect(window.location.search).toBe("?space=p2");
    // O removido sai do histórico (replaceState): voltar não leva a um espaço inexistente.
    expect(window.history.length).toBe(historyLength);
    expect(toast()?.textContent).toContain('"Loja" saiu do Nants. A pasta continua no disco.');
    expect(appStore.getState().config?.projects.map((p) => p.id)).toEqual(["p2"]);
    // O evento SSE da própria remoção chega depois: nada muda, nenhum aviso de "outro cliente".
    await act(async () => {
      stream().emit("project_removed", { event_type: "project_removed", project_id: null, payload: { project_id: "p1" } });
    });
    await settle();
    expect(toast()?.textContent).not.toContain(REMOVED_NOTICE);
    expect(window.location.search).toBe("?space=p2");
  });

  it("404 (já removido) é sucesso: o espaço fecha sem erro; com o envio em voo, 'Removendo' e um único DELETE", async () => {
    const { server, fetchMock } = await openProject();
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => (release = resolve));
    const base = fetchMock.getMockImplementation();
    fetchMock.mockImplementation(async (input: string | URL | Request, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      if (init?.method === "DELETE" && url === "/api/projects/p1") {
        server.calls.push({ method: "DELETE", url, body: undefined });
        await gate;
        return new Response(JSON.stringify({ error: "projeto não encontrado", code: "project_not_found" }), { status: 404, headers: { "Content-Type": "application/json" } });
      }
      if (base === undefined) throw new Error("fetch ausente");
      return base(input, init);
    });
    fireEvent.click(within(project()).getByRole("button", { name: "Remover do Nants" }));
    const dialog = screen.getByRole("alertdialog");
    const confirm = within(dialog).getByRole("button", { name: "Remover do Nants" });
    await act(async () => {
      fireEvent.click(confirm);
      fireEvent.click(confirm);
    });
    expect(within(dialog).getByRole("button", { name: "Removendo" }).hasAttribute("disabled")).toBe(true);
    await act(async () => release());
    await settle();
    expect(server.calls.filter((c) => c.method === "DELETE")).toHaveLength(1);
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(window.location.search).toBe("?space=p2");
    expect(appStore.getState().config?.projects.map((p) => p.id)).toEqual(["p2"]);
  });

  it("409 de execução em andamento: mensagem explicativa e o diálogo continua", async () => {
    const { server } = await openProject();
    server.reply("DELETE", /^\/api\/projects\/p1$/, () => ({ status: 409, body: { error: "já existe uma execução em andamento neste projeto", code: "run_active" } }));
    fireEvent.click(within(project()).getByRole("button", { name: "Remover do Nants" }));
    const dialog = screen.getByRole("alertdialog");
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Remover do Nants" }));
    });
    await settle();
    expect(within(dialog).getByRole("alert").textContent).toBe("Há uma execução em andamento neste projeto. Aguarde terminar para remover.");
    expect(window.location.search).toBe("?space=p1");
  });

  it("removendo o único projeto, o desktop vai para as boas-vindas", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: states().p1 as ProjectState } });
    server.reply("DELETE", /^\/api\/projects\/p1$/, () => ({ status: 200, body: { ok: true, removed: "p1" } }));
    await act(async () => {
      fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Projeto" }));
    });
    await settle();
    fireEvent.click(within(project()).getByRole("button", { name: "Remover do Nants" }));
    await act(async () => {
      fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Remover do Nants" }));
    });
    await settle();
    expect(window.location.search).toBe("");
    expect(window.location.pathname).toBe("/");
    expect(screen.queryByRole("region", { name: "Projeto" })).toBeNull();
  });
});

describe("Projeto: remoção vinda de outra aba", () => {
  it("o espaço ativo removido fecha, aparece o aviso e o desktop vai para outro espaço", async () => {
    const { stream } = await openProject();
    await act(async () => {
      stream().emit("project_removed", { event_type: "project_removed", project_id: null, payload: { project_id: "p1" } });
    });
    await settle();
    expect(window.location.search).toBe("?space=p2");
    expect(toast()?.textContent).toContain(REMOVED_NOTICE);
    expect(screen.queryByRole("region", { name: "Projeto" })).toBeNull();
    await waitFor(() => expect(screen.getByRole("button", { name: /Projeto: Blog/ })).toBeTruthy());
  });

  it("outro espaço removido: o ativo continua, sem aviso; o removido sai da lista", async () => {
    const { stream } = await openProject();
    await act(async () => {
      stream().emit("project_removed", { event_type: "project_removed", project_id: null, payload: { project_id: "p2" } });
    });
    await settle();
    expect(window.location.search).toBe("?space=p1");
    expect(toast()).toBeNull();
    expect(appStore.getState().config?.projects.map((p) => p.id)).toEqual(["p1"]);
    expect(project()).toBeTruthy();
  });
});
