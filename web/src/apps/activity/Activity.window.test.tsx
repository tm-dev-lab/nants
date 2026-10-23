/*
 * Janela Atividade montada pelo shell (spec 093, T07): um cartão por agente com a ação ao vivo
 * (limitada por quadro), as últimas ações da tarefa ativa, a verificação final em destaque e a aba
 * Histórico com os informativos, fora da contagem do sino, e a verificação final.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent, ProjectState } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { manualScheduler } from "../../desktop/test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  // O `resetStore` não limpa as atividades ao vivo.
  appStore.setState({ activities: {} });
});

const P1 = makeProject("p1", "Loja");

function running(): ProjectState {
  const spec = makeSpec("s1", "Loja online", "IN_PROGRESS", [makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "IN_PROGRESS")]);
  return makeProjectState({ id: "p1", name: "Loja", specs: [spec], running: true, run: { spec_id: "s1", status: "running" } });
}

function activity(payload: Record<string, unknown>): ApiEvent {
  return { event_type: "agent_activity", project_id: "p1", payload: { activity_id: "a1", ...payload } } as unknown as ApiEvent;
}

function panel(): HTMLElement {
  return screen.getByRole("region", { name: "Atividade" });
}

const TRAIL = {
  items: [
    { seq: 4, ts: 1_700_000_000, agent: "developer", task_id: "T02", kind: "tool", text: "gerando imagem web/img/gato.webp" },
    { seq: 5, ts: 1_700_000_030, agent: "developer", task_id: "T02", kind: "command", text: "npm test" },
  ],
  total: 2,
  has_more: false,
};

/** Linha do manifesto de imagens como o servidor grava (`sdd::images::ManifestEntry`). */
function manifestLine(path: string, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({ path, hash: "a".repeat(64), prompt: "gato", width: 1024, height: 1024, bytes: 2048, task: "T02", at: "2026-10-19T12:00:00Z", ...extra });
}

const MANIFEST = /\/api\/projects\/p1\/file\?source=base&path=\.sdd%2Fspecs%2Fs1%2Fimages\.jsonl/;

async function mount(scheduler = manualScheduler(), trail: typeof TRAIL = TRAIL, manifest: string | null = manifestLine("web/img/gato.webp")) {
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: running() }, scheduler });
  view.server.reply("GET", /\/api\/projects\/p1\/specs\/s1\/tools/, () => ({ status: 200, body: trail }));
  view.server.reply("GET", MANIFEST, () =>
    manifest === null ? { status: 404, body: { error: "arquivo não encontrado" } } : { status: 200, body: { kind: "text", size: manifest.length, content: manifest } },
  );
  await act(async () => {
    appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
  });
  await settle();
  return { ...view, scheduler };
}

