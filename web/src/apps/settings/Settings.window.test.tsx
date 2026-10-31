/*
 * Janela Configurações montada pelo shell (spec 093, T08): abas (teclado, `selection`), abertura
 * direta por `openSettings` (o `settingsRequest` é aplicado e consumido), o que falta por aba,
 * conexões e protocolos (só a máscara; a chave só no campo password, limpo no envio),
 * agentes, preços com "US$ por imagem", máquina, aparência e a pasta de projetos.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionView } from "../../api/types";
import type { SettingsWindowTab } from "../../desktop/api";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { setLanguage } from "../../i18n";
import { ptBR } from "../../i18n/ptBR";
import { activateApp, notReady, renderShell, type RenderShellOptions, settle } from "../../desktop/shell-test-utils";
import { useShell } from "../../desktop/shellContext";
import { fakeResizeObservers } from "../../desktop/test-utils";
import { appStore } from "../../store";
import { makeProject, makeProjectState } from "../../test-utils";
import { DEFAULT_APPS } from "../registry";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const CONN: ConnectionView = { id: "conn", name: "OpenAI direto", base_url: "https://api.openai.com/v1", api_key_masked: "********abcd" };

async function openSettings(extra: RenderShellOptions = {}) {
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    connections: [CONN],
    ...extra,
  });
  view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({
    status: 200,
    body: [
      { id: "git", label: "Git", ok: true, version: "git version 2.43.0" },
      { id: "docker_permission", label: "Permissão no socket do Docker", ok: false, detail: "permission denied", fix: "sudo usermod -aG docker $USER" },
    ],
  }));
  view.server.reply("GET", /^\/api\/config\/connections\/[^/]+\/models/, () => ({ status: 200, body: { models: [{ id: "model" }], truncated: false } }));
  view.server.reply("POST", /^\/api\/config\/pricing\/autofill/, () => ({ status: 200, body: {} }));
  view.server.reply("POST", /^\/api\/config\/pricing\/estimate/, () => ({ status: 200, body: {} }));
  view.server.reply("GET", /^\/api\/config\/pricing\/estimate/, () => ({ status: 200, body: {} }));
  await act(async () => {
    activateApp("Configurações");
  });
  await settle();
  return view;
}

/** O DOM inteiro serializado (texto e atributos), para provar que a chave não ficou em lugar nenhum. */
function serializedDom(): string {
  const values = [...document.querySelectorAll("input")].map((i) => i.value).join("\n");
  return `${new XMLSerializer().serializeToString(document.body)}\n${values}`;
}

function settings(): HTMLElement {
  return screen.getByRole("region", { name: "Configurações" });
}

function tab(name: string): HTMLElement {
  return within(settings()).getByRole("tab", { name: new RegExp(`^${name}`) });
}

/** Abre o menu "…" de uma conexão e escolhe o item (Testar ou Editar). */
async function connectionAction(name: string, item: "Testar" | "Editar"): Promise<void> {
  await act(async () => {
    fireEvent.click(within(settings()).getByRole("button", { name: `Mais ações de ${name}` }));
  });
  const menu = screen.getByRole("menu", { name: `Ações da conexão ${name}` });
  await act(async () => {
    fireEvent.click(within(menu).getByRole("menuitem", { name: item }));
  });
}

async function chooseTab(name: string): Promise<void> {
  await act(async () => {
    fireEvent.click(tab(name));
  });
  await settle();
}

