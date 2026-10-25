import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent } from "../api/types";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../test-utils";
import { ptBR } from "../i18n/ptBR";
import { BRAND_NAME, BRAND_PRODUCT_LINE_KEY, BRAND_SLOGAN } from "./brand";
import { findForbiddenGlyphs } from "./glyphs";
import { NO_ATTENTION, notReady, READY, renderShell, settle } from "./shell-test-utils";
import { appStore } from "../store";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const P1 = makeProject("p1", "Loja");
const P2 = makeProject("p2", "Blog");

function taskEvent(projectId: string): ApiEvent {
  return {
    event_type: "task_updated",
    project_id: projectId,
    payload: { spec_id: "s1", task_id: "T01", status: "IN_PROGRESS" },
  } as ApiEvent;
}

function topbar(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-topbar]");
  if (el === null) throw new Error("barra ausente");
  return el;
}

describe("Shell: boas-vindas e marca", () => {
  it("sem espaço: boas-vindas com o slogan, Novo projeto e Primeiros passos; a marca usa BRAND_SLOGAN", async () => {
    await renderShell({ projects: [P1] });
    const welcome = document.querySelector<HTMLElement>("[data-welcome]");
    if (welcome === null) throw new Error("boas-vindas ausentes");
    expect(within(welcome).getByText(BRAND_SLOGAN)).toBeTruthy();
    // Identidade (spec 100): logo, logo depois o slogan, e só então a linha de produto.
    const logo = welcome.querySelector(".nd-welcome-brand");
    const slogan = within(welcome).getByText(BRAND_SLOGAN);
    const product = within(welcome).getByText(ptBR[BRAND_PRODUCT_LINE_KEY]);
    expect(logo?.textContent).toBe(BRAND_NAME);
    expect(logo?.nextElementSibling).toBe(slogan);
    expect(slogan.nextElementSibling).toBe(product);
    expect(within(topbar()).getByText(BRAND_SLOGAN)).toBeTruthy();
    expect(within(topbar()).getByText("Nants")).toBeTruthy();
    expect(within(welcome).getByRole("button", { name: "Novo projeto" })).toBeTruthy();
    fireEvent.click(within(welcome).getByRole("button", { name: "Primeiros passos" }));
    expect(screen.getByRole("region", { name: "Primeiros passos" })).toBeTruthy();
    expect(findForbiddenGlyphs(document.body)).toEqual([]);
  });

  it("boas-vindas: Novo projeto abre o diálogo; um projeto da lista abre o espaço dele", async () => {
    await renderShell({ projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    const welcome = document.querySelector<HTMLElement>("[data-welcome]");
    if (welcome === null) throw new Error("boas-vindas ausentes");
    fireEvent.click(within(welcome).getByRole("button", { name: "Novo projeto" }));
    expect(screen.getByRole("dialog", { name: "Novo projeto" })).toBeTruthy();
    fireEvent.click(within(screen.getByRole("dialog", { name: "Novo projeto" })).getByRole("button", { name: "Cancelar" }));
    fireEvent.click(within(welcome).getByRole("button", { name: "Loja" }));
    await settle();
    expect(window.location.search).toBe("?space=p1");
  });
});

describe("Shell: espaços e URL", () => {
  it("qualquer caminho diferente de / é normalizado por replaceState, mantendo só o espaço", async () => {
    const replace = vi.spyOn(window.history, "replaceState");
    await renderShell({ url: "/um/caminho/p1?x=1", projects: [P1] });
    expect(`${window.location.pathname}${window.location.search}`).toBe("/");
    replace.mockClear();
    window.history.pushState({}, "", "/qualquer/coisa?space=p1#a");
    await act(async () => {
      window.dispatchEvent(new PopStateEvent("popstate"));
    });
    expect(`${window.location.pathname}${window.location.search}${window.location.hash}`).toBe("/?space=p1");
    expect(replace).toHaveBeenCalled();
    replace.mockRestore();
  });

  it("abrir um espaço aplica o layout da categoria da fase (planejamento: Conversa + Plano)", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Quadro" })).toBeNull();
  });

  it("a troca de espaço restaura o arranjo salvo da categoria e a seleção; o URL acompanha", async () => {
    const saved = {
      layouts: {
        planning: {
          current: { windows: { costs: { x: 20, y: 20, w: 400, h: 300, z: 1, mode: "normal", open: true } } },
          mine: null,
        },
      },
      selection: { taskId: "T02", filePath: "src/app.ts", conversationScroll: 120 },
    };
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }), p2: makeProjectState({ id: "p2", name: "Blog" }) },
      ui: { p2: { rev: 3, data: saved } },
    });
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
    fireEvent.click(within(topbar()).getByRole("button", { name: /Trocar de projeto/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Blog/ }));
    await settle();
    expect(window.location.search).toBe("?space=p2");
    expect(screen.getByRole("region", { name: "Custos" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Plano" })).toBeNull();
    expect(server.count("GET", /\/api\/projects\/p2\/ui-state/)).toBe(1);
  });

  it("o layout só troca quando a CATEGORIA da fase muda", async () => {
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    });
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
    // planning → waiting_approval: mesma categoria; nada muda.
    await act(async () => {
      stream().emit("sdd_draft_updated", { event_type: "sdd_draft_updated", project_id: "p1", payload: { spec_id: "s1" } });
    });
    await settle();
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
    // executing: categoria execução.
    await act(async () => {
      stream().emit("project_status", { event_type: "project_status", project_id: "p1", payload: { status: "executing", spec_id: "s1" } });
    });
    await settle();
    expect(screen.getByRole("region", { name: "Quadro" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Plano" })).toBeNull();
  });

  it("projeto inexistente: 'Projeto não encontrado' e 'Voltar ao início'", async () => {
    await renderShell({ url: "/?space=zz", projects: [P1] });
    expect(screen.getByRole("alert").textContent).toContain("Projeto não encontrado");
    fireEvent.click(screen.getByRole("button", { name: "Voltar ao início" }));
    await settle();
    expect(window.location.search).toBe("");
    expect(document.querySelector("[data-welcome]")).not.toBeNull();
  });
});