describe("Atividade: cartões por agente e ação ao vivo", () => {
  it("um cartão por agente; parados mostram a frase; ao vivo, o estado e a ação", async () => {
    const { stream, scheduler } = await mount();
    const win = panel();
    const cards = win.querySelectorAll("[data-agent-card]");
    expect([...cards].map((c) => c.getAttribute("data-agent-card"))).toEqual(["developer", "reviewer", "architect"]);
    expect(win.querySelector('[data-agent-card="architect"]')?.textContent).toContain("disponível para perguntas");
    // A-05 (094/T08): a frase do agente parado é texto secundário em sem serifa, nunca mono.
    const idle = win.querySelector('[data-agent-card="architect"] [data-agent-idle]');
    expect(idle?.classList.contains("nd-agent-card-idle")).toBe(true);
    expect(idle?.closest(".nd-mono")).toBeNull();
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "tool", text: "gerando imagem web/img/gato.webp", code: "tool.generate_image", params: { path: "web/img/gato.webp" } }));
    });
    // Limitado por quadro: nada muda até o próximo quadro.
    expect(win.querySelector('[data-agent-card="developer"]')?.getAttribute("data-live")).toBeNull();
    await act(async () => {
      scheduler.runFrames();
    });
    const dev = win.querySelector('[data-agent-card="developer"]') as HTMLElement;
    expect(dev.getAttribute("data-live")).toBe("image");
    expect(dev.querySelector("[data-live-state]")?.textContent).toBe("gerando imagem");
    expect(dev.querySelector("[data-live-detail]")?.textContent).toBe("web/img/gato.webp");
    expect(within(win).getByTestId("activity-summary").textContent).toBe("Desenvolvedor em atividade");
    expect(document.getElementById(windowSubtitleId("activity"))?.textContent).toBe("ao vivo");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("'ajustando o ritmo' e 'repetindo' aparecem como estados do cartão", async () => {
    const { stream, scheduler } = await mount();
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "text", text: "ajustando o ritmo ao limite do provedor: 12 s", code: "pace.waiting", params: { seconds: "12" } }));
      stream().emit("agent_activity", activity({ agent: "reviewer", task_id: "T01", activity_id: "r1", kind: "text", text: "resposta do provedor falhou (incompleta: content_filter); repetindo (1/2)" }));
      scheduler.runFrames();
    });
    const win = panel();
    expect(win.querySelector('[data-agent-card="developer"] [data-live-state]')?.textContent).toBe("ajustando o ritmo");
    expect(win.querySelector('[data-agent-card="reviewer"] [data-live-state]')?.textContent).toBe("repetindo");
  });

  it("chamada longa ao modelo: o papel aparece ativo com o tempo e o tamanho recebido (spec 114 T04)", async () => {
    const { stream, scheduler } = await mount();
    const started = Date.now() - 125_000;
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "start" }));
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "model", model_started_at: started }));
      scheduler.runFrames();
    });
    const win = panel();
    const dev = win.querySelector('[data-agent-card="developer"]') as HTMLElement;
    expect(dev.getAttribute("data-live")).toBe("model");
    expect(dev.querySelector("[data-live-state]")?.textContent).toBe("aguardando o modelo");
    expect(dev.querySelector("[data-live-detail]")?.textContent).toBe("há 2 min");
    expect(dev.querySelector("[data-agent-idle]")).toBeNull();
    // O Revisor, que não chamou o modelo, segue aguardando a vez.
    expect(win.querySelector('[data-agent-card="reviewer"]')?.textContent).toContain("aguardando a vez");
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "heartbeat", received_bytes: 49_152 }));
      scheduler.runFrames();
    });
    expect(dev.querySelector("[data-live-state]")?.textContent).toBe("recebendo a resposta");
    expect(dev.querySelector("[data-live-detail]")?.textContent).toBe("há 2 min · 48 KB recebidos");
    expect(within(win).getByTestId("activity-summary").textContent).toBe("Desenvolvedor em atividade");
    // O raciocínio que chega passa à frente; a chamada fica numa linha à parte.
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "reasoning", text: "conferindo o carrinho" }));
      scheduler.runFrames();
    });
    expect(dev.getAttribute("data-live")).toBe("thinking");
    expect(dev.querySelector("[data-live-model]")?.textContent).toBe("recebendo a resposta há 2 min · 48 KB recebidos");
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "model_end" }));
      scheduler.runFrames();
    });
    expect(dev.querySelector("[data-live-model]")).toBeNull();
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("rajada de eventos: um render por quadro, com o valor mais recente", async () => {
    const { stream, scheduler } = await mount();
    await act(async () => {
      for (let i = 1; i <= 20; i += 1) stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "tool", text: `escrevendo arquivo ${i}` }));
    });
    expect(scheduler.pending()).toBeGreaterThan(0);
    await act(async () => {
      scheduler.runFrames();
    });
    expect(panel().querySelector('[data-agent-card="developer"] [data-live-detail]')?.textContent).toBe("escrevendo arquivo 20");
  });

  it("verificação final em destaque e a etapa no cartão do Revisor", async () => {
    const { stream, scheduler } = await mount();
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "reviewer", activity_id: "f1", kind: "start", stage: "final_verification" }));
      scheduler.runFrames();
    });
    const win = panel();
    expect(within(win).getByTestId("final-verification-strip").textContent).toContain("Verificação final em andamento");
    expect(win.querySelector('[data-agent-card="reviewer"] [data-live-stage]')?.textContent).toBe("Verificação final em andamento");
  });

  it("últimas ações da tarefa ativa (trilha filtrada pela tarefa)", async () => {
    const { server } = await mount();
    const recent = within(panel()).getByRole("region", { name: "Últimas ações · T02" });
    expect(recent.textContent).toContain("npm test");
    expect(recent.textContent).toContain("gerando imagem web/img/gato.webp");
    // A mais recente primeiro.
    expect(recent.querySelector("li")?.textContent).toContain("npm test");
    expect(server.calls.some((c) => c.url.includes("/tools?task=T02&limit=8"))).toBe(true);
  });
});

