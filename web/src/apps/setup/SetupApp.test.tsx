import { cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Readiness } from "../../desktop/api";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { notReady, READY, renderShell, settle } from "../../desktop/shell-test-utils";
import { makeProject, makeProjectState } from "../../test-utils";
import { diagnosisStatus, firstPendingStep, setupSubtitle, stepDone, stepMissing } from "./setup";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const CONN = { id: "openai", name: "OpenAI", base_url: "https://api.openai.com/v1", api_key_masked: "sk-...abcd" };

function setupWindow(): HTMLElement {
  return screen.getByRole("region", { name: "Primeiros passos" });
}

function step(name: RegExp): HTMLElement {
  return within(setupWindow()).getByRole("button", { name });
}

describe("etapas (modelo)", () => {
  it("cada etapa lê as próprias pendências; projeto exige ao menos um", () => {
    const r = notReady();
    expect(stepMissing(r, "machine").map((i) => i.id)).toEqual(["docker_daemon"]);
    expect(stepMissing(r, "connection").map((i) => i.id)).toEqual(["connection"]);
    expect(stepMissing(r, "agents").map((i) => i.id)).toEqual(["agent_architect"]);
    expect(stepDone(r, "project", 0)).toBe(false);
    expect(stepDone(READY, "project", 1)).toBe(true);
    expect(firstPendingStep(r, 0)).toBe("machine");
    expect(firstPendingStep(READY, 0)).toBe("project");
    expect(setupSubtitle(r, 0)).toBe("4 etapas pendentes");
    expect(setupSubtitle(READY, 2)).toBe("Tudo pronto");
  });

  it("estado de cada linha do diagnóstico segue o nants doctor", () => {
    expect(diagnosisStatus({ id: "git", label: "Git", ok: true })).toBe("ok");
    expect(diagnosisStatus({ id: "disk", label: "Disco", ok: false })).toBe("fail");
    expect(diagnosisStatus({ id: "wsl", label: "WSL", ok: true, info: true })).toBe("info");
  });
});

