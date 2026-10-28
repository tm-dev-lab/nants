/*
 * "Precisa de você" (spec 093, decisão 10; T06): a regra de contagem, o painel do sino por
 * projeto (todos os projetos, via `/api/attention`), o caso "recarregar com a permissão pendente
 * em B estando em A", o aviso flutuante (empilhar, resolver em outro lugar, sem roubar o foco), os
 * selos e o título/favicon.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Attention, AttentionProject } from "../../desktop/api";
import { applyAttention } from "../../desktop/attention";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { liveNow } from "../../liveClock";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import {
  ACTION_KINDS,
  attentionGroups,
  attentionTotal,
  countsTowardAttention,
  INFO_KINDS,
  projectItems,
} from "./attentionModel";
import { MAX_PERMISSION_TOASTS, stackToasts } from "./PermissionToasts";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  applyAttention(0);
  setLanguage("pt-BR", { persist: false });
});

const P1 = makeProject("p1", "Loja");
const P2 = makeProject("p2", "Blog");

function entry(id: string, name: string, extra: Partial<AttentionProject> = {}): AttentionProject {
  return { project_id: id, name, pending_permissions: [], pending_choices: false, plan_awaiting_approval: false, paused_or_stalled: null, ...extra };
}

function bell(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-bell]");
  if (el === null) throw new Error("sino ausente");
  return el;
}

function panel(): HTMLElement {
  return screen.getByRole("region", { name: "Precisa de você" });
}

function permissionEvent(projectId: string, requestId: string, command: string, taskId: string | null = "T01") {
  return {
    event_type: "permission_requested",
    project_id: projectId,
    payload: { request_id: requestId, project_id: projectId, task_id: taskId, agent: "developer", command, always_allowed: false },
  };
}

function resolvedEvent(projectId: string, requestId: string) {
  return { event_type: "permission_resolved", project_id: projectId, payload: { request_id: requestId, project_id: projectId, applied: "once" } };
}

describe("regra de contagem (modelo puro)", () => {
  it("só o que exige ação conta; os informativos nunca", () => {
    for (const kind of ACTION_KINDS) expect(countsTowardAttention(kind)).toBe(true);
    for (const kind of INFO_KINDS) expect(countsTowardAttention(kind)).toBe(false);
  });

  it("itens por projeto, total com o setup e o espaço ativo primeiro", () => {
    const attention: Attention = {
      setup_incomplete: true,
      projects: [
        entry("p2", "Blog", {
          pending_permissions: [{ request_id: "r1", agent: "developer", kind: "command", always_allowed: false }],
          pending_choices: true,
          paused_or_stalled: { kind: "paused", spec_id: "s1", task_id: "T02", escalation_options: [{ id: "o1", kind: "guidance" }] },
        }),
        entry("p1", "Loja", { plan_awaiting_approval: true }),
        entry("p3", "Vazio"),
      ],
    };
    expect(attentionTotal(attention)).toBe(5);
    const groups = attentionGroups(attention, "p1");
    expect(groups.map((g) => g.projectId)).toEqual(["p1", "p2"]);
    expect(groups[1]?.items.map((i) => i.kind)).toEqual(["permission", "choices", "paused"]);
    expect(projectItems(entry("x", "X", { paused_or_stalled: { kind: "stalled", spec_id: "s9" } }))[0]).toMatchObject({ kind: "stalled", specId: "s9" });
  });

  it("aviso flutuante: até 3, os mais recentes, e quantos ficaram de fora", () => {
    expect(stackToasts([1, 2, 3, 4, 5])).toEqual({ shown: [3, 4, 5], hidden: 2 });
    expect(stackToasts([1])).toEqual({ shown: [1], hidden: 0 });
    expect(MAX_PERMISSION_TOASTS).toBe(3);
  });
});

describe("painel do sino", () => {
  it("recarregar com a permissão pendente em B estando em A: sino = 1, com B no painel e as decisões", async () => {
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: {
        p1: makeProjectState({ id: "p1", name: "Loja" }),
        p2: makeProjectState({
          id: "p2",
          name: "Blog",
          pending_permissions: [{ request_id: "rb", agent: "developer", command: "npm install three", task_id: "T03", always_allowed: false }],
        }),
      },
      attention: {
        setup_incomplete: false,
        projects: [entry("p2", "Blog", { pending_permissions: [{ request_id: "rb", agent: "developer", kind: "command", task_id: "T03", always_allowed: false }] })],
      },
    });
    expect(bell().getAttribute("aria-label")).toBe("Precisa de você, 1 pendência");
    expect(bell().querySelector("[data-badge]")?.textContent).toBe("1");
    expect(document.title).toBe("(1) Nants · aguardando você");
    fireEvent.click(bell());
    await settle();
    const region = panel();
    const group = region.querySelector<HTMLElement>("[data-attention-project='p2']");
    if (group === null) throw new Error("grupo de B ausente");
    expect(within(group).getByRole("heading", { name: "Blog" })).toBeTruthy();
    // O estado de B é carregado para mostrar o comando e as decisões honestas.
    expect(within(group).getByText("npm install three")).toBeTruthy();
    expect(within(group).getByRole("button", { name: "Permitir uma vez" })).toBeTruthy();
    expect(within(group).getByRole("button", { name: "Permitir nesta execução" })).toBeTruthy();
    expect(within(group).queryByRole("button", { name: "Sempre permitir" })).toBeNull();
    server.reply("POST", /\/api\/projects\/p2\/permissions\/rb$/, () => ({ status: 200, body: { ok: true, applied: "deny" } }));
    await act(async () => {
      fireEvent.click(within(group).getByRole("button", { name: "Negar" }));
    });
    await settle();
    expect(server.calls.find((c) => c.method === "POST" && c.url === "/api/projects/p2/permissions/rb")?.body).toEqual({ decision: "deny" });
    expect(findForbiddenGlyphs(region)).toEqual([]);
  });

  it("agrupa por projeto com ações diretas: perguntas, plano, pausa (escalada e Retomar) e setup", async () => {
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: {
        p1: makeProjectState({ id: "p1", name: "Loja" }),
        p2: makeProjectState({
          id: "p2",
          name: "Blog",
          specs: [
            makeSpec("s1", "Blog", "IN_PROGRESS", [
              makeTask("T02", "Posts", "BLOCKED", { recovery_options: [{ id: "o1", kind: "guidance", label: "Usar SQLite" }], recovery_options_analysis: 4 } as never),
            ]),
          ],
        }),
      },
      attention: {
        setup_incomplete: true,
        projects: [
          entry("p1", "Loja", { pending_choices: true, plan_awaiting_approval: true }),
          entry("p2", "Blog", { paused_or_stalled: { kind: "paused", spec_id: "s1", task_id: "T02", escalation_options: [{ id: "o1", kind: "guidance" }] } }),
        ],
      },
    });
    fireEvent.click(bell());
    await settle();
    const region = panel();
    expect(region.querySelector("[data-attention-item='setup']")).not.toBeNull();
    const loja = region.querySelector<HTMLElement>("[data-attention-project='p1']");
    const blog = region.querySelector<HTMLElement>("[data-attention-project='p2']");
    if (loja === null || blog === null) throw new Error("grupos ausentes");
    expect(loja.textContent).toContain("(este espaço)");
    expect(within(loja).getByRole("button", { name: "Responder na Conversa" })).toBeTruthy();
    expect(within(blog).getByRole("button", { name: "Abrir o projeto Blog" })).toBeTruthy();
    // Escolha de escalada com confirmação.
    server.reply("POST", /\/api\/projects\/p2\/specs\/s1\/tasks\/T02\/recovery-choice$/, () => ({ status: 202, body: { ok: true } }));
    fireEvent.click(within(blog).getByRole("button", { name: "Escolher: Usar SQLite" }));
    await act(async () => {
      fireEvent.click(within(blog).getByRole("button", { name: "Aplicar e retomar" }));
    });
    await settle();
    expect(server.calls.find((c) => c.method === "POST" && /recovery-choice$/.test(c.url))?.body).toEqual({ option_id: "o1", analysis: 4 });
    // Abrir o Plano do espaço ativo pelo painel: fecha o painel e foca a janela.
    fireEvent.click(within(loja).getByRole("button", { name: "Abrir Plano" }));
    await settle();
    expect(screen.queryByRole("region", { name: "Precisa de você" })).toBeNull();
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
  });

  it("execução parada por limite de conta: o sino diz o papel e o motivo (spec 133 T02)", async () => {
    const stalled = (extra: Partial<Extract<AttentionProject["paused_or_stalled"], { kind: "stalled" }>>) =>
      entry("p1", "Loja", { paused_or_stalled: { kind: "stalled", spec_id: "s1", ...extra } });
    await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [makeSpec("s1", "Loja", "IN_PROGRESS", [makeTask("T02", "Carrinho", "IN_PROGRESS")])] }) },
      attention: { setup_incomplete: false, projects: [stalled({ stop_code: "run.billing", role: "developer" })] },
    });
    fireEvent.click(bell());
    await settle();
    const item = panel().querySelector<HTMLElement>("[data-attention-item='stalled']");
    expect(item?.textContent).toContain("Desenvolvedor pausado: sem crédito no provedor");
    expect(findForbiddenGlyphs(panel())).toEqual([]);
    // Sem papel (parada antiga ou que não é do provedor): o título de sempre.
    expect(projectItems(stalled({}))[0]).toMatchObject({ kind: "stalled", stopCode: null, role: null });
    setLanguage("en", { persist: false });
    await settle();
    expect(document.querySelector("[data-attention-item='stalled']")?.textContent).toContain("Developer paused: out of credit at the provider");
  });

  it("Esc fecha e devolve o foco ao sino; sem nada, a frase de vazio", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    fireEvent.click(bell());
    await settle();
    expect(panel().textContent).toContain("Nada precisa de você agora.");
    fireEvent.keyDown(panel(), { key: "Escape" });
    await settle();
    expect(screen.queryByRole("region", { name: "Precisa de você" })).toBeNull();
    expect(document.activeElement).toBe(bell());
  });

  it("a ação em outro projeto troca de espaço e abre a janela quando ele fica pronto", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: {
        p1: makeProjectState({ id: "p1", name: "Loja" }),
        p2: makeProjectState({ id: "p2", name: "Blog", specs: [makeSpec("s1", "Blog", "IN_PROGRESS", [makeTask("T01", "A", "IN_PROGRESS")])] }),
      },
      attention: { setup_incomplete: false, projects: [entry("p2", "Blog", { pending_choices: true })] },
    });
    fireEvent.click(bell());
    await settle();
    fireEvent.click(within(panel()).getByRole("button", { name: "Responder na Conversa" }));
    await settle(40);
    expect(window.location.search).toBe("?space=p2");
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
  });
});

describe("aviso flutuante de permissão", () => {
  it("empilha até 3 com '+N', role=alert, não rouba o foco e some quando resolvido em outro lugar", async () => {
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    });
    const field = screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" });
    field.focus();
    // A ordem dos avisos é a de chegada, mesmo com o relógio de parede recuando no meio (no WSL,
    // ~2 s a cada ~32 s): o relógio da linha do tempo (114/T01) repete a última hora e o
    // desempate é a ordem de chegada (114/T04). O recuo é simulado de propósito: a hora de
    // parede fica 2 s abaixo da última hora já devolvida, e o teste não depende do relógio real.
    liveNow();
    let clock = Date.now() - 2_000;
    vi.spyOn(Date, "now").mockImplementation(() => (clock += 10));
    await act(async () => {
      stream().emit("permission_requested", permissionEvent("p1", "r1", "cargo test"));
      stream().emit("permission_requested", permissionEvent("p2", "r2", "npm install"));
      stream().emit("permission_requested", permissionEvent("p1", "r3", "ls -la"));
      stream().emit("permission_requested", permissionEvent("p2", "r4", "git status"));
    });
    await settle();
    const toasts = Array.from(document.querySelectorAll<HTMLElement>("[data-permission-toast]"));
    expect(toasts.map((t) => t.getAttribute("data-permission-toast"))).toEqual(["r2", "r3", "r4"]);
    for (const toast of toasts) expect(toast.getAttribute("role")).toBe("alert");
    expect(document.querySelector("[data-toast-more]")?.textContent).toContain("1 outro pedido");
    expect(toasts[0]?.textContent).toContain("Blog");
    expect(document.activeElement).toBe(field);
    expect(findForbiddenGlyphs(document.querySelector("[data-permission-toasts]") as HTMLElement)).toEqual([]);

    // Resolvido em outro lugar (outra aba): o aviso some e o da fila entra.
    await act(async () => {
      stream().emit("permission_resolved", resolvedEvent("p2", "r4"));
    });
    await settle();
    expect(Array.from(document.querySelectorAll("[data-permission-toast]")).map((t) => t.getAttribute("data-permission-toast"))).toEqual(["r1", "r2", "r3"]);
    expect(document.querySelector("[data-toast-more]")).toBeNull();
    expect(document.activeElement).toBe(field);
  });

  it("decidir no aviso resolve o pedido (some do aviso e da Conversa)", async () => {
    const { stream, server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    await act(async () => {
      stream().emit("permission_requested", permissionEvent("p1", "r1", "cargo test", null));
    });
    await settle();
    server.reply("POST", /\/api\/projects\/p1\/permissions\/r1$/, () => ({ status: 200, body: { ok: true, applied: "once" } }));
    const toast = document.querySelector<HTMLElement>("[data-permission-toast='r1']");
    if (toast === null) throw new Error("aviso ausente");
    await act(async () => {
      fireEvent.click(within(toast).getByRole("button", { name: "Permitir uma vez" }));
    });
    await settle();
    expect(document.querySelector("[data-permission-toast]")).toBeNull();
    const conversation = screen.getByRole("region", { name: "Conversa" });
    expect(within(conversation).queryByRole("group", { name: "Decisão sobre o comando" })).toBeNull();
    expect(within(conversation).getByText(/^Liberado só desta vez/)).toBeTruthy();
  });

  it("'Ir para a conversa' de outro projeto troca de espaço e abre a Conversa", async () => {
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }), p2: makeProjectState({ id: "p2", name: "Blog" }) },
    });
    await act(async () => {
      stream().emit("permission_requested", permissionEvent("p2", "r2", "npm install"));
    });
    await settle();
    const toast = document.querySelector<HTMLElement>("[data-permission-toast='r2']");
    if (toast === null) throw new Error("aviso ausente");
    fireEvent.click(within(toast).getByRole("button", { name: "Ir para a conversa" }));
    await settle(40);
    expect(window.location.search).toBe("?space=p2");
    await waitFor(() => expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy());
  });
});

describe("painel do sino em en (spec 102 T06)", () => {
  it("todos os itens, as ações e os rótulos acessíveis sem português; o título da aba também", async () => {
    setLanguage("en", { persist: false });
    await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Shop"), makeProject("p2", "Notes"), makeProject("p3", "Board")],
      states: {
        p1: makeProjectState({ id: "p1", name: "Shop", pending_permissions: [{ request_id: "r1", agent: "developer", command: "npm ci", task_id: "T01", always_allowed: false }] }),
        p2: makeProjectState({
          id: "p2",
          name: "Notes",
          specs: [
            makeSpec("s1", "Notes", "IN_PROGRESS", [
              makeTask("T02", "Posts", "BLOCKED", { recovery_options: [{ id: "o1", kind: "guidance", label: "Use SQLite" }], recovery_options_analysis: 4 } as never),
            ]),
          ],
        }),
        p3: makeProjectState({ id: "p3", name: "Board" }),
      },
      attention: {
        setup_incomplete: true,
        projects: [
          entry("p1", "Shop", {
            pending_permissions: [{ request_id: "r1", agent: "developer", kind: "command", task_id: "T01", always_allowed: false }],
            pending_choices: true,
            plan_awaiting_approval: true,
          }),
          entry("p2", "Notes", { paused_or_stalled: { kind: "paused", spec_id: "s1", task_id: "T02", escalation_options: [{ id: "o1", kind: "guidance" }] } }),
          entry("p3", "Board", { paused_or_stalled: { kind: "stalled", spec_id: "s9" } }),
        ],
      },
    });
    expect(document.title).toBe("(6) Nants · waiting for you");
    fireEvent.click(bell());
    await settle();
    const region = screen.getByRole("region", { name: "Needs you" });
    expect(within(region).getByRole("heading", { name: "Needs you" })).toBeTruthy();
    expect(region.textContent).toContain("6 pending items");
    expect(region.textContent).toContain("Setup incomplete");
    expect(within(region).getByRole("button", { name: "Open Getting started" })).toBeTruthy();
    expect(region.textContent).toContain("(this space)");
    expect(region.textContent).toContain("Permission · Developer · T01");
    expect(within(region).getByRole("button", { name: "Answer in Conversation" })).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Open Plan" })).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Open the project Notes" })).toBeTruthy();
    expect(region.textContent).toContain("Run paused · T02");
    expect(within(region).getByRole("button", { name: "Choose: Use SQLite" })).toBeTruthy();
    expect(within(region).getByRole("list", { name: "Escalation options" })).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Resume" })).toBeTruthy();
    expect(region.textContent).toContain("Run stopped");
    expect(within(region).getByRole("button", { name: "Continue" })).toBeTruthy();
    expect(within(region).getAllByRole("button", { name: "View in the conversation" }).length).toBe(2);
    expect(within(region).getByRole("button", { name: "Close" })).toBeTruthy();
    fireEvent.click(within(region).getByRole("button", { name: "Choose: Use SQLite" }));
    expect(within(region).getByRole("button", { name: "Apply and resume" })).toBeTruthy();
    expect(within(region).getByRole("button", { name: "Cancel" })).toBeTruthy();
    expect(portugueseLeftovers(region)).toEqual([]);
  });
});

describe("título e favicon", () => {
  it("com pendências o título e o favicon mudam; com zero voltam ao original", () => {
    document.head.replaceChildren();
    const link = document.createElement("link");
    link.setAttribute("rel", "icon");
    link.setAttribute("href", "/brand/favicon.svg");
    document.head.append(link);
    document.title = "Nants";
    applyAttention(2);
    expect(document.title).toBe("(2) Nants · aguardando você");
    expect(link.getAttribute("href")?.startsWith("data:image/svg+xml,")).toBe(true);
    expect(link.getAttribute("type")).toBe("image/svg+xml");
    applyAttention(0);
    expect(document.title).toBe("Nants");
    expect(link.getAttribute("href")).toBe("/brand/favicon.svg");
    expect(link.getAttribute("type")).toBeNull();
  });
});