describe("Atividade: aba Histórico", () => {
  it("informativos (Revisor aprovou, Imagem gerada) na aba, gravada no ui-state, sem mexer no sino", async () => {
    const { server } = await mount();
    const bell = (): string => screen.getByRole("button", { name: /Precisa de você/ }).textContent ?? "";
    const before = bell();
    await act(async () => {
      appStore.appendMessage("p1", { role: "agent", agent: "reviewer", taskId: "T01", specId: "s1", attempt: 1, text: "Aprovado — tudo certo" });
    });
    const win = panel();
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "Histórico" }));
    });
    await settle();
    expect(within(win).getByRole("tab", { name: "Histórico" }).getAttribute("aria-selected")).toBe("true");
    const history = within(win).getByRole("region", { name: "Histórico" });
    const kinds = [...history.querySelectorAll("[data-history-kind]")].map((el) => el.getAttribute("data-history-kind"));
    expect(kinds.sort()).toEqual(["image_generated", "reviewer_approved"]);
    expect(history.textContent).toContain("Revisor aprovou T01");
    expect(history.textContent).toContain("web/img/gato.webp");
    expect(bell()).toBe(before);
    const put = server.calls.filter((c) => c.method === "PUT" && (c.body as { section?: string }).section === "selection").at(-1);
    expect((put?.body as { value: { activityTab: string } }).value.activityTab).toBe("history");
    expect(findForbiddenGlyphs(win)).toEqual([]);
    // Setas trocam de aba.
    fireEvent.keyDown(within(win).getByRole("tab", { name: "Histórico" }), { key: "ArrowLeft" });
    await settle();
    expect(within(win).getByRole("tab", { name: "Ao vivo" }).getAttribute("aria-selected")).toBe("true");
  });

  async function historyKinds(): Promise<string[]> {
    const win = panel();
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "Histórico" }));
    });
    await settle();
    const history = within(win).getByRole("region", { name: "Histórico" });
    return [...history.querySelectorAll("[data-history-kind]")].map((el) => el.getAttribute("data-history-kind") ?? "");
  }

  const call = { seq: 1, ts: 1_700_000_000, agent: "developer", task_id: "T02", kind: "tool", text: "gerando imagem web/img/gato.webp" };
  const trailOf = (...items: (typeof call)[]) => ({ items, total: items.length, has_more: false });

  it("chamada em curso (só 'gerando imagem' na trilha, sem manifesto): não é imagem gerada", async () => {
    const { server } = await mount(manualScheduler(), trailOf(call), null);
    expect(await historyKinds()).toEqual([]);
    expect(server.count("GET", MANIFEST)).toBeGreaterThan(0);
  });

  it("falha registrada só na trilha ('geração de imagem falhou'): não é imagem gerada", async () => {
    const failed = { ...call, seq: 2, ts: 1_700_000_040, text: "geração de imagem falhou (provider_http_400 (código: invalid_value))" };
    await mount(manualScheduler(), trailOf(call, failed), null);
    expect(await historyKinds()).toEqual([]);
  });

  it("4ª falha, sem mensagem de sistema nem evento: sem linha gravada, não é imagem gerada", async () => {
    // Acima de 4 MiB: a geração foi cobrada e o manifesto tem a linha com `stored: false`.
    await mount(manualScheduler(), trailOf(call, { ...call, seq: 2 }, { ...call, seq: 3 }, { ...call, seq: 4 }), manifestLine("web/img/gato.webp", { stored: false }));
    expect(await historyKinds()).toEqual([]);
  });

  it("sucesso real (linha gravada no manifesto): imagem gerada, com a tarefa e o caminho", async () => {
    await mount(manualScheduler(), trailOf(call), [manifestLine("web/img/gato.webp"), manifestLine("web/img/cao.webp", { reused: true })].join("\n"));
    expect(await historyKinds()).toEqual(["image_generated", "image_generated"]);
    const history = within(panel()).getByRole("region", { name: "Histórico" });
    expect(history.textContent).toContain("Imagem gerada · T02");
    expect(history.textContent).toContain("web/img/cao.webp (reaproveitada)");
  });
});