describe("Configurações: abas", () => {
  it("seis abas num tablist; setas, Home e End trocam e focam; a aba ativa vai para selection", async () => {
    const { server } = await openSettings();
    const list = within(settings()).getByRole("tablist", { name: "Seções das Configurações" });
    expect(within(list).getAllByRole("tab").map((t) => t.textContent)).toEqual(["Conexões", "Agentes", "Preços", "Máquina", "Aparência", "Projetos"]);
    expect(tab("Conexões").getAttribute("aria-selected")).toBe("true");
    const panel = within(settings()).getByRole("tabpanel");
    expect(panel.getAttribute("aria-labelledby")).toBe(tab("Conexões").id);
    tab("Conexões").focus();
    fireEvent.keyDown(list, { key: "ArrowRight" });
    expect(tab("Agentes").getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(tab("Agentes"));
    fireEvent.keyDown(list, { key: "End" });
    expect(tab("Projetos").getAttribute("aria-selected")).toBe("true");
    fireEvent.keyDown(list, { key: "ArrowDown" });
    expect(tab("Conexões").getAttribute("aria-selected")).toBe("true");
    fireEvent.keyDown(list, { key: "ArrowUp" });
    expect(tab("Projetos").getAttribute("aria-selected")).toBe("true");
    await settle();
    expect(server.model.ui.p1?.data.selection).toMatchObject({ settingsTab: "projects" });
    expect(findForbiddenGlyphs(settings())).toEqual([]);
  });

  it("a aba salva em selection volta ao abrir", async () => {
    await openSettings({ ui: { p1: { rev: 1, data: { selection: { settingsTab: "machine" } } } } });
    expect(tab("Máquina").getAttribute("aria-selected")).toBe("true");
    expect(within(settings()).getByRole("heading", { name: "Máquina" })).toBeTruthy();
  });

  it("a aba Máquina mostra a versão do Nants vinda de GET /api/config (spec 103 T03)", async () => {
    await openSettings({ ui: { p1: { rev: 1, data: { selection: { settingsTab: "machine" } } } } });
    const line = settings().querySelector("[data-nants-version]");
    expect(line?.textContent).toBe("Nants versão 0.1.0");
  });

  it("a aba Máquina tem a seção Docker com os comandos do rótulo nants.project e o aviso de outra instalação (spec 138 T12)", async () => {
    await openSettings({ ui: { p1: { rev: 1, data: { selection: { settingsTab: "machine" } } } } });
    const docker = settings().querySelector("[data-docker-help]");
    expect(docker).not.toBeNull();
    expect(within(docker as HTMLElement).getByRole("heading", { name: "Docker" })).toBeTruthy();
    const commands = [...(docker as HTMLElement).querySelectorAll("[data-docker-command] code")].map((c) => c.textContent);
    expect(commands).toEqual([
      "docker ps -a --filter label=nants.project",
      "docker network ls --filter label=nants.project",
      "docker rm <id>",
      "docker network rm <id>",
    ]);
    expect(docker?.querySelector("[data-docker-warning]")?.textContent).toContain("podem ser de outra instalação do Nants");
    expect(findForbiddenGlyphs(settings())).toEqual([]);
    act(() => setLanguage("en", { persist: false }));
    expect(document.querySelector("[data-docker-warning]")?.textContent).toContain("may belong to another Nants installation");
  });

  it("a versão do Nants em Configurações segue o idioma (spec 103 T03, dicionário da spec 101)", async () => {
    await openSettings({ ui: { p1: { rev: 1, data: { selection: { settingsTab: "machine" } } } } });
    act(() => setLanguage("en", { persist: false }));
    const line = document.querySelector("[data-nants-version]");
    expect(line?.textContent).toBe("Nants version 0.1.0");
  });

  it("abas no slot da barra de título (095/T06, C-07), horizontais; estreita, só o ícone com o nome para o leitor", async () => {
    const observers = fakeResizeObservers();
    await openSettings({ createObserver: observers.factory });
    const list = within(settings()).getByRole("tablist");
    expect(list.closest("[data-window-tools]")).not.toBeNull();
    expect(list.closest("[data-window-titlebar]")).not.toBeNull();
    expect(list.closest("[data-window-content]")).toBeNull();
    expect(list.getAttribute("aria-orientation")).toBe("horizontal");
    expect(list.getAttribute("data-compact")).toBeNull();
    // Sem rótulo repetido: o nome da seção fica só para o leitor de tela.
    expect(within(settings()).getByRole("heading", { name: "Conexões" }).classList.contains("nd-sr-only")).toBe(true);
    await act(async () => observers.resize(380));
    expect(settings().getAttribute("data-size")).toBe("narrow");
    expect(list.getAttribute("data-compact")).toBe("true");
    expect(tab("Agentes").getAttribute("title")).toBe("Agentes");
    expect(tab("Agentes").querySelector("svg")).not.toBeNull();
    expect(tab("Agentes").querySelector(".nd-sr-only")?.textContent).toBe("Agentes");
  });

  it("abas que não cabem na barra viram só ícone; voltam ao nome quando a barra cresce além do ponto em que transbordaram", async () => {
    const callbacks: (() => void)[] = [];
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(cb: () => void) {
          callbacks.push(cb);
        }
        observe(): void {}
        disconnect(): void {}
      },
    );
    await openSettings();
    const list = within(settings()).getByRole("tablist");
    const bar = list.closest("[data-window-tools]")?.parentElement;
    if (bar === null || bar === undefined) throw new Error("barra ausente");
    let barWidth = 700;
    let needed = 600;
    bar.getBoundingClientRect = () => ({ width: barWidth }) as DOMRect;
    Object.defineProperty(list, "clientWidth", { configurable: true, get: () => 400 });
    Object.defineProperty(list, "scrollWidth", { configurable: true, get: () => needed });
    const resize = async (): Promise<void> => {
      await act(async () => {
        for (const cb of callbacks) cb();
      });
    };
    expect(list.getAttribute("data-compact")).toBeNull();
    await resize();
    expect(list.getAttribute("data-compact")).toBe("true");
    // Cresceu além do ponto, mas o nome ainda não cabe: tenta e volta ao ícone (sem vaivém).
    barWidth = 750;
    await resize();
    expect(list.getAttribute("data-compact")).toBe("true");
    // Agora cabe.
    barWidth = 820;
    needed = 400;
    await resize();
    expect(list.getAttribute("data-compact")).toBeNull();
    expect(tab("Agentes").textContent).toBe("Agentes");
  });

  it("boas-vindas (sem espaço): abre pelo menu do N e troca de aba só na janela", async () => {
    const { server } = await renderShell({ connections: [CONN] });
    await act(async () => {
      activateApp("Configurações");
    });
    await settle();
    await chooseTab("Aparência");
    expect(tab("Aparência").getAttribute("aria-selected")).toBe("true");
    expect(server.calls.some((c) => c.method === "PUT" && c.url.startsWith("/api/projects/"))).toBe(false);
  });
});

