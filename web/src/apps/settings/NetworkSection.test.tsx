/*
 * Configurações, seção "Acesso pela rede" (spec 097, T02; spec 111, T03): o interruptor da opção
 * salva com a API simulada, o aviso de reinício, a coerência com o Windows e as instruções por
 * ambiente; e, com o Nants na rede nesta execução, endereço, faixa e Liberada ou Pendente.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { NetworkConfig, NetworkView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { appStore } from "../../store";
import type { RouteReply } from "../../test-utils";
import { makeProject, makeProjectState } from "../../test-utils";
import { reachEnvironment } from "../app/runSession";
import { NETWORK_TEXT, mirroredEnableScript, mirroredUndoScript, wslPowerShellScript, wslUndoScript } from "./networkModel";

const original = { ...reachEnvironment };

afterEach(() => {
  cleanup();
  Object.assign(reachEnvironment, original);
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const RANGE = { start: 3001, end: 3010 };

function netConfig(over: Partial<NetworkConfig> = {}): NetworkConfig {
  return {
    env: "wsl_nat",
    saved_lan: true,
    lan: true,
    mode_source: "saved",
    port: 3000,
    lan_port: 3000,
    view_ports: RANGE,
    windows_redirect: "yes",
    windows_redirect_target: "this_wsl",
    firewall_rule: "yes",
    restart_required: false,
    restart_command: "nants",
    coherence: "ok",
    ...over,
  };
}

type NetReply = NetworkConfig | RouteReply | Promise<RouteReply>;

async function openMachine(network: NetworkView | undefined, opts: { net?: NetReply; globalUi?: Record<string, unknown> } = {}) {
  reachEnvironment.pageHostname = () => "192.168.0.68";
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    ui: { p1: { rev: 1, data: { selection: { settingsTab: "machine" } } }, ...(opts.globalUi !== undefined ? { global: { rev: 1, data: opts.globalUi } } : {}) },
  });
  view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: [{ id: "git", label: "Git", ok: true }] }));
  const net = opts.net;
  if (net !== undefined) {
    view.server.reply("GET", /^\/api\/config\/network$/, () => {
      if (net instanceof Promise) return net as unknown as RouteReply;
      return "env" in net ? { status: 200, body: net } : net;
    });
  }
  act(() => {
    appStore.setState((s) => (s.config === null || network === undefined ? s : { config: { ...s.config, network } }));
  });
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Configurações" }));
  });
  await settle();
  return view;
}

function settings(): HTMLElement {
  return screen.getByRole("region", { name: "Configurações" });
}

function section(): HTMLElement {
  const el = settings().querySelector<HTMLElement>("[data-network-section]");
  if (el === null) throw new Error("seção ausente");
  return el;
}

function must<T extends Element>(el: T | null): T {
  if (el === null) throw new Error("elemento ausente");
  return el;
}

function lanSwitch(): HTMLElement {
  return within(section()).getByRole("switch", { name: "Acesso pela rede local" });
}

describe("Configurações: Acesso pela rede, faixa de visualização (097/T02)", () => {
  it("fora do --lan: sem endereço nem faixa; servidor sem a API de rede: aviso discreto", async () => {
    await openMachine({ nants_port: 3000, lan_view_ports: null, wsl: true });
    const s = section();
    expect(s.querySelector("[data-network-range]")).toBeNull();
    expect(s.querySelector("[data-network-load-failed]")?.textContent).toBe(NETWORK_TEXT.loadFailed);
    expect(within(s).queryByRole("switch")).toBeNull();
  });

  it("em --lan: endereço, faixa e Pendente; marcar e desmarcar grava o estado global", async () => {
    const view = await openMachine({ nants_port: 3000, lan_view_ports: RANGE, wsl: false }, { net: netConfig({ env: "linux" }) });
    const s = section();
    expect(within(s).getByRole("heading", { name: "Acesso pela rede" })).toBeTruthy();
    expect(s.querySelector("[data-network-address]")?.textContent).toBe("http://192.168.0.68:3000");
    expect(s.querySelector("[data-network-range]")?.textContent).toBe("3001 a 3010");
    expect(s.querySelector("[data-network-state]")?.textContent).toBe("Pendente");
    await act(async () => {
      fireEvent.click(within(section()).getByRole("button", { name: "Marcar como liberada" }));
    });
    await settle();
    expect(section().querySelector("[data-network-state]")?.textContent).toBe("Liberada");
    expect(view.server.model.ui.global?.data).toEqual({ network: { released: RANGE } });
    await act(async () => {
      fireEvent.click(within(section()).getByRole("button", { name: "Marcar como pendente" }));
    });
    await settle();
    expect(view.server.model.ui.global?.data).toEqual({ network: { released: null } });
  });

  it("marcada para outra faixa: Pendente", async () => {
    await openMachine({ nants_port: 3000, lan_view_ports: { start: 3101, end: 3110 }, wsl: false }, { globalUi: { network: { released: RANGE } } });
    expect(section().querySelector("[data-network-state]")?.textContent).toBe("Pendente");
  });
});

describe("Configurações: Acesso pela rede local (111/T03)", () => {
  it("carregamento discreto enquanto o Windows é consultado, sem travar as Configurações", async () => {
    let release: (r: RouteReply) => void = () => undefined;
    const gate = new Promise<RouteReply>((resolve) => {
      release = resolve;
    });
    await openMachine(undefined, { net: gate });
    expect(section().querySelector("[data-network-loading]")?.textContent).toBe(NETWORK_TEXT.loading);
    expect(settings().querySelector("[data-machine-tab]")).not.toBeNull();
    expect(within(section()).queryByRole("switch")).toBeNull();
    await act(async () => {
      release({ status: 200, body: netConfig() });
    });
    await settle();
    expect(section().querySelector("[data-network-loading]")).toBeNull();
    expect(lanSwitch().getAttribute("aria-checked")).toBe("true");
  });

  it("interruptor: grava pela API e diz que vale no próximo início, com o comando para reiniciar", async () => {
    let current = netConfig({ saved_lan: null, lan: false, mode_source: "default", windows_redirect: "no", windows_redirect_target: "none", firewall_rule: "no" });
    const view = await openMachine(undefined, { net: current });
    const puts: unknown[] = [];
    // Servidor simulado: grava a opção e recalcula a coerência, relida pela tela depois de gravar.
    view.server.reply("GET", /^\/api\/config\/network$/, () => ({ status: 200, body: current }));
    view.server.reply("PUT", /^\/api\/config\/network$/, (body) => {
      puts.push(body);
      const lan = (body as { lan: boolean }).lan;
      current = { ...current, saved_lan: lan, coherence: lan ? "needs_script" : "ok" };
      return { status: 200, body: { saved_lan: lan, lan: false, applies_on_next_start: true, restart_required: lan, restart_command: "nants" } };
    });
    expect(lanSwitch().getAttribute("aria-checked")).toBe("false");
    expect(section().querySelector("[data-network-restart]")).toBeNull();
    await act(async () => {
      fireEvent.click(lanSwitch());
    });
    await settle();
    expect(puts).toEqual([{ lan: true }]);
    expect(view.server.count("GET", /^\/api\/config\/network$/)).toBe(2);
    expect(lanSwitch().getAttribute("aria-checked")).toBe("true");
    // Salva ligada e sem reiniciar (spec 112, decisão 8): o passo 1 fica pronto com a pendência
    // discreta do reinício, e o passo 2 já libera, com o script de ligar aberto.
    expect(section().querySelector("[data-network-restart]")).toBeNull();
    expect(section().querySelector("[data-step='enable'] [data-step-detail]")?.textContent).toBe("Ligada; vale ao reiniciar (Ctrl+C e nants).");
    const windows = must(section().querySelector<HTMLElement>("[data-step='windows']"));
    expect(windows.getAttribute("data-step-status")).toBe("pending");
    expect(section().querySelector("[data-network-coherence]")?.textContent).toBe("Falta rodar o script de ligar no Windows.");
    expect(must(section().querySelector("details[data-network-script-block='enable']")).hasAttribute("open")).toBe(true);
    // Desligar de novo: volta a coincidir com esta execução; nada de reinício.
    await act(async () => {
      fireEvent.click(lanSwitch());
    });
    await settle();
    expect(puts).toEqual([{ lan: true }, { lan: false }]);
    expect(section().querySelector("[data-network-restart]")).toBeNull();
    expect(section().querySelector("[data-network-saved]")?.textContent).toBe("Salvo. Já vale nesta execução.");
    expect(section().querySelector("[data-network-coherence]")).toBeNull();
    expect(findForbiddenGlyphs(section())).toEqual([]);
  });

  it("falha ao gravar: mensagem e o interruptor fica como estava", async () => {
    const view = await openMachine(undefined, { net: netConfig({ env: "linux", saved_lan: false, lan: false }) });
    view.server.reply("PUT", /^\/api\/config\/network$/, () => ({ status: 500, body: { error: "disco cheio" } }));
    await act(async () => {
      fireEvent.click(lanSwitch());
    });
    await settle();
    expect(lanSwitch().getAttribute("aria-checked")).toBe("false");
    expect(section().querySelector("[data-network-save-error]")?.textContent).toContain("Não foi possível salvar");
  });

  it("modo do --lan: aviso discreto e nenhum pedido de reinício sem mudança nesta tela", async () => {
    await openMachine(undefined, { net: netConfig({ saved_lan: null, lan: true, mode_source: "flag", restart_required: true, coherence: "will_adopt" }) });
    expect(section().querySelector("[data-network-flag]")?.textContent).toBe("O modo desta execução veio do --lan, que não fica salvo.");
    expect(section().querySelector("[data-network-restart]")).toBeNull();
    // Adoção possível (código do servidor): só informa; nada de desfazer nem de porta diferente.
    const adopt = must(section().querySelector<HTMLElement>("[data-network-coherence='will_adopt']"));
    expect(adopt.textContent).toBe("O Windows já está configurado para este WSL: o próximo início liga o acesso pela rede local.");
    expect(adopt.className).toBe("nd-field-hint");
    expect(section().textContent).not.toContain("rode o script de desfazer");
    expect(must(section().querySelector("details[data-network-script-block='undo']")).hasAttribute("open")).toBe(false);
  });

  it("opção salva diferente do modo atual (fora do --lan): aviso discreto com o reinício", async () => {
    await openMachine(undefined, { net: netConfig({ saved_lan: false, lan: true, mode_source: "host" }) });
    const restart = must(section().querySelector<HTMLElement>("[data-network-restart]"));
    expect(restart.textContent).toContain("Nesta execução, o Nants está na rede local.");
    expect(restart.textContent).toContain("Vale a partir do próximo início");
  });

  it("Linux: sem script, só a dica do firewall; com o Nants na rede, endereço e faixa uma vez só", async () => {
    await openMachine(undefined, { net: netConfig({ env: "linux", windows_redirect: "no", windows_redirect_target: "none", firewall_rule: "unknown" }) });
    let s = section();
    expect(s.querySelector("details, pre")).toBeNull();
    expect(s.textContent).not.toMatch(/PowerShell|netsh|portproxy/);
    expect(s.querySelector("[data-network-linux]")?.textContent).toBe("Se houver firewall no Linux, libere as portas 3000, 3001 a 3010.");
    expect(s.querySelector("[data-network-coherence]")).toBeNull();
    cleanup();
    await openMachine({ nants_port: 3000, lan_view_ports: RANGE, wsl: false }, { net: netConfig({ env: "linux", firewall_rule: "unknown" }) });
    s = section();
    expect(s.querySelector("[data-network-linux]")?.textContent).toBe(NETWORK_TEXT.linuxHintAbove);
    expect(s.textContent?.split("192.168.0.68:3000").length).toBe(2);
    expect(s.textContent?.split("3001 a 3010").length).toBe(2);
    // No Linux, a marcação manual continua.
    expect(s.querySelector("[data-network-state]")?.textContent).toBe("Pendente");
    expect(within(s).getByRole("button", { name: "Marcar como liberada" })).toBeTruthy();
  });

  it("WSL com o Windows conferido: estado real numa linha, sem marcação manual; sem conferir, a marcação volta", async () => {
    await openMachine({ nants_port: 3000, lan_view_ports: RANGE, wsl: true }, { net: netConfig() });
    let s = section();
    expect(s.querySelector("[data-network-state]")?.textContent).toBe("Liberada no Windows");
    expect(s.querySelector("[data-network-toggle]")).toBeNull();
    expect(s.textContent).not.toContain("não consegue verificar");
    cleanup();
    await openMachine({ nants_port: 3000, lan_view_ports: RANGE, wsl: true }, { net: netConfig({ windows_redirect: "no", windows_redirect_target: "none" }) });
    s = section();
    expect(s.querySelector("[data-network-state]")?.textContent).toBe("Pendente no Windows");
    expect(s.querySelector("[data-network-toggle]")).toBeNull();
    cleanup();
    await openMachine({ nants_port: 3000, lan_view_ports: RANGE, wsl: true }, { net: netConfig({ windows_redirect: "unknown", windows_redirect_target: "unknown", firewall_rule: "unknown" }) });
    s = section();
    expect(s.querySelector("[data-network-state]")?.textContent).toBe("Pendente");
    expect(within(s).getByRole("button", { name: "Marcar como liberada" })).toBeTruthy();
  });

  it("WSL NAT ligado e coerente: scripts de ligar e desfazer recolhidos, com copiar", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await openMachine(undefined, { net: netConfig() });
    const s = section();
    expect(s.querySelector("[data-network-coherence]")).toBeNull();
    const enable = must(s.querySelector<HTMLElement>("details[data-network-script-block='enable']"));
    const undo = must(s.querySelector<HTMLElement>("details[data-network-script-block='undo']"));
    expect(enable.hasAttribute("open")).toBe(false);
    expect(undo.hasAttribute("open")).toBe(false);
    expect(enable.querySelector("summary")?.textContent).toBe("Script para ligar no Windows");
    expect(undo.querySelector("summary")?.textContent).toBe("Script para desfazer no Windows");
    expect(enable.querySelector("[data-network-script]")?.textContent).toBe(wslPowerShellScript(3000, RANGE));
    expect(undo.querySelector("[data-network-script]")?.textContent).toBe(wslUndoScript(3000, RANGE));
    expect(enable.textContent).toContain("PowerShell como administrador");
    expect(enable.querySelector("[data-network-private-note]")).toBeNull();
    expect(s.textContent).not.toMatch(/marcada como privada|Rede privada/);
    expect(s.querySelector("[data-network-security]")?.textContent).toBe(NETWORK_TEXT.security);
    expect(enable.textContent).toContain("networkingMode=mirrored");
    expect(undo.textContent).toContain("PowerShell como administrador");
    await act(async () => {
      fireEvent.click(within(undo).getByRole("button", { name: "Copiar" }));
    });
    await settle();
    expect(writeText).toHaveBeenCalledWith(wslUndoScript(3000, RANGE));
    expect(within(undo).getByRole("button", { name: "Copiado" })).toBeTruthy();
    expect(within(enable).getByRole("button", { name: "Copiar" })).toBeTruthy();
    expect(findForbiddenGlyphs(s)).toEqual([]);
  });

  it("WSL NAT desligado com redirecionamento: rodar o script de desfazer, que abre", async () => {
    await openMachine(undefined, { net: netConfig({ saved_lan: false, lan: false, port: 3001, lan_port: 3000, coherence: "still_redirected" }) });
    const s = section();
    expect(s.querySelector("[data-network-coherence='still_redirected']")?.textContent).toBe(
      "O Windows ainda redireciona as portas: rode o script de desfazer. Por isso o Nants pode estar numa porta diferente da 3000.",
    );
    expect(must(s.querySelector("details[data-network-script-block='undo']")).hasAttribute("open")).toBe(true);
    // Rede desligada: o passo 2 espera o passo 1 e mostra só o script de desfazer.
    expect(s.querySelector("details[data-network-script-block='enable']")).toBeNull();
  });

  it("WSL NAT ligado com outro destino ou sem a regra: rodar o script de ligar", async () => {
    await openMachine(undefined, { net: netConfig({ windows_redirect_target: "other", coherence: "other_target" }) });
    expect(section().querySelector("[data-network-coherence]")?.textContent).toContain("rode o script de ligar de novo");
    cleanup();
    await openMachine(undefined, { net: netConfig({ firewall_rule: "no", coherence: "missing_rule" }) });
    expect(section().querySelector("[data-network-coherence]")?.textContent).toBe("Falta a regra de firewall Nants no Windows: rode o script de ligar.");
  });

  it("não deu para conferir o Windows: nenhum aviso de coerência", async () => {
    await openMachine(undefined, { net: netConfig({ windows_redirect: "unknown", windows_redirect_target: "unknown", firewall_rule: "unknown", coherence: "unknown" }) });
    expect(section().querySelector("[data-network-coherence]")).toBeNull();
    cleanup();
    await openMachine(undefined, { net: netConfig({ saved_lan: false, lan: false, windows_redirect: "unknown", windows_redirect_target: "unknown", coherence: "unknown" }) });
    expect(section().querySelector("[data-network-coherence]")).toBeNull();
  });

  it("WSL espelhado: só a regra de firewall, para ligar e para desfazer", async () => {
    await openMachine(undefined, { net: netConfig({ env: "wsl_mirrored", windows_redirect: "no", windows_redirect_target: "none", firewall_rule: "yes" }) });
    const s = section();
    expect(s.textContent).toContain(NETWORK_TEXT.mirroredIntro);
    const enable = must(s.querySelector("details[data-network-script-block='enable'] [data-network-script]")).textContent ?? "";
    const undo = must(s.querySelector("details[data-network-script-block='undo'] [data-network-script]")).textContent ?? "";
    expect(enable).toBe(mirroredEnableScript(3000, RANGE));
    expect(undo).toBe(mirroredUndoScript());
    expect(enable + undo).not.toContain("portproxy");
    expect(s.querySelector("[data-network-coherence]")).toBeNull();
    cleanup();
    await openMachine(undefined, { net: netConfig({ env: "wsl_mirrored", windows_redirect: "no", windows_redirect_target: "none", firewall_rule: "no", coherence: "missing_rule" }) });
    expect(section().querySelector("[data-network-coherence]")?.textContent).toBe("Falta a regra de firewall Nants no Windows: rode o script de ligar.");
  });

  it("WSL desconhecido: as instruções do NAT e a frase do modo espelhado", async () => {
    await openMachine(undefined, { net: netConfig({ env: "wsl_unknown" }) });
    const s = section();
    expect(s.querySelector("[data-network-unknown-note]")?.textContent).toBe(
      "Não deu para conferir o modo de rede do WSL. No modo espelhado, só a parte do firewall é necessária.",
    );
    expect(s.querySelector("details[data-network-script-block='enable'] [data-network-script]")?.textContent).toBe(wslPowerShellScript(3000, RANGE));
  });
});
