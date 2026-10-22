/*
 * Configurações nos dois idiomas (spec 102, T05): em en, nenhum texto visível ou acessível das seis
 * abas fica em português (formulários, menus, mensagens, pendências da prontidão e o diagnóstico da
 * máquina pelo `code` neutro do servidor); o que a pessoa gravou (nome da conexão, modelo, caminho)
 * aparece como veio; pt-BR igual a hoje; a troca de idioma vale na hora, sem recarregar.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionView, PrereqItem } from "../../api/types";
import type { Readiness } from "../../desktop/api";
import { renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { appStore } from "../../store";
import { makeProject, makeProjectState } from "../../test-utils";
import { reachEnvironment } from "../app/runSession";

const original = { ...reachEnvironment };

afterEach(() => {
  cleanup();
  Object.assign(reachEnvironment, original);
  setLanguage("pt-BR", { persist: false });
  vi.unstubAllGlobals();
});

type Lang = "pt-BR" | "en";
const SETTINGS: Readonly<Record<Lang, string>> = { "pt-BR": "Configurações", en: "Settings" };

const P1 = makeProject("p1", "Loja");
const CONN: ConnectionView = { id: "conn", name: "Minha conexao", base_url: "https://api.openai.com/v1", api_key_masked: "********abcd" };
const GIB = 1024 * 1024 * 1024;

/** Diagnóstico como o servidor da T05 manda: `detail` pt-BR de reserva e o detalhe neutro. */
const PREREQS: PrereqItem[] = [
  { id: "git", label: "Git", ok: true, version: "git version 2.43.0" },
  {
    id: "docker_permission",
    label: "Permissão no socket do Docker",
    ok: false,
    detail: "permission denied ao acessar o socket",
    detail_code: "permission_denied",
    detail_params: {},
    fix: "Adicione seu usuário ao grupo docker: sudo usermod -aG docker $USER e reabra a sessão (feche e abra o terminal; no WSL, rode wsl --shutdown no Windows).",
  },
  {
    id: "disk",
    label: "Espaço em disco",
    ok: false,
    detail: "1,5 GB livres em /home/dev/NantsProjects",
    detail_code: "disk",
    detail_params: {},
    detail_parts: [{ code: "disk_free", params: { size_bytes: GIB * 1.5, path: "/home/dev/NantsProjects" } }],
    fix: "Libere espaço em disco: o Nants precisa de pelo menos 5 GB livres para as imagens Docker e os projetos.",
  },
];

/** Prontidão com `code`/`params` neutros (servidor da T05). */
function readiness(): Readiness {
  const key = { id: "connection_key:conn", label: "Chave da conexão Minha conexao", code: "connection_key", params: { name: "Minha conexao" }, settings_tab: "connections" as const };
  const perm = {
    id: "docker_permission",
    label: "Permissão no socket do Docker",
    code: "docker_permission",
    fix: "Adicione seu usuário ao grupo docker: sudo usermod -aG docker $USER e reabra a sessão.",
    settings_tab: "machine" as const,
  };
  const price = { id: "pricing:model", label: "Preço do modelo model", code: "pricing", params: { model: "model" }, settings_tab: "pricing" as const };
  return {
    chat: { ok: false, missing: [key] },
    create_project: { ok: false, missing: [key] },
    execute: { ok: false, missing: [perm] },
    recommended: { ok: false, missing: [price] },
  };
}

async function open(lang: Lang, options: RenderShellOptions = {}) {
  setLanguage(lang, { persist: false });
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    connections: [CONN],
    ...options,
  });
  view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: PREREQS }));
  view.server.reply("GET", /^\/api\/config\/connections\/[^/]+\/models/, () => ({ status: 200, body: { models: [{ id: "model" }], truncated: false } }));
  view.server.reply("POST", /^\/api\/config\/pricing\/autofill/, () => ({ status: 200, body: {} }));
  view.server.reply("POST", /^\/api\/config\/pricing\/estimate/, () => ({ status: 200, body: {} }));
  view.server.reply("GET", /^\/api\/config\/pricing\/estimate/, () => ({ status: 200, body: {} }));
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: SETTINGS[lang] }));
  });
  await settle();
  return view;
}

const win = (lang: Lang = "en"): HTMLElement => screen.getByRole("region", { name: SETTINGS[lang] });

async function chooseTab(name: RegExp): Promise<void> {
  await act(async () => {
    fireEvent.click(within(win()).getByRole("tab", { name }));
  });
  await settle();
}