/** Conteúdo de teste que pede uma aba das Configurações, como o "Configurar" da prontidão. */
function Opener() {
  const shell = useShell();
  const go = (t: SettingsWindowTab) => () => shell.openSettings(t);
  return (
    <div>
      <button type="button" onClick={go("machine")}>
        Configurar máquina
      </button>
      <button type="button" onClick={go("pricing")}>
        Configurar preços
      </button>
    </div>
  );
}

describe("Configurações: abertura direta", () => {
  it("openSettings(aba) abre a janela na aba pedida e consome o pedido; um novo pedido vale de novo", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      connections: [CONN],
      apps: { ...DEFAULT_APPS, conversation: { Content: Opener, scroll: "app" } },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Configurar máquina" }));
    });
    await settle();
    expect(tab("Máquina").getAttribute("aria-selected")).toBe("true");
    // O usuário troca de aba; o pedido já consumido não volta a valer.
    await chooseTab("Conexões");
    expect(tab("Conexões").getAttribute("aria-selected")).toBe("true");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Configurar preços" }));
    });
    await settle();
    expect(tab("Preços").getAttribute("aria-selected")).toBe("true");
  });

  it("o que falta na prontidão aparece na aba certa, com a contagem na lista de abas", async () => {
    await openSettings({ readiness: notReady() });
    expect(within(tab("Conexões")).getByText("1")).toBeTruthy();
    expect(within(settings()).getByRole("note").textContent).toContain("Conexão com um provedor");
    await chooseTab("Máquina");
    expect(within(settings()).getByRole("note").textContent).toContain("Docker daemon");
    await chooseTab("Aparência");
    expect(within(settings()).queryByRole("note")).toBeNull();
  });
});

describe("Configurações: carga da configuração", () => {
  it("sem configuração: 'Carregando as configurações.' (status); falha: erro fixo com 'Tentar de novo', que relê a configuração", async () => {
    const { server } = await openSettings();
    act(() => {
      appStore.setState({ config: null, configError: null });
    });
    expect(within(settings()).getByRole("status").textContent).toBe("Carregando as configurações.");
    act(() => {
      appStore.setState({ configError: { kind: "http", status: 500, message: "falha interna do servidor", messageKey: "api.serverError" } });
    });
    const alert = within(settings()).getByRole("alert");
    expect(alert.textContent).toContain("falha interna do servidor");
    const before = server.count("GET", /^\/api\/config$/);
    await act(async () => {
      fireEvent.click(within(alert).getByRole("button", { name: "Tentar de novo" }));
    });
    await settle();
    expect(server.count("GET", /^\/api\/config$/)).toBe(before + 1);
    expect(within(settings()).getByRole("list", { name: "Conexões salvas" })).toBeTruthy();
  });
});

