/*
 * Primeiros passos nos dois idiomas (spec 102, T05): em en, nenhum texto visível ou acessível das
 * quatro etapas fica em português, inclusive as pendências e o diagnóstico completo da 103/T02
 * montados pelo `code` neutro do servidor; o que a máquina devolve (versões, caminhos, tags) aparece
 * como veio; pt-BR igual a hoje; a troca de idioma vale na hora.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PrereqItem } from "../../api/types";
import type { Readiness } from "../../desktop/api";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { setupSubtitle } from "./setup";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.unstubAllGlobals();
});

type Lang = "pt-BR" | "en";
const SETUP: Readonly<Record<Lang, string>> = { "pt-BR": "Primeiros passos", en: "Getting started" };

/** Prontidão do servidor da T05: `label` pt-BR de reserva, `code` e `params` neutros. */
function readiness(): Readiness {
  const connection = { id: "connection", label: "Conexão com um provedor de modelos", code: "connection", settings_tab: "connections" as const };
  const architect = { id: "agent_architect", label: "Arquiteto: conexão e modelo", code: "agent_architect", settings_tab: "agents" as const };
  const daemon = {
    id: "docker_daemon",
    label: "Docker daemon",
    code: "docker_daemon",
    fix: "Inicie o Docker: sudo systemctl start docker (Linux) ou abra o Docker Desktop (Windows/WSL) e ligue a integração com a sua distribuição WSL.",
    settings_tab: "machine" as const,
  };
  return {
    chat: { ok: false, missing: [connection, architect] },
    create_project: { ok: false, missing: [connection, architect] },
    execute: { ok: false, missing: [connection, architect, daemon] },
    recommended: { ok: true, missing: [] },
  };
}

const DIAGNOSIS: PrereqItem[] = [
  { id: "git", label: "Git", ok: true, version: "git version 2.43.0" },
  { id: "docker_daemon", label: "Docker daemon", ok: false, detail: "daemon inacessível", detail_code: "daemon_unreachable", detail_params: {}, fix: "Inicie o Docker." },
  {
    id: "docker_compose",
    label: "Docker Compose v2",
    ok: false,
    detail: "plugin compose ausente",
    detail_code: "compose_missing",
    detail_params: {},
    fix: "Instale o plugin Compose v2: sudo apt install docker-compose-plugin (ou atualize o Docker Desktop).",
  },
  {
    id: "wsl",
    label: "WSL",
    ok: true,
    info: true,
    detail: "detectado: o Docker pode ser o Docker Desktop com a integração WSL ligada ou o Docker Engine dentro da distribuição",
    detail_code: "wsl_detected",
    detail_params: {},
  } as PrereqItem,
  {
    id: "playwright_images",
    label: "Imagens do Playwright",
    ok: true,
    info: true,
    detail: "já baixadas: v1.48.0-jammy. Projetos com outra versão baixam cerca de 2 GB no primeiro teste de navegador",
    detail_code: "playwright_some",
    detail_params: { tags: "v1.48.0-jammy" },
  } as PrereqItem,
  // Código desconhecido (servidor mais novo): o texto do servidor fica de reserva.
  { id: "futuro", label: "Item futuro", ok: true, detail: "detalhe do servidor", detail_code: "nao_existe", detail_params: {} },
];

async function open(lang: Lang, diagnosis: PrereqItem[] = DIAGNOSIS) {
  setLanguage(lang, { persist: false });
  const view = await renderShell({ projects: [], readiness: readiness() });
  view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: diagnosis }));
  // Relê o diagnóstico com a resposta acima (a janela abriu sozinha antes do `reply`).
  await act(async () => {
    fireEvent.click(within(win(lang)).getByRole("button", { name: lang === "en" ? /^Connection/ : /^Conexão/ }));
  });
  await act(async () => {
    fireEvent.click(within(win(lang)).getByRole("button", { name: lang === "en" ? /^Your machine/ : /^Sua máquina/ }));
  });
  await settle();
  return view;
}

const win = (lang: Lang = "en"): HTMLElement => screen.getByRole("region", { name: SETUP[lang] });

function expectNoPortuguese(): void {
  // "Item futuro" e "detalhe do servidor" vêm do servidor sem tradução (código desconhecido).
  const allowed = new Set(["Item futuro", "detalhe do servidor"]);
  expect(portugueseLeftovers(win()).filter((text) => !allowed.has(text))).toEqual([]);
  for (const el of screen.queryAllByRole("dialog")) expect(portugueseLeftovers(el)).toEqual([]);
}

async function goTo(step: RegExp): Promise<void> {
  await act(async () => {
    fireEvent.click(within(win()).getByRole("button", { name: step }));
  });
  await settle();
}

