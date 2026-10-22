/*
 * Faixa de portas de visualização (spec 097, T02): pela rede, a ajuda do App e da Execução fala da
 * faixa real vinda da API; "Já liberei" grava a faixa no estado da interface global (vale para
 * todos os aparelhos) e esconde a ajuda; se a faixa em uso mudar, a ajuda volta.
 */
import { act, cleanup, fireEvent, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { settle } from "../../desktop/shell-test-utils";
import { appWindow, execWindow, mountRun, running } from "./app-test-utils";
import { reachEnvironment } from "./runSession";

const original = { ...reachEnvironment };

const RANGE = { start: 3001, end: 3010 };
const NETWORK = { nants_port: 3000, lan_view_ports: RANGE, wsl: false };

const HINT_3001 =
  "Página em branco neste aparelho? As portas de visualização do Nants (3001 a 3010) podem não estar liberadas na rede do computador onde ele roda. Basta liberar essa faixa uma vez. Depois de liberar, use Já liberei: o Nants não consegue verificar isso sozinho.";

function fromLan(): void {
  reachEnvironment.pageHostname = () => "192.168.0.68";
}

const hint = (): HTMLElement | null => appWindow().querySelector<HTMLElement>("[data-reach-hint]");
const execNote = (): HTMLElement | null => execWindow().querySelector<HTMLElement>("[data-port-hint]");

afterEach(() => {
  cleanup();
  Object.assign(reachEnvironment, original);
  vi.unstubAllGlobals();
});

describe("App e Execução: ajuda com a faixa (097/T02)", () => {
  it("pela rede: a ajuda traz a faixa real, Recarregar e Já liberei, sem comandos", async () => {
    fromLan();
    await mountRun({ app: { status: running(NETWORK) }, open: ["App", "Execução"] });
    const line = hint();
    if (line === null) throw new Error("ajuda ausente");
    expect(line.querySelector("span")?.textContent).toBe(HINT_3001);
    expect(within(line).getAllByRole("button").map((b) => b.textContent)).toEqual(["Recarregar", "Já liberei"]);
    expect(line.querySelector('[data-variant="primary"]')).toBeNull();
    expect(appWindow().querySelector("code, pre, details")).toBeNull();
    expect(line.textContent).not.toMatch(/WSL|PowerShell|netsh|firewall|Windows/);
    expect(execNote()?.textContent).toBe("Página em branco neste aparelho? As portas de visualização (3001 a 3010) podem não estar liberadas na rede.");
    expect(findForbiddenGlyphs(appWindow())).toEqual([]);
    expect(findForbiddenGlyphs(execWindow())).toEqual([]);
  });

  it("acesso local: nenhuma ajuda, mesmo com a faixa", async () => {
    await mountRun({ app: { status: running(NETWORK) }, open: ["App", "Execução"] });
    expect(hint()).toBeNull();
    expect(execNote()).toBeNull();
  });

  it("Já liberei grava a faixa no estado global e esconde a ajuda no App e na Execução", async () => {
    fromLan();
    const view = await mountRun({ app: { status: running(NETWORK) }, open: ["App", "Execução"] });
    const line = hint();
    if (line === null) throw new Error("ajuda ausente");
    await act(async () => {
      fireEvent.click(within(line).getByRole("button", { name: "Já liberei" }));
    });
    await settle();
    expect(hint()).toBeNull();
    expect(execNote()).toBeNull();
    const puts = view.server.calls.filter((c) => c.method === "PUT" && c.url === "/api/ui-state");
    expect(puts.map((c) => c.body)).toEqual([{ base_rev: 0, section: "network", value: { released: RANGE } }]);
    expect(view.server.model.ui.global?.data).toEqual({ network: { released: RANGE } });
  });

  it("marcada em outro aparelho: a ajuda já não aparece", async () => {
    fromLan();
    await mountRun({ app: { status: running(NETWORK) }, globalUi: { network: { released: RANGE } }, open: ["App", "Execução"] });
    expect(hint()).toBeNull();
    expect(execNote()).toBeNull();
  });

  it("a faixa em uso mudou: a ajuda volta, com a faixa nova", async () => {
    fromLan();
    await mountRun({
      app: { status: running({ ...NETWORK, lan_view_ports: { start: 3101, end: 3110 } }) },
      globalUi: { network: { released: RANGE } },
      open: ["App", "Execução"],
    });
    expect(hint()?.textContent).toContain("As portas de visualização do Nants (3101 a 3110)");
    expect(execNote()?.textContent).toContain("(3101 a 3110)");
  });
});