describe("Configurações: Conexões", () => {
  it("sem conexões: estado vazio e o formulário de nova conexão já aberto", async () => {
    await openSettings({ connections: [] });
    expect(within(settings()).getByText("Nenhuma conexão cadastrada ainda.")).toBeTruthy();
    expect(within(settings()).queryByRole("list", { name: "Conexões salvas" })).toBeNull();
    expect(within(settings()).getByRole("region", { name: "Nova conexão" })).toBeTruthy();
  });

  it("lista: ponto, nome, detalhe (host, protocolo, só a máscara), 'usada por' e o menu '…' (Testar, Editar); 'Nova conexão' abaixo", async () => {
    const spare: ConnectionView = { id: "spare", name: "Reserva", base_url: "https://spare.example.com/v1", api_key_masked: "****0000" };
    await openSettings({ connections: [CONN, spare] });
    const list = within(settings()).getByRole("list", { name: "Conexões salvas" });
    const rows = within(list).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("data-connection"))).toEqual(["conn", "spare"]);
    const [first, second] = rows as [HTMLElement, HTMLElement];
    expect(first.querySelector(".nd-dot")).not.toBeNull();
    expect(within(first).getByText("OpenAI direto")).toBeTruthy();
    const detail = first.querySelector("[data-conn-detail]");
    expect(detail?.textContent).toBe("api.openai.com · Chat Completions · chave ********abcd");
    expect(detail?.getAttribute("title")).toBe("https://api.openai.com/v1");
    expect(first.querySelector("[data-conn-usage]")?.textContent).toMatch(/^usada por /);
    expect(second.querySelector("[data-conn-usage]")?.textContent).toBe("sem uso");
    expect(second.getAttribute("data-conn-tone")).toBe("muted");
    // Sem caixa: a linha não é cartão.
    expect(first.classList.contains("nd-card")).toBe(false);
    await act(async () => {
      fireEvent.click(within(first).getByRole("button", { name: "Mais ações de OpenAI direto" }));
    });
    expect(within(screen.getByRole("menu", { name: "Ações da conexão OpenAI direto" })).getAllByRole("menuitem").map((i) => i.textContent)).toEqual(["Testar", "Editar"]);
    // "Nova conexão" vem depois da lista.
    const add = within(settings()).getByRole("button", { name: "Nova conexão" });
    expect(list.compareDocumentPosition(add) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(settings().textContent).not.toContain("sk-");
  });

  it("nova conexão: chave em password, enviada uma vez, fora do DOM depois do envio; prontidão relida", async () => {
    const { server } = await openSettings();
    server.reply("PUT", /^\/api\/config\/connections$/, (body) => {
      const b = body as { id: string; name: string; base_url: string };
      return { status: 200, body: { id: b.id, name: b.name, base_url: b.base_url, api_key_masked: "********3456", wire_api: "responses" } };
    });
    const win = settings();
    fireEvent.click(within(win).getByRole("button", { name: "Nova conexão" }));
    // Spec 116: o primeiro passo pergunta como conectar.
    fireEvent.click(within(win).getByRole("button", { name: /^OpenAI ou compatível/ }));
    const form = within(win).getByRole("region", { name: "Nova conexão" });
    const key = within(form).getByLabelText("Chave de API") as HTMLInputElement;
    expect(key.type).toBe("password");
    fireEvent.change(within(form).getByLabelText("Identificador"), { target: { value: "novo" } });
    fireEvent.change(within(form).getByLabelText("Nome"), { target: { value: "Novo" } });
    fireEvent.change(within(form).getByLabelText("URL base"), { target: { value: "https://api.example.com" } });
    fireEvent.click(within(form).getByRole("radio", { name: /Responses/ }));
    fireEvent.change(key, { target: { value: "sk-segredo-123456" } });
    // Antes do envio a chave só existe no campo password (a varredura acha onde ela está).
    expect(serializedDom()).toContain("sk-segredo");
    expect([...document.querySelectorAll("input")].filter((i) => i.value.includes("sk-segredo")).map((i) => i.type)).toEqual(["password"]);
    expect(settings().textContent).not.toContain("sk-segredo");
    const readinessBefore = server.count("GET", /^\/api\/readiness$/);
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar conexão" }));
    });
    await settle();
    const puts = server.calls.filter((c) => c.method === "PUT" && c.url === "/api/config/connections");
    expect(puts).toHaveLength(1);
    expect(puts[0]?.body).toMatchObject({ id: "novo", api_key: "sk-segredo-123456", wire_api: "responses" });
    expect(serializedDom()).not.toContain("sk-segredo");
    expect(within(win).queryByRole("region", { name: "Nova conexão" })).toBeNull();
    expect(appStore.getState().config?.connections.map((c) => c.id)).toEqual(["conn", "novo"]);
    expect(server.count("GET", /^\/api\/readiness$/)).toBe(readinessBefore + 1);
  });

  it("erro 422: mensagem fixa, formulário mantido e o campo da chave limpo", async () => {
    const { server } = await openSettings();
    server.reply("PUT", /^\/api\/config\/connections$/, () => ({ status: 422, body: { error: "api_key é obrigatória", code: "api_key_required" } }));
    const win = settings();
    fireEvent.click(within(win).getByRole("button", { name: "Nova conexão" }));
    // Spec 116: o primeiro passo pergunta como conectar.
    fireEvent.click(within(win).getByRole("button", { name: /^OpenAI ou compatível/ }));
    const form = within(win).getByRole("region", { name: "Nova conexão" });
    fireEvent.change(within(form).getByLabelText("Identificador"), { target: { value: "novo" } });
    fireEvent.change(within(form).getByLabelText("Nome"), { target: { value: "Novo" } });
    fireEvent.change(within(form).getByLabelText("URL base"), { target: { value: "https://api.example.com" } });
    fireEvent.change(within(form).getByLabelText("Chave de API"), { target: { value: "sk-abc-999" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar conexão" }));
    });
    await settle();
    expect(within(form).getByRole("alert").textContent).toBe("Dados inválidos. Revise os campos e tente de novo.");
    expect((within(form).getByLabelText("Chave de API") as HTMLInputElement).value).toBe("");
    expect(serializedDom()).not.toContain("sk-abc-999");
  });

  it("protocolo: sem wire_api abre e salva Chat explícito; conexão responses abre em Responses e mantém ao salvar só o nome", async () => {
    const resp: ConnectionView = { id: "resp", name: "Azure", base_url: "https://az.example.com/v1", api_key_masked: "********9999", wire_api: "responses" };
    const { server } = await openSettings({ connections: [CONN, resp] });
    server.reply("PUT", /^\/api\/config\/connections$/, (body) => {
      const b = body as ConnectionView;
      return { status: 200, body: { ...b, api_key_masked: "********0000" } };
    });
    const win = settings();
    expect(win.querySelector("[data-connection='conn'] [data-conn-detail]")?.textContent).toContain("Chat Completions");
    expect(win.querySelector("[data-connection='resp'] [data-conn-detail]")?.textContent).toContain("Responses");

    const save = async (form: HTMLElement): Promise<void> => {
      await act(async () => {
        fireEvent.click(within(form).getByRole("button", { name: "Salvar conexão" }));
      });
      await settle();
    };
    // 095/T06: "Editar" fica no menu "…" da linha, com ícone (nunca só texto).
    await connectionAction("OpenAI direto", "Editar");
    let form = within(win).getByRole("region", { name: "Editar conexão OpenAI direto" });
    expect((within(form).getByRole("radio", { name: /Chat Completions/ }) as HTMLInputElement).checked).toBe(true);
    await save(form);

    await connectionAction("Azure", "Editar");
    form = within(win).getByRole("region", { name: "Editar conexão Azure" });
    expect((within(form).getByRole("radio", { name: /Responses/ }) as HTMLInputElement).checked).toBe(true);
    fireEvent.change(within(form).getByLabelText("Nome"), { target: { value: "Azure 2" } });
    await save(form);

    const puts = server.calls.filter((c) => c.method === "PUT" && c.url === "/api/config/connections").map((c) => c.body);
    expect(puts).toHaveLength(2);
    expect(puts[0]).toMatchObject({ id: "conn", wire_api: "chat" });
    expect(puts[1]).toMatchObject({ id: "resp", name: "Azure 2", wire_api: "responses" });
  });

  it("editar: o campo da chave começa vazio (a chave salva nunca aparece) e o id fica fixo", async () => {
    await openSettings();
    const win = settings();
    await connectionAction("OpenAI direto", "Editar");
    const form = within(win).getByRole("region", { name: "Editar conexão OpenAI direto" });
    expect((within(form).getByLabelText("Chave de API") as HTMLInputElement).value).toBe("");
    expect((within(form).getByLabelText("Identificador") as HTMLInputElement).disabled).toBe(true);
    expect((within(form).getByLabelText("Nome") as HTMLInputElement).value).toBe("OpenAI direto");
    fireEvent.click(within(form).getByRole("button", { name: "Cancelar" }));
    expect(within(win).queryByRole("region", { name: /Editar conexão/ })).toBeNull();
  });

  it("Testar usa o id salvo (sem chave nem modelo)", async () => {
    const { server } = await openSettings();
    server.reply("POST", /^\/api\/config\/connections\/test$/, () => ({ status: 200, body: { ok: true, latency_ms: 42, models_count: 3 } }));
    const card = settings().querySelector<HTMLElement>("[data-connection='conn']");
    if (card === null) throw new Error("cartão ausente");
    await connectionAction("OpenAI direto", "Testar");
    await settle();
    expect(server.calls.find((c) => c.url === "/api/config/connections/test")?.body).toEqual({ id: "conn" });
    expect(within(card).getByRole("status").textContent).toContain("Conexão ok");
  });
});