describe("Primeiros passos", () => {
  it("abre sozinha quando o chat não está pronto, sem roubar o foco, e só uma vez", async () => {
    const { server } = await renderShell({ projects: [], readiness: notReady() });
    const win = setupWindow();
    expect(win.contains(document.activeElement)).toBe(false);
    expect(within(win).getByRole("heading", { name: "1. Sua máquina" })).toBeTruthy();
    // Fechar não a reabre sozinha (nem numa nova leitura da prontidão).
    fireEvent.click(within(win).getByRole("button", { name: "Fechar" }));
    expect(screen.queryByRole("region", { name: "Primeiros passos" })).toBeNull();
    server.model.readiness = notReady();
    fireEvent.click(screen.getByRole("button", { name: "Setup incompleto" }));
    expect(setupWindow()).toBeTruthy();
    expect(findForbiddenGlyphs(setupWindow())).toEqual([]);
  });

  it("não abre sozinha quando o chat está pronto (mesmo com a execução pendente)", async () => {
    const readiness: Readiness = { ...READY, execute: notReady().execute };
    await renderShell({ projects: [makeProject("p1", "Loja")], readiness });
    expect(screen.queryByRole("region", { name: "Primeiros passos" })).toBeNull();
  });

  it("abre sozinha também dentro de um espaço, depois do layout da fase", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Loja")],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      readiness: notReady(),
    });
    expect(setupWindow()).toBeTruthy();
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
  });

  it("Sua máquina: comandos para copiar e 'Verificar de novo'", async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    const { server } = await renderShell({ readiness: notReady() });
    const win = setupWindow();
    const item = win.querySelector<HTMLElement>("[data-missing='docker_daemon']");
    if (item === null) throw new Error("pendência ausente");
    // 095/T06: uma linha com divisória, sem cartão contornado.
    expect(item.classList.contains("nd-card")).toBe(false);
    expect(item.classList.contains("nd-missing")).toBe(true);
    expect(item.textContent).toContain("sudo systemctl start docker");
    fireEvent.click(within(item).getByRole("button", { name: "Copiar o comando para Docker daemon" }));
    await settle(3);
    expect(writeText).toHaveBeenCalledWith("sudo systemctl start docker");
    server.model.readiness = { ...notReady(), execute: { ok: false, missing: notReady().chat.missing } };
    fireEvent.click(within(win).getByRole("button", { name: "Verificar de novo" }));
    await settle();
    expect(server.count("POST", /^\/api\/readiness\/refresh$/)).toBe(1);
    expect(within(setupWindow()).getByText("Sua máquina está pronta.")).toBeTruthy();
  });

  it("Sua máquina: diagnóstico completo do nants doctor, com OK, falha e informativo", async () => {
    const diagnosis = [
      { id: "git", label: "Git", ok: true, version: "git version 2.43.0" },
      { id: "docker_daemon", label: "Docker daemon", ok: false, detail: "daemon inacessível", fix: "sudo systemctl start docker" },
      { id: "disk", label: "Espaço em disco", ok: false, detail: "1,0 GB livres em /home/u/NantsProjects", fix: "Libere espaço em disco" },
      { id: "wsl", label: "WSL", ok: true, info: true, detail: "detectado" },
      { id: "playwright_images", label: "Imagens do Playwright", ok: true, info: true, detail: "nenhuma baixada ainda: o primeiro teste de navegador de um projeto baixa cerca de 2 GB" },
    ];
    const { server } = await renderShell({ readiness: notReady() });
    server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: diagnosis }));
    fireEvent.click(step(/Conexão/));
    fireEvent.click(step(/Sua máquina/));
    await settle();
    const list = within(setupWindow()).getByRole("list", { name: "Diagnóstico da máquina" });
    const row = (id: string): HTMLElement => {
      const el = list.querySelector<HTMLElement>(`[data-prereq='${id}']`);
      if (el === null) throw new Error(`linha ${id} ausente`);
      return el;
    };
    expect(row("git").getAttribute("data-status")).toBe("ok");
    expect(row("git").textContent).toContain("git version 2.43.0");
    expect(row("wsl").getAttribute("data-status")).toBe("info");
    expect(row("wsl").textContent).toContain("informativo");
    expect(row("playwright_images").textContent).toContain("cerca de 2 GB");
    expect(row("disk").getAttribute("data-status")).toBe("fail");
    expect(within(row("disk")).getByRole("button", { name: "Copiar o comando para Espaço em disco" })).toBeTruthy();
    // A correção do daemon já está na lista de pendências: não se repete no diagnóstico.
    expect(within(row("docker_daemon")).queryByRole("button")).toBeNull();
    expect(within(setupWindow()).getAllByRole("button", { name: "Copiar o comando para Docker daemon" })).toHaveLength(1);
    expect(findForbiddenGlyphs(setupWindow())).toEqual([]);
    // "Verificar de novo" relê o diagnóstico.
    const before = server.count("GET", /^\/api\/system\/prereqs$/);
    fireEvent.click(within(setupWindow()).getByRole("button", { name: "Verificar de novo" }));
    await settle();
    expect(server.count("GET", /^\/api\/system\/prereqs$/)).toBe(before + 1);
  });

  it("Sua máquina: sem o diagnóstico completo, as pendências continuam", async () => {
    await renderShell({ readiness: notReady() });
    await settle();
    const win = setupWindow();
    expect(within(win).getByText("Não foi possível ler o diagnóstico completo agora.")).toBeTruthy();
    expect(win.querySelector("[data-missing='docker_daemon']")).not.toBeNull();
  });

  it("Conexão: adicionar (sem a chave no estado depois do envio) e testar", async () => {
    const { server } = await renderShell({ readiness: notReady() });
    server.reply("PUT", /^\/api\/config\/connections$/, (body) => {
      const b = body as { id: string; name: string; base_url: string };
      server.model.connections = [{ id: b.id, name: b.name, base_url: b.base_url, api_key_masked: "sk-...1234" }];
      return { status: 200, body: server.model.connections[0] };
    });
    server.reply("POST", /^\/api\/config\/connections\/test$/, () => ({ status: 200, body: { ok: true, latency_ms: 42, models_count: 3 } }));
    fireEvent.click(step(/Conexão/));
    const win = setupWindow();
    expect(within(win).getByRole("heading", { name: "2. Conexão" })).toBeTruthy();
    fireEvent.change(within(win).getByLabelText("Identificador"), { target: { value: "openai" } });
    fireEvent.change(within(win).getByLabelText("Nome"), { target: { value: "OpenAI" } });
    fireEvent.change(within(win).getByLabelText("URL base"), { target: { value: "https://api.openai.com/v1" } });
    const key = within(win).getByLabelText("Chave de API") as HTMLInputElement;
    fireEvent.change(key, { target: { value: "sk-secreta" } });
    const readinessBefore = server.count("GET", /^\/api\/readiness$/);
    fireEvent.click(within(win).getByRole("button", { name: "Salvar conexão" }));
    await settle();
    const put = server.calls.find((c) => c.method === "PUT" && c.url === "/api/config/connections");
    expect(put?.body).toMatchObject({ id: "openai", api_key: "sk-secreta", wire_api: "chat" });
    expect(server.count("GET", /^\/api\/readiness$/)).toBe(readinessBefore + 1);
    expect(document.body.textContent).not.toContain("sk-secreta");
    const card = setupWindow().querySelector<HTMLElement>("[data-connection='openai']");
    if (card === null) throw new Error("cartão ausente");
    // 095/T06: a mesma linha das Configurações; aqui "Testar" fica na própria linha.
    expect(card.classList.contains("nd-conn")).toBe(true);
    expect(card.querySelector("[data-conn-detail]")?.textContent).toContain("api.openai.com");
    fireEvent.click(within(card).getByRole("button", { name: "Testar conexão OpenAI" }));
    await settle();
    expect(within(card).getByRole("status").textContent).toContain("Conexão ok");
  });

  it("Agentes: 'Usar o mesmo modelo para todos' envia o modelo do Arquiteto aos três", async () => {
    const { server } = await renderShell({ readiness: notReady(), connections: [CONN] });
    server.reply("GET", /\/models/, () => ({ status: 200, body: { models: [{ id: "gpt-x" }], truncated: false } }));
    server.reply("PUT", /^\/api\/config\/agents$/, (body) => ({ status: 200, body }));
    fireEvent.click(step(/Agentes/));
    await settle();
    const win = setupWindow();
    const architect = within(win).getByRole("region", { name: "Arquiteto" });
    fireEvent.change(within(architect).getByLabelText(/Conexão/), { target: { value: "openai" } });
    fireEvent.change(within(architect).getByLabelText(/Modelo/), { target: { value: "gpt-x" } });
    fireEvent.click(within(win).getByLabelText("Usar o mesmo modelo para todos"));
    const developer = within(win).getByRole("region", { name: "Desenvolvedor" });
    expect((within(developer).getByLabelText(/Conexão/) as HTMLSelectElement).disabled).toBe(true);
    const readinessBefore = server.count("GET", /^\/api\/readiness$/);
    fireEvent.click(within(win).getByRole("button", { name: "Salvar agentes" }));
    await settle();
    const put = server.calls.find((c) => c.method === "PUT" && c.url === "/api/config/agents");
    const body = put?.body as Record<string, { connection_id: string; model: string; effort: string }>;
    for (const role of ["architect", "developer", "reviewer"]) {
      expect(body[role]?.connection_id).toBe("openai");
      expect(body[role]?.model).toBe("gpt-x");
    }
    expect(body.reviewer?.effort).toBe("low");
    expect(server.count("GET", /^\/api\/readiness$/)).toBe(readinessBefore + 1);
    expect(within(setupWindow()).getByText("Agentes salvos.")).toBeTruthy();
  });

  it("Primeiro projeto: abre o diálogo Novo projeto", async () => {
    await renderShell({ readiness: notReady() });
    fireEvent.click(step(/Primeiro projeto/));
    fireEvent.click(within(setupWindow()).getByRole("button", { name: "Criar o primeiro projeto" }));
    expect(screen.getByRole("dialog", { name: "Novo projeto" })).toBeTruthy();
  });

  it("navegação: Próxima etapa e Anterior; etapas concluídas marcadas", async () => {
    const docker = notReady().execute.missing.filter((i) => i.id === "docker_daemon");
    await renderShell({ readiness: { ...READY, execute: { ok: false, missing: docker } }, projects: [makeProject("p1", "Loja")] });
    fireEvent.click(screen.getByRole("button", { name: "Setup incompleto" }));
    const win = setupWindow();
    expect(within(win).getByRole("heading", { name: "1. Sua máquina" })).toBeTruthy();
    expect(step(/Conexão/).getAttribute("data-done")).toBe("true");
    expect(step(/Sua máquina/).getAttribute("aria-current")).toBe("step");
    fireEvent.click(within(win).getByRole("button", { name: "Próxima etapa" }));
    expect(within(setupWindow()).getByRole("heading", { name: "2. Conexão" })).toBeTruthy();
    fireEvent.click(within(setupWindow()).getByRole("button", { name: "Anterior" }));
    expect(within(setupWindow()).getByRole("heading", { name: "1. Sua máquina" })).toBeTruthy();
    // Subtítulo vivo na barra de título.
    expect(win.closest("[data-window]")?.textContent).toContain("1 etapa pendente");
  });
});
