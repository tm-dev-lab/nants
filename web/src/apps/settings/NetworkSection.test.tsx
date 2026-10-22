/*
 * Configurações, seção "Acesso pela rede" (spec 097, T02): só em `--lan`; endereço com o host da
 * página, faixa e estado Liberada ou Pendente (marcar e desmarcar); o script do PowerShell só com
 * `wsl: true`, gerado com os valores reais.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { NetworkView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { appStore } from "../../store";
import { makeProject, makeProjectState } from "../../test-utils";
import { reachEnvironment } from "../app/runSession";
import { NETWORK_TEXT, wslPowerShellScript } from "./networkModel";

const original = { ...reachEnvironment };

afterEach(() => {
  cleanup();
  Object.assign(reachEnvironment, original);
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const RANGE = { start: 3001, end: 3010 };

async function openMachine(network: NetworkView | undefined, globalUi?: Record<string, unknown>) {
  reachEnvironment.pageHostname = () => "192.168.0.68";
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    ui: { p1: { rev: 1, data: { selection: { settingsTab: "machine" } } }, ...(globalUi !== undefined ? { global: { rev: 1, data: globalUi } } : {}) },
  });
  view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: [{ id: "git", label: "Git", ok: true }] }));
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

function section(): HTMLElement | null {
  return settings().querySelector<HTMLElement>("[data-network-section]");
}

function must(el: HTMLElement | null): HTMLElement {
  if (el === null) throw new Error("elemento ausente");
  return el;
}

describe("Configurações: Acesso pela rede (097/T02)", () => {
  it("fora do --lan (sem faixa) ou em servidor antigo: a seção não aparece", async () => {
    await openMachine({ nants_port: 3000, lan_view_ports: null, wsl: true });
    expect(settings().querySelector("[data-machine-tab]")).not.toBeNull();
    expect(section()).toBeNull();
    cleanup();
    await openMachine(undefined);
    expect(section()).toBeNull();
  });

  it("em --lan fora do WSL: endereço, faixa e Pendente, sem nenhum comando", async () => {
    await openMachine({ nants_port: 3000, lan_view_ports: RANGE, wsl: false });
    const s = must(section());
    expect(within(s).getByRole("heading", { name: "Acesso pela rede" })).toBeTruthy();
    expect(s.querySelector("[data-network-address]")?.textContent).toBe("http://192.168.0.68:3000");
    expect(s.querySelector("[data-network-range]")?.textContent).toBe("3001 a 3010");
    expect(s.querySelector("[data-network-state]")?.textContent).toBe("Pendente");
    expect(s.textContent).toContain("não consegue verificar");
    expect(s.querySelector("details, pre, code")).toBeNull();
    expect(s.textContent).not.toMatch(/PowerShell|netsh|wsl hostname|mirrored/);
    expect(s.querySelector('[data-variant="primary"]')).toBeNull();
    expect(findForbiddenGlyphs(s)).toEqual([]);
  });

  it("marcar e desmarcar a liberação grava a seção network do estado global", async () => {
    const view = await openMachine({ nants_port: 3000, lan_view_ports: RANGE, wsl: false });
    await act(async () => {
      fireEvent.click(within(must(section())).getByRole("button", { name: "Marcar como liberada" }));
    });
    await settle();
    expect(must(section()).querySelector("[data-network-state]")?.textContent).toBe("Liberada");
    expect(view.server.model.ui.global?.data).toEqual({ network: { released: RANGE } });
    await act(async () => {
      fireEvent.click(within(must(section())).getByRole("button", { name: "Marcar como pendente" }));
    });
    await settle();
    expect(must(section()).querySelector("[data-network-state]")?.textContent).toBe("Pendente");
    expect(view.server.model.ui.global?.data).toEqual({ network: { released: null } });
  });

  it("marcada para outra faixa: Pendente", async () => {
    await openMachine({ nants_port: 3000, lan_view_ports: { start: 3101, end: 3110 }, wsl: false }, { network: { released: RANGE } });
    expect(must(section()).querySelector("[data-network-state]")?.textContent).toBe("Pendente");
  });

  it("no WSL: bloco recolhido com o script real, Copiar e as observações", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await openMachine({ nants_port: 3000, lan_view_ports: RANGE, wsl: true });
    const s = must(section());
    const details = must(s.querySelector<HTMLElement>("details[data-network-wsl]"));
    expect(details.hasAttribute("open")).toBe(false);
    expect(details.querySelector("summary")?.textContent).toBe("Como liberar no Windows (WSL)");
    const script = must(details.querySelector<HTMLElement>("[data-network-script]")).textContent ?? "";
    expect(script).toBe(wslPowerShellScript(3000, RANGE));
    expect(script).toContain("foreach ($p in @(3000) + (3001..3010)) {");
    expect(script).toContain("-LocalPort 3000,3001-3010 -Profile Private -RemoteAddress LocalSubnet");
    expect(details.textContent).toContain("PowerShell como administrador");
    expect(details.textContent).toContain("só para aparelhos da rede local, com a rede marcada como privada no Windows");
    expect(details.querySelector("[data-network-private-note]")?.textContent).toBe(NETWORK_TEXT.wslPrivate);
    expect(details.textContent).toContain("o Windows provavelmente trata a rede como pública");
    expect(details.textContent).toContain("depois de reiniciar, basta rodar o script de novo");
    expect(details.textContent).toContain("networkingMode=mirrored");
    await act(async () => {
      fireEvent.click(within(details).getByRole("button", { name: "Copiar" }));
    });
    await settle();
    expect(writeText).toHaveBeenCalledWith(script);
    expect(within(details).getByRole("button", { name: "Copiado" })).toBeTruthy();
    expect(findForbiddenGlyphs(s)).toEqual([]);
  });
});