describe("Configurações: Agentes, Preços, Máquina, Aparência e Projetos", () => {
  it("Agentes: três papéis com conexão, modelo, esforço e limite de passos; salvar relê a prontidão", async () => {
    const { server } = await openSettings();
    server.reply("PUT", /^\/api\/config\/agents$/, (body) => ({ status: 200, body }));
    await chooseTab("Agentes");
    const win = settings();
    expect(within(win).getByRole("region", { name: "Arquiteto" })).toBeTruthy();
    expect(within(win).getByLabelText(/Limite de passos.*Desenvolvedor/)).toBeTruthy();
    expect(within(win).getByRole("radiogroup", { name: "Esforço do Revisor" })).toBeTruthy();
    const before = server.count("GET", /^\/api\/readiness$/);
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Salvar agentes" }));
    });
    await settle();
    expect(server.calls.some((c) => c.method === "PUT" && c.url === "/api/config/agents")).toBe(true);
    expect(server.count("GET", /^\/api\/readiness$/)).toBe(before + 1);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("Agentes sem conexão: orienta e leva à aba Conexões", async () => {
    await openSettings({ connections: [] });
    await chooseTab("Agentes");
    fireEvent.click(within(settings()).getByRole("button", { name: "Ir para Conexões" }));
    expect(tab("Conexões").getAttribute("aria-selected")).toBe("true");
  });

  it("Preços: cartão por modelo com US$ por imagem; salvar envia image_per_unit; valor inválido aponta o modelo", async () => {
    const { server } = await openSettings();
    server.reply("PUT", /^\/api\/config\/pricing$/, (body) => ({ status: 200, body }));
    await chooseTab("Preços");
    const win = settings();
    const card = win.querySelector<HTMLElement>("[data-model='model']");
    if (card === null) throw new Error("cartão do modelo ausente");
    expect(within(card).getByText("sem preço")).toBeTruthy();
    fireEvent.change(within(card).getByLabelText("Entrada de model"), { target: { value: "1,25" } });
    fireEvent.change(within(card).getByLabelText("Saída de model"), { target: { value: "10" } });
    fireEvent.change(within(card).getByLabelText("US$ por imagem de model"), { target: { value: "0,04" } });
    expect(within(card).getByText("manual")).toBeTruthy();
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Salvar preços" }));
    });
    await settle();
    const put = server.calls.find((c) => c.method === "PUT" && c.url === "/api/config/pricing");
    expect(put?.body).toEqual({ model: { input_per_mtok: 1.25, output_per_mtok: 10, image_per_unit: 0.04, source: "manual" } });
    expect(within(win).getByText("Preços salvos.")).toBeTruthy();
    fireEvent.change(within(card).getByLabelText("Saída de model"), { target: { value: "abc" } });
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Salvar preços" }));
    });
    expect(within(win).getByText(/Preço inválido em model/)).toBeTruthy();
    // O anel de foco do campo com unidade fica no contêiner (um só anel).
    expect(card.querySelector(".nd-affix > input.nd-input")).toBeNull();
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("Máquina: lista com versão, detalhe e comando para copiar; 'Verificar de novo' confere e relê", async () => {
    const { server } = await openSettings();
    await chooseTab("Máquina");
    const win = settings();
    expect(within(win).getByText("git version 2.43.0")).toBeTruthy();
    // O comando de correção vem do dicionário pelo id estável do item (spec 102 T05).
    expect(within(win).getByText(ptBR["prereq.fix.docker_permission"])).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Copiar o comando para Permissão no socket do Docker" })).toBeTruthy();
    const prereqsBefore = server.count("GET", /^\/api\/system\/prereqs$/);
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Verificar de novo" }));
    });
    await settle();
    expect(server.count("POST", /^\/api\/readiness\/refresh$/)).toBe(1);
    expect(server.count("GET", /^\/api\/system\/prereqs$/)).toBe(prereqsBefore + 1);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("Aparência: as mesmas escolhas do Personalizar; trocar o tema muda o desktop e grava appearance", async () => {
    const { server } = await openSettings();
    await chooseTab("Aparência");
    const win = settings();
    expect(within(win).getAllByRole("button").filter((b) => b.hasAttribute("data-theme-choice"))).toHaveLength(3);
    expect(within(win).getAllByRole("button").filter((b) => b.hasAttribute("data-wallpaper-choice"))).toHaveLength(4);
    await act(async () => {
      fireEvent.click(win.querySelector("[data-theme-choice='light']") as HTMLElement);
    });
    await settle();
    expect(document.querySelector(".nd-root")?.getAttribute("data-theme")).toBe("light");
    expect(server.model.ui.global?.data.appearance).toMatchObject({ theme: "light" });
    expect((win.querySelector("[data-theme-choice='light']") as HTMLElement).getAttribute("aria-pressed")).toBe("true");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("Projetos: mostra a raiz em uso; salvar envia a raiz; 422 mostra a mensagem fixa; 'Usar o padrão' envia null", async () => {
    const { server } = await openSettings();
    act(() => {
      appStore.setState((s) => (s.config === null ? s : { config: { ...s.config, ...{ projects_root: "~/Projetos", projects_root_effective: "/home/dev/Projetos" } } }));
    });
    let reply = { status: 422, body: { error: "pasta de projetos inválida: diretório de sistema não é permitido", code: "projects_root_system" } as unknown };
    server.reply("PUT", /^\/api\/config\/projects-root$/, () => reply);
    await chooseTab("Projetos");
    const win = settings();
    expect(win.querySelector("[data-root-effective]")?.textContent).toBe("/home/dev/Projetos");
    const field = within(win).getByLabelText("Pasta de projetos") as HTMLInputElement;
    expect(field.value).toBe("~/Projetos");
    fireEvent.change(field, { target: { value: "/etc" } });
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Salvar pasta" }));
    });
    await settle();
    expect(within(win).getByRole("alert").textContent).toBe("Pasta de projetos inválida: diretório de sistema não é permitido.");
    reply = { status: 200, body: { projects_root: null, projects_root_effective: "/home/dev/NantsProjects" } };
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Usar o padrão" }));
    });
    await settle();
    const puts = server.calls.filter((c) => c.method === "PUT" && c.url === "/api/config/projects-root").map((c) => c.body);
    expect(puts).toEqual([{ projects_root: "/etc" }, { projects_root: null }]);
    expect(win.querySelector("[data-root-effective]")?.textContent).toBe("/home/dev/NantsProjects");
    expect(within(win).getByRole("status").textContent).toContain("de volta ao padrão");
    await waitFor(() => expect((within(win).getByLabelText("Pasta de projetos") as HTMLInputElement).value).toBe(""));
  });
});