describe("Primeiros passos em en", () => {
  it("Sua máquina: pendências e diagnóstico completo pelo code, com OK, falha e informativo", async () => {
    await open("en");
    const w = win();
    expect(within(w).getByRole("navigation", { name: "Steps" })).toBeTruthy();
    expect(within(w).getByRole("heading", { name: "1. Your machine" })).toBeTruthy();
    expect(w.textContent).toContain("Nants uses Git for versioning and Docker to run and test the projects.");
    const missing = within(w).getByRole("list", { name: "What the machine is missing" });
    const daemon = missing.querySelector("[data-missing='docker_daemon']") as HTMLElement;
    expect(daemon.querySelector(".nd-strong")?.textContent).toBe("Docker daemon");
    expect(daemon.querySelector("code")?.textContent).toMatch(/^Start Docker: sudo systemctl start docker \(Linux\)/);
    expect(within(daemon).getByRole("button", { name: "Copy the command for Docker daemon" }).textContent).toBe("Copy");
    expect(within(w).getByRole("heading", { name: "Full diagnosis" })).toBeTruthy();
    expect(w.textContent).toContain("The same result as the nants doctor command in the terminal.");
    const list = within(w).getByRole("list", { name: "Machine diagnosis" });
    const row = (id: string): HTMLElement => list.querySelector(`[data-prereq='${id}']`) as HTMLElement;
    expect(row("git").textContent).toBe("Git: okgit version 2.43.0");
    expect(row("docker_daemon").textContent).toContain("Docker daemon: has a problem");
    expect(row("docker_daemon").textContent).toContain("daemon unreachable");
    expect(row("docker_compose").textContent).toContain("compose plugin missing");
    expect(row("docker_compose").querySelector("code")?.textContent).toBe(
      "Install the Compose v2 plugin: sudo apt install docker-compose-plugin (or update Docker Desktop).",
    );
    expect(row("wsl").textContent).toContain("WSL: informational");
    expect(row("wsl").textContent).toContain("detected: Docker may be Docker Desktop");
    expect(row("playwright_images").textContent).toContain("Playwright images: informational");
    expect(row("playwright_images").textContent).toContain("already downloaded: v1.48.0-jammy.");
    expect(row("futuro").textContent).toContain("detalhe do servidor");
    expect(within(w).getByRole("button", { name: "Check again" })).toBeTruthy();
    expect(within(w).getByRole("button", { name: /^Your machine, pending$/ })).toBeTruthy();
    expect(within(w).getByRole("button", { name: "Next step" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("setup"))?.textContent).toBe("4 steps pending");
    expectNoPortuguese();
  });

  it("Conexão, Agentes e Primeiro projeto (com o diálogo Novo projeto)", async () => {
    await open("en");
    await goTo(/^Connection/);
    expect(within(win()).getByRole("heading", { name: "2. Connection" })).toBeTruthy();
    expect(win().textContent).toContain("A connection, by API key (OpenAI or compatible, Anthropic or Google Gemini) or through your ChatGPT plan, is what lets you talk to the Architect.");
    // Spec 116: o primeiro passo pergunta como conectar (chave de API ou plano ChatGPT).
    expect(win().textContent).toContain("How to connect");
    expect(within(win()).getByRole("button", { name: "Continue with ChatGPT" })).toBeTruthy();
    expectNoPortuguese();
    fireEvent.click(within(win()).getByRole("button", { name: /^OpenAI or compatible/ }));
    expect(within(win()).getByLabelText("Identifier")).toBeTruthy();
    expectNoPortuguese();

    await goTo(/^Agents/);
    expect(win().textContent).toContain("Each agent uses a connection and a model.");
    expect(win().textContent).toContain("Add a connection in the previous step first.");
    expectNoPortuguese();

    await goTo(/^First project/);
    expect(win().textContent).toContain("A project is a folder with Git, created by Nants in your projects folder.");
    expect(win().textContent).toContain("Before creating: Connection to a model provider, Architect: connection and model.");
    expect(within(win()).getByRole("button", { name: "Previous" })).toBeTruthy();
    expectNoPortuguese();
    await act(async () => {
      fireEvent.click(within(win()).getByRole("button", { name: "Create the first project" }));
    });
    const dialog = screen.getByRole("dialog", { name: "New project" });
    expect(within(dialog).getByLabelText("Project name").getAttribute("placeholder")).toBe("e.g.: Corner shop");
    expectNoPortuguese();
  });

  it("Sua máquina: ambiente e Acesso pela rede local vindos do servidor, com o aviso em âmbar (111/T03)", async () => {
    const items: PrereqItem[] = [
      { id: "git", label: "Git", ok: true, version: "git version 2.43.0" },
      { id: "wsl", label: "WSL", ok: true, info: true, detail: "WSL no modo de rede NAT", detail_code: "env_wsl_nat", detail_params: {} },
      {
        id: "windows_port_redirect",
        label: "Acesso pela rede local",
        ok: true,
        warn: true,
        detail: "acesso pela rede local ligado, mas",
        detail_code: "lan_on_no_redirect",
        detail_params: { port: "3000" },
        fix: "Abra Configurações > Acesso pela rede",
      },
      { id: "playwright_images", label: "Imagens do Playwright", ok: true, info: true, detail: "nenhuma", detail_code: "playwright_none", detail_params: {} },
    ];
    await open("en", items);
    const list = within(win()).getByRole("list", { name: "Machine diagnosis" });
    const row = (id: string): HTMLElement => list.querySelector(`[data-prereq='${id}']`) as HTMLElement;
    expect(row("wsl").textContent).toContain("WSL in NAT networking mode (the default)");
    const lan = row("windows_port_redirect");
    expect(lan.getAttribute("data-status")).toBe("warn");
    expect(lan.textContent).toContain("Local network access: attention");
    expect(lan.textContent).toContain("local network access is on, but Windows does not redirect port 3000 to WSL");
    expect(lan.querySelector("code")?.textContent).toMatch(/^Open Settings > Network access/);
    expect(lan.querySelector("[data-status='warn']")).not.toBeNull();
    expectNoPortuguese();
    cleanup();

    await open("pt-BR", [
      { id: "wsl", label: "WSL", ok: true, info: true, detail: "x", detail_code: "env_wsl_mirrored", detail_params: {} },
      { id: "windows_port_redirect", label: "Acesso pela rede local", ok: true, detail: "x", detail_code: "lan_on_mirrored_ok", detail_params: { port: "3000" } },
    ]);
    const w = screen.getByRole("region", { name: "Primeiros passos" });
    const ptRow = (id: string): HTMLElement => w.querySelector(`[data-prereq='${id}']`) as HTMLElement;
    expect(ptRow("wsl").textContent).toContain("WSL no modo de rede espelhado");
    expect(ptRow("windows_port_redirect").getAttribute("data-status")).toBe("ok");
    expect(ptRow("windows_port_redirect").textContent).toContain("Acesso pela rede local");
    expect(ptRow("windows_port_redirect").textContent).toContain("regra de firewall Nants presente no Windows (modo espelhado, porta 3000)");
    expect(ptRow("windows_port_redirect").querySelector("code")).toBeNull();
  });

  it("subtítulo da janela: verificando, etapas pendentes e tudo pronto", () => {
    setLanguage("en", { persist: false });
    expect(setupSubtitle(null, 0)).toBe("Checking");
    expect(setupSubtitle(readiness(), 0)).toBe("4 steps pending");
    const ready: Readiness = {
      chat: { ok: true, missing: [] },
      create_project: { ok: true, missing: [] },
      execute: { ok: true, missing: [] },
      recommended: { ok: true, missing: [] },
    };
    expect(setupSubtitle(ready, 0)).toBe("1 step pending");
    expect(setupSubtitle(ready, 1)).toBe("All set");
  });
});

describe("Primeiros passos em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora (etapas, pendências, diagnóstico e subtítulo)", async () => {
    await open("pt-BR");
    const w = (): HTMLElement => screen.getByRole("region", { name: /^(Primeiros passos|Getting started)$/ });
    expect(within(w()).getByRole("heading", { name: "1. Sua máquina" })).toBeTruthy();
    const daemon = w().querySelector("[data-missing='docker_daemon']") as HTMLElement;
    expect(daemon.querySelector("code")?.textContent).toMatch(/^Inicie o Docker: sudo systemctl start docker/);
    const row = (id: string): HTMLElement => w().querySelector(`[data-prereq='${id}']`) as HTMLElement;
    expect(row("docker_daemon").textContent).toContain("Docker daemon: com problema");
    expect(row("docker_daemon").textContent).toContain("daemon inacessível");
    expect(row("wsl").textContent).toContain("WSL: informativo");
    expect(row("playwright_images").textContent).toContain("Imagens do Playwright");
    expect(document.getElementById(windowSubtitleId("setup"))?.textContent).toBe("4 etapas pendentes");

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    expect(within(w()).getByRole("heading", { name: "1. Your machine" })).toBeTruthy();
    expect(w().querySelector("[data-missing='docker_daemon'] code")?.textContent).toMatch(/^Start Docker: sudo systemctl start docker/);
    expect(row("docker_daemon").textContent).toContain("daemon unreachable");
    expect(row("playwright_images").textContent).toContain("Playwright images: informational");
    expect(document.getElementById(windowSubtitleId("setup"))?.textContent).toBe("4 steps pending");
    expectNoPortuguese();
  });
});