/**
 * Sem português na janela e no que flutua (menus, diálogos). O nome do idioma no seletor é sempre
 * escrito no próprio idioma (spec 101, decisão 3), e o nome da conexão é conteúdo da pessoa.
 */
function expectNoPortuguese(): void {
  const own = (text: string): string => text.split(CONN.name).join(" ");
  expect(portugueseLeftovers(win()).filter((text) => text !== "Português (Brasil)" && portugueseLeftoversOf(own(text)))).toEqual([]);
  for (const el of [...screen.queryAllByRole("menu"), ...screen.queryAllByRole("dialog")]) {
    expect(portugueseLeftovers(el).filter((text) => portugueseLeftoversOf(own(text)))).toEqual([]);
  }
}

/** O texto, sem o conteúdo da pessoa, ainda tem português. */
function portugueseLeftoversOf(text: string): boolean {
  const probe = document.createElement("div");
  probe.textContent = text;
  return portugueseLeftovers(probe).length > 0;
}

describe("Configurações em en", () => {
  it("abas, Conexões (linha, menu, teste, formulário e validação) e a pendência pelo code com o nome da conexão", async () => {
    const view = await open("en", { readiness: readiness() });
    view.server.reply("POST", /^\/api\/config\/connections\/test$/, () => ({ status: 200, body: { ok: true, latency_ms: 42, models_count: 3 } }));
    const list = within(win()).getByRole("tablist", { name: "Settings sections" });
    expect(within(list).getAllByRole("tab").map((t) => t.querySelector(".nd-settings-tab-label")?.textContent)).toEqual([
      "Connections",
      "Agents",
      "Pricing",
      "Machine",
      "Appearance",
      "Projects",
    ]);
    expect(within(list).getByRole("tab", { name: /^Connections/ }).textContent).toContain("1 pending item");
    expect(win().querySelector("[data-tab-missing]")?.textContent).toBe("Still to set up: Key of connection Minha conexao.");
    const row = win().querySelector("[data-connection='conn']") as HTMLElement;
    expect(row.querySelector("[data-conn-detail]")?.textContent).toBe("api.openai.com · Chat Completions · key ********abcd");
    expect(row.querySelector("[data-conn-usage]")?.textContent).toBe("used by Architect, Developer, Reviewer");
    expect(within(row).getByText("in use")).toBeTruthy();
    expect(within(win()).getByRole("list", { name: "Saved connections" })).toBeTruthy();
    expectNoPortuguese();

    await act(async () => {
      fireEvent.click(within(row).getByRole("button", { name: "More actions for Minha conexao" }));
    });
    const menu = screen.getByRole("menu", { name: "Actions for connection Minha conexao" });
    expect(within(menu).getAllByRole("menuitem").map((m) => m.textContent)).toEqual(["Test", "Edit"]);
    expectNoPortuguese();
    await act(async () => {
      fireEvent.click(within(menu).getByRole("menuitem", { name: "Test" }));
    });
    await settle();
    expect(row.querySelector("[data-ok='true']")?.textContent).toBe("Connection ok · 3 models (42 ms).");
    expectNoPortuguese();

    await act(async () => {
      fireEvent.click(within(win()).getByRole("button", { name: "New connection" }));
    });
    const form = within(win()).getByRole("region", { name: "New connection" });
    for (const label of ["Identifier", "Name", "Base URL", "API key"]) expect(within(form).getByLabelText(label)).toBeTruthy();
    expect(within(form).getByText("Protocol")).toBeTruthy();
    expect(within(form).getByText("Responses (reasoning)")).toBeTruthy();
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Save connection" }));
    });
    expect(within(form).getByText("Enter an identifier.")).toBeTruthy();
    expect(within(form).getByText("Enter the base URL.")).toBeTruthy();
    expect(within(form).getByText("Enter the API key or an env:NAME reference.")).toBeTruthy();
    expect(within(form).getByRole("button", { name: "Cancel" })).toBeTruthy();
    expectNoPortuguese();
  });

  it("Agentes e Preços: rótulos, dicas, selo e mensagens", async () => {
    const view = await open("en", { readiness: readiness() });
    await chooseTab(/^Agents/);
    expect(within(win()).getByRole("heading", { name: "Routing per agent" })).toBeTruthy();
    expect(within(win()).getByLabelText("Use the same model for all")).toBeTruthy();
    const architect = within(win()).getByRole("region", { name: "Architect" });
    expect(within(architect).getByRole("radiogroup", { name: "Effort for Architect" })).toBeTruthy();
    expect(within(architect).getByLabelText("Connection for Architect")).toBeTruthy();
    const developer = within(win()).getByRole("region", { name: "Developer" });
    expect(developer.textContent).toContain("Safety net; the budget controls the cost. Empty uses the default (150); from 10 to 1000.");
    expect(within(win()).getByRole("button", { name: "Save agents" })).toBeTruthy();
    expectNoPortuguese();

    view.server.reply("PUT", /^\/api\/config\/pricing$/, () => ({ status: 200, body: { model: { input_per_mtok: 1.5, output_per_mtok: 10, source: "manual" } } }));
    await chooseTab(/^Pricing/);
    expect(win().querySelector("[data-tab-missing]")?.textContent).toBe("Still to set up: Price of model model.");
    expect(within(win()).getByRole("heading", { name: "Prices per model" })).toBeTruthy();
    const card = win().querySelector("[data-model='model']") as HTMLElement;
    expect(within(card).getByText("no price")).toBeTruthy();
    for (const label of ["Input for model", "Output for model", "Cache for model", "US$ per image for model"]) expect(within(card).getByLabelText(label)).toBeTruthy();
    expect(within(card).getByText("US$ per image (optional)")).toBeTruthy();
    fireEvent.change(within(card).getByLabelText("Input for model"), { target: { value: "1.5" } });
    fireEvent.change(within(card).getByLabelText("Output for model"), { target: { value: "10" } });
    expect(within(card).getByText("manual")).toBeTruthy();
    await act(async () => {
      fireEvent.click(within(win()).getByRole("button", { name: "Save prices" }));
    });
    await settle();
    expect(within(win()).getByText("Prices saved.")).toBeTruthy();
    expectNoPortuguese();
  });

  it("Máquina: diagnóstico pelo code neutro (detalhe, partes em GB, correção), pendência e Acesso pela rede", async () => {
    reachEnvironment.pageHostname = () => "localhost";
    const view = await open("en", { readiness: readiness() });
    view.server.reply("GET", /^\/api\/config\/network$/, () => ({
      status: 200,
      body: {
        env: "wsl_nat",
        saved_lan: false,
        lan: false,
        mode_source: "saved",
        port: 3001,
        lan_port: 3000,
        view_ports: { start: 3001, end: 3010 },
        windows_redirect: "yes",
        windows_redirect_target: "this_wsl",
        coherence: "still_redirected",
        firewall_rule: "yes",
        restart_required: false,
        restart_command: "nants",
      },
    }));
    act(() => {
      appStore.setState((s) => (s.config === null ? s : { config: { ...s.config, network: { nants_port: 3000, lan_view_ports: { start: 3001, end: 3010 }, wsl: true } } }));
    });
    await chooseTab(/^Machine/);
    const w = win();
    expect(w.querySelector("[data-tab-missing]")?.textContent).toBe("Still to set up: Docker permission.");
    expect(within(w).getByRole("list", { name: "Machine prerequisites" })).toBeTruthy();
    const perm = w.querySelector("[data-prereq='docker_permission']") as HTMLElement;
    expect(perm.textContent).toContain("Docker permission: has a problem");
    expect(perm.textContent).toContain("permission denied on the socket");
    expect(perm.querySelector("code")?.textContent).toMatch(/^Add your user to the docker group: sudo usermod -aG docker \$USER/);
    expect(within(perm).getByRole("button", { name: "Copy the command for Docker permission" })).toBeTruthy();
    const disk = w.querySelector("[data-prereq='disk']") as HTMLElement;
    expect(disk.textContent).toContain("Disk space");
    expect(disk.textContent).toContain("1.5 GB free in /home/dev/NantsProjects");
    expect(w.querySelector("[data-prereq='git']")?.textContent).toContain("git version 2.43.0");
    expect(within(w).getByRole("button", { name: "Check again" })).toBeTruthy();
    const network = w.querySelector("[data-network-section]") as HTMLElement;
    expect(network.querySelector("[data-network-address]")?.textContent).toBe("http://<IP of this computer>:3000");
    // WSL com o Windows conferido: o estado real, sem a marcação manual.
    expect(network.querySelector("[data-network-state]")?.textContent).toBe("Allowed on Windows");
    expect(within(network).queryByRole("button", { name: "Mark as allowed" })).toBeNull();
    expect(within(network).getByRole("switch", { name: "Local network access" }).getAttribute("aria-checked")).toBe("false");
    expect(network.querySelector("[data-network-coherence]")?.textContent).toBe(
      "Windows still redirects the ports: run the undo script. That is why Nants may be on a port other than 3000.",
    );
    expect([...network.querySelectorAll("summary")].map((el) => el.textContent)).toEqual(["Script to undo it on Windows"]);
    expect(within(network).getByLabelText("PowerShell script: Script to undo it on Windows")).toBeTruthy();
    expect(w.querySelector("[data-nants-version]")?.textContent).toBe("Nants version 0.1.0");
    expectNoPortuguese();
  });

  it("Aparência e Projetos (a pasta como veio; o 422 traduzido); subtítulo da janela", async () => {
    const view = await open("en", { readiness: readiness() });
    expect(document.getElementById(windowSubtitleId("settings"))?.textContent).toBe("2 setup items pending");
    await chooseTab(/^Appearance/);
    expect(within(win()).getByRole("group", { name: "Idioma / Language" })).toBeTruthy();
    expectNoPortuguese();

    view.server.reply("PUT", /^\/api\/config\/projects-root$/, () => ({ status: 422, body: { error: "pasta de projetos inválida: diretório de sistema não é permitido" } }));
    await chooseTab(/^Projects/);
    const form = within(win()).getByRole("form", { name: "Projects root" });
    expect(form.textContent).toContain("A new project asks only for the name");
    expect(within(form).getByLabelText("Projects folder")).toBeTruthy();
    fireEvent.change(within(form).getByLabelText("Projects folder"), { target: { value: "/etc" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Save folder" }));
    });
    await settle();
    expect(within(form).getByRole("alert").textContent).toBe("Invalid projects folder: system directories are not allowed.");
    expect(within(form).getByRole("button", { name: "Use the default" })).toBeTruthy();
    expectNoPortuguese();
  });
});