describe("Shell: modal aberto bloqueia os atalhos do desktop", () => {
  it("com o Novo projeto aberto, F6, Alt+1, Ctrl+Z, Shift+/ e Ctrl+K não agem; o foco fica no diálogo", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    // Uma mudança de layout desfazível antes, para o Ctrl+Z ter o que desfazer.
    fireEvent.click(within(screen.getByRole("region", { name: "Plano" })).getByRole("button", { name: "Fechar" }));
    const layout = (): string => Array.from(document.querySelectorAll("[data-window]")).map((el) => `${el.getAttribute("data-window")}:${(el as HTMLElement).style.zIndex}:${el.hasAttribute("hidden")}`).join("|");
    fireEvent.click(within(topbar()).getByRole("button", { name: /Trocar de projeto/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Novo projeto" }));
    const dialog = screen.getByRole("dialog", { name: "Novo projeto" });
    const field = within(dialog).getByLabelText("Nome do projeto");
    expect(document.activeElement).toBe(field);
    const before = layout();
    const button = within(dialog).getByRole("button", { name: "Cancelar" });
    button.focus();
    for (const key of [
      { key: "F6" },
      { key: "F6", shiftKey: true },
      { key: "1", code: "Digit1", altKey: true },
      { key: "z", ctrlKey: true },
      { key: "?", shiftKey: true, code: "Slash" },
      { key: "k", ctrlKey: true },
    ]) {
      fireEvent.keyDown(button, key);
    }
    expect(layout()).toBe(before);
    expect(screen.queryByRole("region", { name: "Plano" })).toBeNull();
    expect(screen.queryByRole("dialog", { name: "Lançador" })).toBeNull();
    expect(document.querySelectorAll('[aria-modal="true"]').length).toBe(1);
    expect(dialog.contains(document.activeElement)).toBe(true);
    // O Esc continua fechando o que está por cima.
    fireEvent.keyDown(button, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Novo projeto" })).toBeNull();
  });

  it("com o lançador aberto, F6 e Alt+1 não mexem nas janelas", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    const input = within(screen.getByRole("dialog", { name: "Lançador" })).getByRole("combobox");
    fireEvent.keyDown(input, { key: "1", code: "Digit1", altKey: true });
    fireEvent.keyDown(input, { key: "F6" });
    expect(document.activeElement).toBe(input);
  });
});

describe("Shell: menus", () => {
  it("Tab fecha o menu e devolve o foco ao botão que o abriu", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    const button = within(topbar()).getByRole("button", { name: /Layout:/ });
    fireEvent.click(button);
    const menu = screen.getByRole("menu", { name: "Layout" });
    fireEvent.keyDown(menu, { key: "Tab" });
    expect(screen.queryByRole("menu", { name: "Layout" })).toBeNull();
    expect(document.activeElement).toBe(button);
  });
});

describe("Shell: barra superior", () => {
  it("seletor de espaços: projetos com estado e 'Novo projeto'; Esc fecha e devolve o foco", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      attention: { setup_incomplete: false, projects: [{ project_id: "p2", name: "Blog", pending_permissions: [], pending_choices: true, plan_awaiting_approval: false, paused_or_stalled: null }] },
    });
    const button = within(topbar()).getByRole("button", { name: /Trocar de projeto/ });
    fireEvent.click(button);
    const menu = screen.getByRole("menu", { name: "Projetos" });
    expect(within(menu).getByRole("menuitemradio", { name: /Loja/ }).getAttribute("aria-checked")).toBe("true");
    expect(within(menu).getByRole("menuitemradio", { name: /Blog/ }).textContent).toContain("Precisa de você (1)");
    expect(within(menu).getByRole("menuitem", { name: "Novo projeto" })).toBeTruthy();
    expect(findForbiddenGlyphs(menu)).toEqual([]);
    fireEvent.keyDown(menu, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "Projetos" })).toBeNull();
    expect(document.activeElement).toBe(button);
    fireEvent.click(button);
    fireEvent.click(within(screen.getByRole("menu", { name: "Projetos" })).getByRole("menuitem", { name: "Novo projeto" }));
    expect(screen.getByRole("dialog", { name: "Novo projeto" })).toBeTruthy();
  });

  it("menu Layout: aplicar o padrão mostra o aviso com Desfazer, que desfaz a mudança", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    // Fecha o Plano: o padrão o traz de volta.
    fireEvent.click(within(screen.getByRole("region", { name: "Plano" })).getByRole("button", { name: "Fechar" }));
    expect(screen.queryByRole("region", { name: "Plano" })).toBeNull();
    fireEvent.click(within(topbar()).getByRole("button", { name: /Layout: Planejamento/ }));
    const menu = screen.getByRole("menu", { name: "Layout" });
    expect(findForbiddenGlyphs(menu)).toEqual([]);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Aplicar o padrão" }));
    await settle(3);
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
    const toast = document.querySelector<HTMLElement>("[data-toast]");
    if (toast === null) throw new Error("aviso ausente");
    expect(toast.textContent).toContain("Layout padrão aplicado");
    fireEvent.click(within(toast).getByRole("button", { name: "Desfazer" }));
    expect(screen.queryByRole("region", { name: "Plano" })).toBeNull();
  });

  it("o aviso de layout some depois de 8 s", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    vi.useFakeTimers();
    fireEvent.click(within(topbar()).getByRole("button", { name: /Layout:/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Salvar como meu layout desta fase" }));
    expect(document.querySelector("[data-toast]")?.textContent).toContain("Layout salvo");
    expect(document.querySelector("[data-undo]")).toBeNull();
    await act(async () => {
      vi.advanceTimersByTime(8_000);
    });
    expect(document.querySelector("[data-toast]")).toBeNull();
  });

  it("pílula: fase, título da spec, tarefa; Parar pede confirmação e chama a rota", async () => {
    const spec = makeSpec("s1", "Carrinho de compras", "IN_PROGRESS", [
      makeTask("T01", "a", "DONE"),
      makeTask("T02", "b", "IN_PROGRESS"),
      makeTask("T03", "c", "TODO"),
    ]);
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", running: true, run: { spec_id: "s1", status: "running" }, specs: [spec] }) },
    });
    server.reply("POST", /\/stop$/, () => ({ status: 202, body: { stopping: true } }));
    const pill = document.querySelector<HTMLElement>("[data-run-pill]");
    if (pill === null) throw new Error("pílula ausente");
    expect(pill.textContent).toContain("Executando");
    expect(pill.textContent).toContain("Carrinho de compras");
    expect(pill.textContent).toContain("T02 de 3");
    expect(findForbiddenGlyphs(pill)).toEqual([]);
    fireEvent.click(within(pill).getByRole("button", { name: "Parar" }));
    const dialog = screen.getByRole("alertdialog", { name: "Parar a execução?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Parar execução" }));
    await settle();
    expect(server.count("POST", /\/api\/projects\/p1\/specs\/s1\/stop$/)).toBe(1);
    expect(within(pill).getByRole("button", { name: "Parando" }).hasAttribute("disabled")).toBe(true);
  });

  it("pílula (spec 123 T02): durante os testes a fase é 'Testes · 7 min', âmbar perto do limite; no fim volta a 'Executando'", async () => {
    const spec = makeSpec("s1", "Carrinho", "IN_PROGRESS", [makeTask("T01", "a", "IN_PROGRESS")]);
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", running: true, run: { spec_id: "s1", status: "running" }, specs: [spec] }) },
    });
    const pill = (): HTMLElement => {
      const el = document.querySelector<HTMLElement>("[data-run-pill]");
      if (el === null) throw new Error("pílula ausente");
      return el;
    };
    const emit = (payload: Record<string, unknown>): void =>
      stream().emit("agent_activity", { event_type: "agent_activity", project_id: "p1", payload: { agent: "tests", task_id: "T01", activity_id: "T01-1-3", ...payload } });
    expect(pill().querySelector("[data-run-operation]")).toBeNull();
    await act(async () => emit({ kind: "command", text: "npx playwright test", command_started_at: Date.now() - 420_000, command_limit_ms: 600_000 }));
    await settle();
    const op = pill().querySelector<HTMLElement>("[data-run-operation]");
    expect(op?.textContent).toBe("Testes · 7 min");
    expect(op?.getAttribute("data-near-limit")).toBeNull();
    // O rótulo da fase continua no `status`, para leitores de tela, fora da vista.
    expect(within(pill()).getByRole("status").textContent).toBe("Executando");
    expect(within(pill()).getByRole("status").className).toBe("nd-sr-only");
    expect(findForbiddenGlyphs(pill())).toEqual([]);
    await act(async () => emit({ activity_id: "T01-1-4", kind: "command", text: "npx playwright test", command_started_at: Date.now() - 500_000, command_limit_ms: 600_000 }));
    await settle();
    expect(pill().querySelector("[data-run-operation]")?.getAttribute("data-near-limit")).toBe("true");
    await act(async () => emit({ activity_id: "T01-1-4", kind: "end" }));
    await settle();
    expect(pill().querySelector("[data-run-operation]")).toBeNull();
    expect(within(pill()).getByRole("status").className).toBe("nd-run-phase");
  });

  it("pílula: Cancelar não chama a rota; erro da rota de Parar aparece no diálogo e o Parar continua utilizável", async () => {
    const spec = makeSpec("s1", "Carrinho", "IN_PROGRESS", [makeTask("T01", "a", "IN_PROGRESS")]);
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", running: true, run: { spec_id: "s1", status: "running" }, specs: [spec] }) },
    });
    server.reply("POST", /\/stop$/, () => ({ status: 409, body: { error: "não há execução em andamento desta spec", code: "no_run_for_spec" } }));
    const pill = document.querySelector<HTMLElement>("[data-run-pill]");
    if (pill === null) throw new Error("pílula ausente");
    fireEvent.click(within(pill).getByRole("button", { name: "Parar" }));
    fireEvent.click(within(screen.getByRole("alertdialog", { name: "Parar a execução?" })).getByRole("button", { name: "Cancelar" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(server.count("POST", /\/stop$/)).toBe(0);

    fireEvent.click(within(pill).getByRole("button", { name: "Parar" }));
    const dialog = screen.getByRole("alertdialog", { name: "Parar a execução?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Parar execução" }));
    await settle();
    expect(within(dialog).getByRole("alert").textContent).toBe("não há execução em andamento desta spec");
    const stop = within(pill).getByRole("button", { name: "Parar" }) as HTMLButtonElement;
    expect(stop.disabled).toBe(false);
    expect((within(dialog).getByRole("button", { name: "Parar execução" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("pílula: depois de parar, o estado sem execução tira o Parar", async () => {
    const spec = makeSpec("s1", "Carrinho", "IN_PROGRESS", [makeTask("T01", "a", "IN_PROGRESS")]);
    const { server, stream } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", running: true, run: { spec_id: "s1", status: "running" }, specs: [spec] }) },
    });
    server.reply("POST", /\/stop$/, () => ({ status: 202, body: { stopping: true } }));
    const pill = (): HTMLElement => {
      const el = document.querySelector<HTMLElement>("[data-run-pill]");
      if (el === null) throw new Error("pílula ausente");
      return el;
    };
    fireEvent.click(within(pill()).getByRole("button", { name: "Parar" }));
    fireEvent.click(within(screen.getByRole("alertdialog", { name: "Parar a execução?" })).getByRole("button", { name: "Parar execução" }));
    await settle();
    expect(within(pill()).getByRole("button", { name: "Parando" })).toBeTruthy();
    server.model.states.p1 = makeProjectState({ id: "p1", name: "Loja", running: false, run: null, specs: [spec] });
    act(() => stream().emit("project_status", { event_type: "project_status", project_id: "p1", payload: { status: "idle", reason: "stopped" } }));
    await settle();
    expect(within(pill()).queryByRole("button", { name: /^Parar|Parando/ })).toBeNull();
  });

  it("pílula: execução parada mostra Continuar (resume); erro aparece em role=alert", async () => {
    const spec = makeSpec("s1", "Blog", "IN_PROGRESS", [makeTask("T01", "a", "DONE"), makeTask("T02", "b", "TODO")]);
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [spec] }) },
    });
    server.reply("POST", /\/resume$/, () => ({ status: 412, body: { error: "prereq_missing", missing: ["docker_daemon"] } }));
    const pill = document.querySelector<HTMLElement>("[data-run-pill]");
    if (pill === null) throw new Error("pílula ausente");
    expect(pill.textContent).toContain("Parado");
    fireEvent.click(within(pill).getByRole("button", { name: "Continuar" }));
    await settle();
    expect(server.count("POST", /\/specs\/s1\/resume$/)).toBe(1);
    expect(document.querySelector("[data-bar-alert='run']")?.textContent).toContain("Docker daemon");
  });

  it("pílula: destaque ao integrar (data-flash) por alguns segundos", async () => {
    const spec = makeSpec("s1", "Blog", "COMPLETED", [makeTask("T01", "a", "DONE")]);
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [spec] }) } });
    const pill = document.querySelector<HTMLElement>("[data-run-pill]");
    expect(pill?.getAttribute("data-phase")).toBe("integrated");
    expect(pill?.getAttribute("data-flash")).toBe("true");
  });

  it("095/T07 A-01: o título da spec fica no encaixe que se esconde, e continua na dica e no rótulo", async () => {
    const spec = makeSpec("s1", "Blog com comentários", "COMPLETED", [makeTask("T01", "a", "DONE")]);
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [spec] }) } });
    const pill = document.querySelector<HTMLElement>("[data-run-pill]");
    if (pill === null) throw new Error("pílula ausente");
    expect(pill.getAttribute("role")).toBe("group");
    expect(pill.getAttribute("aria-label")).toBe("Concluído: Blog com comentários");
    const slot = pill.querySelector<HTMLElement>("[data-run-title-slot]");
    expect(slot?.getAttribute("title")).toBe("Blog com comentários");
    expect(slot?.querySelector(".nd-run-title")?.textContent).toBe("Blog com comentários");
  });

  it("Aprovação automática: ligar pede confirmação; desligar é direto. Internet: confirmação com o aviso de busca", async () => {
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", web_search_available: false }) },
    });
    server.reply("PUT", /\/yolo$/, () => {
      const current = server.model.states.p1;
      if (current !== undefined) server.model.states.p1 = { ...current, yolo: true };
      return { status: 200, body: server.model.states.p1 };
    });
    server.reply("PUT", /\/internet$/, () => ({ status: 200, body: { ...server.model.states.p1, architect_internet: true } }));
    const yolo = within(topbar()).getByRole("switch", { name: "Aprovação automática" });
    expect(yolo.getAttribute("aria-checked")).toBe("false");
    // Rótulo curto na barra e a dica completa no title (095/T08, decisão 12).
    expect(yolo.textContent).toBe("Automático");
    expect(yolo.getAttribute("title")).toBe("Aprovação automática: os agentes seguem sem pedir sua permissão");
    fireEvent.click(yolo);
    const dialog = screen.getByRole("dialog", { name: "Ligar a aprovação automática?" });
    expect(dialog.textContent).toContain("A aprovação automática não é um sandbox: ela só reduz as perguntas.");
    expect(dialog.textContent).not.toContain("YOLO");
    fireEvent.click(within(dialog).getByRole("button", { name: "Ligar a aprovação automática" }));
    await settle();
    expect(server.count("PUT", /\/yolo$/)).toBe(1);
    expect(screen.queryByRole("dialog", { name: "Ligar a aprovação automática?" })).toBeNull();
    const on = within(topbar()).getByRole("switch", { name: "Aprovação automática" });
    expect(on.getAttribute("aria-checked")).toBe("true");
    expect(on.textContent).toBe("Automático");
    expect(topbar().textContent).not.toContain("YOLO");

    const internet = within(topbar()).getByRole("switch", { name: "Arquiteto pode consultar a internet" });
    fireEvent.click(internet);
    const confirm = screen.getByRole("dialog", { name: "Permitir que o Arquiteto consulte a internet?" });
    expect(confirm.querySelector("[data-internet-no-search]")).not.toBeNull();
    fireEvent.click(within(confirm).getByRole("button", { name: "Permitir" }));
    await settle();
    expect(within(topbar()).getByRole("switch", { name: "Arquiteto pode consultar a internet" }).getAttribute("aria-checked")).toBe("true");
    expect(findForbiddenGlyphs(topbar())).toEqual([]);
  });

  it("erro da aprovação automática aparece abaixo da barra (role=alert)", async () => {
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", yolo: true }) },
    });
    server.reply("PUT", /\/yolo$/, () => ({ status: 500, body: {} }));
    fireEvent.click(within(topbar()).getByRole("switch", { name: "Aprovação automática" }));
    await settle();
    expect(document.querySelector("[data-bar-alert='yolo']")?.textContent).toContain("Não foi possível alterar a aprovação automática");
  });

  it("Internet: desligada por padrão, sem explicação fora do diálogo; com busca disponível, sem o aviso; Cancelar não chama a API", async () => {
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", web_search_available: true }) },
    });
    const internet = within(topbar()).getByRole("switch", { name: "Arquiteto pode consultar a internet" });
    expect(internet.getAttribute("aria-checked")).toBe("false");
    expect(screen.queryByText(/Só vale no plano e na recuperação/)).toBeNull();
    fireEvent.click(internet);
    const confirm = screen.getByRole("dialog", { name: "Permitir que o Arquiteto consulte a internet?" });
    expect(confirm.querySelector("[data-internet-no-search]")).toBeNull();
    fireEvent.click(within(confirm).getByRole("button", { name: "Cancelar" }));
    expect(screen.queryByRole("dialog", { name: "Permitir que o Arquiteto consulte a internet?" })).toBeNull();
    expect(server.count("PUT", /\/internet$/)).toBe(0);
    expect(internet.getAttribute("aria-checked")).toBe("false");
  });

  it("Internet: erro ao permitir reverte o interruptor e aparece em role=alert no diálogo", async () => {
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", architect_internet: false }) },
    });
    server.reply("PUT", /\/internet$/, () => ({ status: 422, body: { error: "ruim" } }));
    fireEvent.click(within(topbar()).getByRole("switch", { name: "Arquiteto pode consultar a internet" }));
    const confirm = screen.getByRole("dialog", { name: "Permitir que o Arquiteto consulte a internet?" });
    fireEvent.click(within(confirm).getByRole("button", { name: "Permitir" }));
    await settle();
    expect(within(confirm).getByRole("alert").textContent).toBe("Não foi possível alterar o acesso à internet do Arquiteto: dados inválidos");
    expect(within(topbar()).getByRole("switch", { name: "Arquiteto pode consultar a internet" }).getAttribute("aria-checked")).toBe("false");
    expect(server.count("PUT", /\/internet$/)).toBe(1);
  });

  it("Internet: desligar é direto, sem diálogo; erro ao desligar aparece abaixo da barra e o estado volta", async () => {
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", architect_internet: true }) },
    });
    server.reply("PUT", /\/internet$/, () => ({ status: 200, body: makeProjectState({ id: "p1", name: "Loja", architect_internet: false }) }));
    const internet = (): HTMLElement => within(topbar()).getByRole("switch", { name: "Arquiteto pode consultar a internet" });
    expect(internet().getAttribute("aria-checked")).toBe("true");
    fireEvent.click(internet());
    expect(screen.queryByRole("dialog")).toBeNull();
    await settle();
    expect(server.calls.filter((c) => c.method === "PUT" && c.url.endsWith("/internet")).map((c) => c.body)).toEqual([{ enabled: false }]);
    expect(internet().getAttribute("aria-checked")).toBe("false");

    // Ligado de novo pelo servidor; agora o desligar falha.
    act(() => appStore.setState((s) => ({ projectStates: { ...s.projectStates, p1: makeProjectState({ id: "p1", name: "Loja", architect_internet: true }) } })));
    server.reply("PUT", /\/internet$/, () => ({ status: 500, body: { error: "falha" } }));
    fireEvent.click(internet());
    await settle();
    expect(document.querySelector("[data-bar-alert='internet']")?.textContent).toBe("Não foi possível alterar o acesso à internet do Arquiteto: falha interna do servidor");
    expect(internet().getAttribute("aria-checked")).toBe("true");
  });

  it("selo 'Setup incompleto' aparece com a prontidão pendente e reabre os Primeiros passos", async () => {
    await renderShell({ projects: [P1], readiness: { ...READY, execute: notReady().execute } });
    const badge = within(topbar()).getByRole("button", { name: "Setup incompleto" });
    expect(screen.queryByRole("region", { name: "Primeiros passos" })).toBeNull();
    fireEvent.click(badge);
    expect(screen.getByRole("region", { name: "Primeiros passos" })).toBeTruthy();
  });

  it("sem pendências, sem selo", async () => {
    await renderShell({ projects: [P1] });
    expect(within(topbar()).queryByRole("button", { name: "Setup incompleto" })).toBeNull();
  });

  it("sino: contagem do /api/attention (permissões, escolhas, plano, pausa e setup) e painel da T06", async () => {
    const panel = vi.fn(({ onClose }: { onClose: () => void }) => (
      <div role="region" aria-label="Precisa de você">
        <button type="button" onClick={onClose}>
          Fechar painel
        </button>
      </div>
    ));
    await renderShell({
      projects: [P1, P2],
      renderAttentionPanel: panel,
      attention: {
        setup_incomplete: true,
        projects: [
          {
            project_id: "p2",
            name: "Blog",
            pending_permissions: [{ request_id: "r1", agent: "developer", kind: "command", always_allowed: false }],
            pending_choices: true,
            plan_awaiting_approval: true,
            paused_or_stalled: { kind: "stalled", spec_id: "s1" },
          },
        ],
      },
    });
    const bell = within(topbar()).getByRole("button", { name: "Precisa de você, 5 pendências" });
    expect(bell.querySelector("[data-badge]")?.textContent).toBe("5");
    fireEvent.click(bell);
    expect(screen.getByRole("region", { name: "Precisa de você" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Fechar painel" }));
    expect(screen.queryByRole("region", { name: "Precisa de você" })).toBeNull();
  });

  it("Buscar abre o lançador e mostra Ctrl+K", async () => {
    await renderShell({ projects: [P1] });
    const search = within(topbar()).getByRole("button", { name: /Buscar/ });
    expect(search.getAttribute("aria-keyshortcuts")).toBe("Control+K");
    expect(search.querySelector("kbd")?.textContent).toBe("Ctrl+K");
    fireEvent.click(search);
    expect(screen.getByRole("dialog", { name: "Lançador" })).toBeTruthy();
  });
});

describe("Shell: useSpaceData e eventos", () => {
  it("com N janelas abertas, um task_updated gera exatamente 1 GET de estado", async () => {
    const saved = {
      layouts: {
        planning: {
          current: {
            windows: Object.fromEntries(
              ["conversation", "plan", "board", "activity", "files", "costs"].map((k, i) => [k, { x: 10 * i, y: 10 * i, w: 400, h: 300, z: i + 1, mode: "normal", open: true }]),
            ),
          },
          mine: null,
        },
      },
    };
    const { server, stream } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      ui: { p1: { rev: 1, data: saved } },
    });
    expect(document.querySelectorAll("[data-window]").length).toBe(6);
    const before = server.count("GET", /\/api\/projects\/p1\/state$/);
    await act(async () => {
      stream().emit("task_updated", taskEvent("p1"));
    });
    await settle();
    expect(server.count("GET", /\/api\/projects\/p1\/state$/)).toBe(before + 1);
  });

  it("uma única conexão SSE, no fluxo global /api/events, mesmo trocando de espaço", async () => {
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }), p2: makeProjectState({ id: "p2", name: "Blog" }) },
    });
    const { FakeEventSource } = await import("../test-utils");
    expect(FakeEventSource.instances.length).toBe(1);
    expect(stream().url).toBe("/api/events");
    fireEvent.click(within(topbar()).getByRole("button", { name: /Trocar de projeto/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Blog/ }));
    await settle();
    expect(FakeEventSource.instances.filter((s) => s.readyState !== 2).length).toBe(1);
    expect(FakeEventSource.instances.length).toBe(1);
  });

  it("erro de carga do estado: 'Tentar de novo' recarrega", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    server.reply("GET", /\/api\/projects\/p1\/state$/, () => ({ status: 500, body: {} }));
    // Aviso de mudança do projeto: a recarga do espaço falha e o erro aparece.
    const { appStore } = await import("../store");
    await act(async () => {
      appStore.setState((s) => ({ revisions: { ...s.revisions, p1: (s.revisions.p1 ?? 0) + 1 } }));
    });
    await settle();
    const alert = document.querySelector<HTMLElement>("[data-bar-alert='load']");
    if (alert === null) throw new Error("erro ausente");
    server.reply("GET", /\/api\/projects\/p1\/state$/, () => ({ status: 200, body: server.model.states.p1 }));
    const before = server.count("GET", /\/api\/projects\/p1\/state$/);
    fireEvent.click(within(alert).getByRole("button", { name: "Tentar de novo" }));
    await settle();
    expect(server.count("GET", /\/api\/projects\/p1\/state$/)).toBe(before + 1);
    expect(document.querySelector("[data-bar-alert='load']")).toBeNull();
  });

  it("carregando: cartão 'Carregando projeto' até o estado chegar", async () => {
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const { installFetch, installFakeEventSource, resetStore } = await import("../test-utils");
    const { fakeNantsServer, TEST_UI_DEPS } = await import("./shell-test-utils");
    const { Shell } = await import("./Shell");
    const { render } = await import("@testing-library/react");
    resetStore();
    installFakeEventSource();
    const server = fakeNantsServer({ projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    installFetch(async (method, url, body) => {
      if (/\/state$/.test(url)) await gate;
      return server.handler(method, url, body);
    });
    window.history.replaceState({}, "", "/?space=p1");
    render(<Shell uiStateDeps={TEST_UI_DEPS} attentionIntervalMs={0} createObserver={null} />);
    await settle();
    expect(document.querySelector("[data-space-loading]")?.textContent).toContain("Carregando projeto");
    release();
    await settle();
    expect(document.querySelector("[data-space-loading]")).toBeNull();
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
  });

  it("pedidos órfãos: permissões pendentes antes do estado aparecem como aviso", async () => {
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const { installFetch, installFakeEventSource, resetStore } = await import("../test-utils");
    const { fakeNantsServer, TEST_UI_DEPS } = await import("./shell-test-utils");
    const { Shell } = await import("./Shell");
    const { render } = await import("@testing-library/react");
    const { FakeEventSource } = await import("../test-utils");
    resetStore();
    installFakeEventSource();
    const server = fakeNantsServer({ projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    installFetch(async (method, url, body) => {
      if (/\/state$/.test(url)) await gate;
      return server.handler(method, url, body);
    });
    window.history.replaceState({}, "", "/?space=p1");
    render(<Shell uiStateDeps={TEST_UI_DEPS} attentionIntervalMs={0} createObserver={null} />);
    await settle();
    await act(async () => {
      FakeEventSource.instances[0]?.emit("permission_requested", {
        event_type: "permission_requested",
        project_id: "p1",
        payload: { request_id: "r1", project_id: "p1", task_id: null, agent: "architect", command: "npm i" },
      });
    });
    expect(document.querySelector("[data-orphan-permissions]")?.textContent).toContain("Um pedido de permissão aguarda você");
    // Sem o estado, o pedido continua decidível (aviso flutuante com as decisões).
    expect(screen.getAllByRole("button", { name: "Permitir uma vez" }).length).toBeGreaterThan(0);
    release();
    await settle();
  });

  it("erro de carga do estado com pedido pendente: o erro aparece e o pedido segue decidível", async () => {
    const { server, stream } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    server.reply("GET", /\/api\/projects\/p1\/state$/, () => ({ status: 500, body: {} }));
    const { appStore } = await import("../store");
    await act(async () => {
      appStore.setState((s) => ({ revisions: { ...s.revisions, p1: (s.revisions.p1 ?? 0) + 1 } }));
    });
    await act(async () => {
      stream().emit("permission_requested", {
        event_type: "permission_requested",
        project_id: "p1",
        payload: { request_id: "r9", project_id: "p1", task_id: null, agent: "architect", command: "npm ci" },
      });
    });
    await settle();
    expect(document.querySelector("[data-bar-alert='load']")).not.toBeNull();
    expect(screen.getAllByRole("button", { name: "Permitir uma vez" }).length).toBeGreaterThan(0);
  });

  it("/api/attention: recarregado na reconexão do stream e na retomada da aba", async () => {
    const { server } = await renderShell({ projects: [P1] });
    const { FakeEventSource } = await import("../test-utils");
    const first = FakeEventSource.instances[0];
    if (first === undefined) throw new Error("stream ausente");
    await act(async () => {
      first.onopen?.(new Event("open"));
    });
    // 095/T07: determinístico. Cada passo espera pela CONDIÇÃO (nova conexão, nova consulta), nunca
    // por um tempo fixo; e a contagem de base só é lida depois de tudo da abertura assentar.
    await settle();
    const attention = (): number => server.count("GET", /^\/api\/attention$/);
    const before = attention();
    // Queda e reconexão (1 s de espera na primeira tentativa).
    await act(async () => {
      first.onerror?.(new Event("error"));
    });
    const second = await waitFor(
      () => {
        const next = FakeEventSource.instances[1];
        if (next === undefined) throw new Error("reconexão ainda não aberta");
        return next;
      },
      { timeout: 5_000, interval: 20 },
    );
    expect(second.url).toBe("/api/events");
    await act(async () => {
      second.onopen?.(new Event("open"));
    });
    await waitFor(() => expect(attention()).toBe(before + 1), { timeout: 5_000, interval: 20 });
    await settle();
    const afterReconnect = attention();
    expect(afterReconnect).toBe(before + 1);
    // Retomada da aba: oculta por mais de 2 s e de volta.
    // O tempo oculta é medido pelo relógio monotônico (114/T01), não pelo de parede.
    let fake = performance.now();
    const nowSpy = vi.spyOn(performance, "now").mockImplementation(() => fake);
    const visibility = vi.spyOn(document, "visibilityState", "get");
    visibility.mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    fake += 5_000;
    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    nowSpy.mockRestore();
    await waitFor(() => expect(attention()).toBe(afterReconnect + 1), { timeout: 5_000, interval: 20 });
    await settle();
    expect(attention()).toBe(afterReconnect + 1);
    visibility.mockRestore();
  });

  it("/api/attention: abertura, eventos relevantes e lagged, com no máximo 1 consulta a cada 500 ms", async () => {
    const { server, stream } = await renderShell({ projects: [P1], attentionIntervalMs: 500 });
    expect(server.count("GET", /^\/api\/attention$/)).toBe(1);
    // Rajada de eventos relevantes dentro da janela: no máximo uma consulta a mais, no fim dela.
    await act(async () => {
      for (let i = 0; i < 5; i += 1) stream().emit("task_updated", taskEvent("p1"));
      stream().emit("lagged", { skipped: 3 });
    });
    await settle(5);
    expect(server.count("GET", /^\/api\/attention$/)).toBeLessThanOrEqual(2);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 650));
    });
    await settle(5);
    expect(server.count("GET", /^\/api\/attention$/)).toBe(2);
    // Evento irrelevante (atividade) não consulta.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 550));
      stream().emit("agent_activity", {
        event_type: "agent_activity",
        project_id: "p1",
        payload: { agent: "developer", activity_id: "a1", kind: "start" },
      });
    });
    await settle(5);
    expect(server.count("GET", /^\/api\/attention$/)).toBe(2);
    expect(NO_ATTENTION.projects).toEqual([]);
  });
});