describe("Atividade enxuta (095/T05, reference/Main.dc.html)", () => {
  it("uma linha por agente: ponto de estado, nome e ação; sem avatar nem cartão", async () => {
    const { stream, scheduler } = await mount();
    const win = panel();
    const rows = [...win.querySelectorAll<HTMLElement>("[data-agent-card]")];
    expect(rows.every((r) => r.classList.contains("nd-agent-row") && !r.classList.contains("nd-card"))).toBe(true);
    expect(win.querySelector("[data-agent-card] .nd-avatar")).toBeNull();
    const architect = win.querySelector('[data-agent-card="architect"]') as HTMLElement;
    expect(architect.querySelector('.nd-agent-dot[aria-hidden="true"]')?.getAttribute("data-dot")).toBe("idle");
    expect(architect.querySelector(".nd-agent-row-name")?.textContent).toBe("Arquiteto");
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "tool", text: "gerando imagem web/img/gato.webp", code: "tool.generate_image", params: { path: "web/img/gato.webp" } }));
      stream().emit("agent_activity", activity({ agent: "reviewer", task_id: "T01", activity_id: "r1", kind: "text", text: "ajustando o ritmo ao limite do provedor: 12 s", code: "pace.waiting", params: { seconds: "12" } }));
      scheduler.runFrames();
    });
    const dev = win.querySelector('[data-agent-card="developer"]') as HTMLElement;
    expect(dev.querySelector(".nd-agent-dot")?.getAttribute("data-dot")).toBe("cyan");
    expect(dev.querySelector(".nd-agent-row-action")?.textContent).toBe("gerando imagem web/img/gato.webp T02");
    expect(dev.querySelector(".nd-agent-row-task.nd-mono")?.textContent).toBe("T02");
    expect(win.querySelector('[data-agent-card="reviewer"] .nd-agent-dot')?.getAttribute("data-dot")).toBe("amber");
  });

  it("CSS: divisória entre as linhas, nome em 600, ação em --muted; o raio cheio só no ponto", () => {
    const css = readFileSync(resolve(process.cwd(), "src/apps/activity/activity.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    // Regra com exatamente este seletor (não um grupo de seletores que o inclua).
    const rule = (selector: string): string => {
      const i = css.search(new RegExp(`\\}\\s*${selector.replace(/[.[\]"=]/g, "\\$&")} \\{`));
      if (i === -1) throw new Error(`regra ausente: ${selector}`);
      return css.slice(css.indexOf("{", i) + 1, css.indexOf("}", i + 1));
    };
    expect(rule(".nd-agent-row")).toContain("border-bottom: 1px solid var(--divider);");
    expect(rule(".nd-agent-row-name")).toContain("font-weight: var(--fw-semibold);");
    expect(rule(".nd-agent-row-action")).toContain("color: var(--muted);");
    expect(rule(".nd-agent-row-action")).toContain("font: var(--fs-control)/1.45 var(--font-sans);");
    expect(rule(".nd-agent-dot")).toContain("border-radius: var(--r-pill);");
    const pills = [...css.matchAll(/([^{}]+)\{[^}]*--r-pill/g)].map((m) => (m[1] ?? "").trim());
    expect(pills).toEqual([".nd-agent-dot"]);
    // Histórico em linhas com divisória, sem caixa por item.
    expect(rule(".nd-history-item")).toContain("border-bottom: 1px solid var(--divider);");
    expect(rule(".nd-history-item")).not.toContain("background:");
  });
});