describe("Configurações em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora (abas, pendência pelo code, linha da conexão, diagnóstico e subtítulo)", async () => {
    await open("pt-BR", { readiness: readiness() });
    const tabs = (): (string | null | undefined)[] =>
      within(screen.getByRole("region", { name: /^(Configurações|Settings)$/ }))
        .getAllByRole("tab")
        .map((t) => t.querySelector(".nd-settings-tab-label")?.textContent);
    expect(tabs()).toEqual(["Conexões", "Agentes", "Preços", "Máquina", "Aparência", "Projetos"]);
    expect(win("pt-BR").querySelector("[data-tab-missing]")?.textContent).toBe("Falta configurar: Chave da conexão Minha conexao.");
    expect(win("pt-BR").querySelector("[data-conn-detail]")?.textContent).toBe("api.openai.com · Chat Completions · chave ********abcd");
    expect(win("pt-BR").querySelector("[data-conn-usage]")?.textContent).toBe("usada por Arquiteto, Desenvolvedor, Revisor");
    expect(document.getElementById(windowSubtitleId("settings"))?.textContent).toBe("2 pendências no setup");
    await act(async () => {
      fireEvent.click(within(win("pt-BR")).getByRole("tab", { name: /^Máquina/ }));
    });
    await settle();
    const perm = win("pt-BR").querySelector("[data-prereq='docker_permission']") as HTMLElement;
    expect(perm.textContent).toContain("Permissão no socket do Docker: com problema");
    expect(perm.textContent).toContain("permission denied ao acessar o socket");
    expect(win("pt-BR").querySelector("[data-prereq='disk']")?.textContent).toContain("1,5 GB livres em /home/dev/NantsProjects");
    expect(win("pt-BR").querySelector("[data-tab-missing]")?.textContent).toBe("Falta configurar: Permissão no socket do Docker.");
    expect(portugueseLeftovers(win("pt-BR")).length).toBeGreaterThan(0);

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    expect(tabs()).toEqual(["Connections", "Agents", "Pricing", "Machine", "Appearance", "Projects"]);
    expect(win().querySelector("[data-prereq='disk']")?.textContent).toContain("1.5 GB free in /home/dev/NantsProjects");
    expect(win().querySelector("[data-tab-missing]")?.textContent).toBe("Still to set up: Docker permission.");
    expect(document.getElementById(windowSubtitleId("settings"))?.textContent).toBe("2 setup items pending");
    expectNoPortuguese();
  });
});
