/*
 * Pedidos de permissão na linha do tempo da Conversa: no planejamento (Arquiteto) e na execução
 * (Desenvolvedor/Revisor), na posição cronológica, com o selo da tarefa; decididos viram o
 * resultado com o escopo aplicado; chegada ao vivo pelo fluxo aparece sem recarregar.
 */
import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent, SpecView, TaskView } from "../../api/types";
import { appStore, useAppStore } from "../../store";
import type { ChatMessage, PendingPermission, ResolvedPermission } from "../../store";
import { installFetch, resetStore } from "../../test-utils";
import { Conversation } from "./Conversation";

vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

const PID = "p1";
const T0 = 1_700_000_000_000;
const EMPTY_PENDING: readonly PendingPermission[] = [];
const EMPTY_RESOLVED: readonly ResolvedPermission[] = [];
const TASKS: TaskView[] = [
  { id: "T01", title: "Modelo", status: "DONE", attempts: 0, max_attempts: 3, depends_on: [], last_review_verdict: "APPROVED" },
  { id: "T02", title: "Middleware", status: "IN_PROGRESS", attempts: 1, max_attempts: 3, depends_on: ["T01"], last_review_verdict: "NONE" },
];
const SPEC: SpecView = { id: "s1", title: "Plano", status: "IN_PROGRESS", tasks: TASKS };

function pending(requestId: string, taskId: string | null, agent: string, command: string, at = T0): PendingPermission {
  return { requestId, projectId: PID, taskId, agent, command, alwaysAllowed: false, alwaysDeniedCode: "arbitrary", at };
}

function resolvedEntry(requestId: string, taskId: string | null, command: string, applied: "once" | "run" | "deny"): ResolvedPermission {
  return { ...pending(requestId, taskId, "developer", command), applied, resolvedAt: T0 + 1000 };
}

function agentMessage(id: number, agent: string, text: string, taskId?: string, at = T0 - 5000): ChatMessage {
  return { id, role: "agent", agent, text, at, origin: "live", taskId, specId: "s1", dialogSeq: id };
}

/** A Conversa ligada ao store como a janela a monta. */
function Harness({ messages, spec = null }: { messages: ChatMessage[]; spec?: SpecView | null }) {
  const pendingList = useAppStore((s) => s.permissions[PID] ?? EMPTY_PENDING);
  const resolvedList = useAppStore((s) => s.resolvedPermissions[PID] ?? EMPTY_RESOLVED);
  return <Conversation projectId={PID} spec={spec} messages={messages} pending={pendingList} resolved={resolvedList} onDraftUpdated={async () => {}} />;
}

beforeEach(() => {
  resetStore({ phases: { [PID]: "waiting_approval" } });
  installFetch(() => ({ body: { ok: true, applied: "once" } }));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("planejamento: pedido do Arquiteto na conversa", () => {
  it("o pedido aparece como mensagem do Arquiteto, depois da mensagem que o causou", () => {
    const messages: ChatMessage[] = [
      { id: 1, role: "user", text: "crie os testes", at: T0 - 9000, origin: "live", confirmed: true },
      { id: 2, role: "architect", text: "vou verificar o projeto", at: T0 - 8000, origin: "live" },
    ];
    resetStore({ messages: { [PID]: messages }, permissions: { [PID]: [pending("r1", null, "architect", "cat README.md")] } });
    render(<Harness messages={messages} />);
    const items = within(screen.getByRole("list", { name: "Mensagens da conversa" })).getAllByRole("listitem");
    expect(items[1]?.textContent).toContain("vou verificar o projeto");
    expect(items[2]?.textContent).toContain("cat README.md");
    expect(within(items[2] as HTMLElement).getByText("Arquiteto")).toBeTruthy();
    expect(within(items[2] as HTMLElement).getByRole("button", { name: "Permitir uma vez" })).toBeTruthy();
  });

  it("decidir muda a mensagem na conversa para o resultado, sem botões", async () => {
    const user = userEvent.setup();
    resetStore({ permissions: { [PID]: [pending("r1", null, "architect", "cat README.md")] } });
    render(<Harness messages={[]} />);
    await user.click(screen.getByRole("button", { name: "Permitir uma vez" }));
    expect(await screen.findByText(/^Liberado só desta vez/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Permitir uma vez" })).toBeNull();
  });
});

describe("planejamento: resolvidos de tarefas da execução", () => {
  it("resolvido de tarefa da execução não aparece na conversa do planejamento (sem spec ativa)", () => {
    resetStore({ resolvedPermissions: { [PID]: [resolvedEntry("r2", "T02", "node --test", "once")] } });
    render(<Harness messages={[]} />);
    expect(screen.queryByText("node --test")).toBeNull();
  });
});

describe("execução: pedido na conversa única", () => {
  const DIALOG: ChatMessage[] = [agentMessage(1, "developer", "dev do T02", "T02"), agentMessage(2, "developer", "dev do T01", "T01")];

  it("pedido aparece na posição cronológica, depois das mensagens anteriores, com Negar", () => {
    resetStore({ permissions: { [PID]: [pending("r1", "T02", "developer", "node --test", T0 - 1000)] } });
    render(<Harness messages={DIALOG} spec={SPEC} />);
    const items = within(screen.getByRole("list", { name: "Mensagens da conversa" })).getAllByRole("listitem");
    const at = items.findIndex((item) => item.textContent?.includes("node --test"));
    expect(at).toBeGreaterThan(items.findIndex((item) => item.textContent?.includes("dev do T01")));
    expect(within(screen.getByText("node --test").closest("article") as HTMLElement).getByRole("button", { name: "Negar" })).toBeTruthy();
  });

  it("pedido de outra tarefa aparece com o selo da tarefa, junto do diálogo de todas as tarefas", () => {
    resetStore({ permissions: { [PID]: [pending("r1", "T01", "reviewer", "make test")] } });
    render(<Harness messages={DIALOG} spec={SPEC} />);
    const card = screen.getByText("make test").closest("article") as HTMLElement;
    expect(within(card).getByText("T01")).toBeTruthy();
    expect(screen.getByText("dev do T01")).toBeTruthy();
    expect(screen.getByText("dev do T02")).toBeTruthy();
  });

  it("resolvido nesta sessão continua visível e mostra o escopo aplicado", () => {
    resetStore({ phases: { [PID]: "executing" }, resolvedPermissions: { [PID]: [resolvedEntry("r0", "T01", "git status", "run")] } });
    render(<Harness messages={DIALOG} spec={SPEC} />);
    expect(screen.getByText("git status")).toBeTruthy();
    expect(screen.getByText(/^Liberado nesta execução/)).toBeTruthy();
  });

  it("chegada ao vivo pelo fluxo aparece sem recarregar", () => {
    resetStore({ phases: { [PID]: "executing" } });
    render(<Harness messages={DIALOG} spec={SPEC} />);
    expect(screen.queryByRole("group", { name: "Decisão sobre o comando" })).toBeNull();
    const event = { event_type: "permission_requested", project_id: PID, payload: { request_id: "r5", project_id: PID, task_id: "T02", agent: "developer", command: "npm run build", always_allowed: false } } as unknown as ApiEvent;
    act(() => appStore.applyEvent(event));
    expect(screen.getByText("npm run build")).toBeTruthy();
    expect(screen.getByRole("group", { name: "Decisão sobre o comando" })).toBeTruthy();
  });

  it("enquanto o Arquiteto pensa com um pedido pendente, o aviso diz que ele espera a decisão", () => {
    resetStore({ permissions: { [PID]: [pending("r1", null, "architect", "ls")] } });
    act(() => appStore.beginArchitectTurn(PID, { turnId: "t1", startedAt: Date.now() }));
    render(<Harness messages={[]} />);
    expect(screen.getByText("O Arquiteto está pensando, aguardando sua decisão de permissão")).toBeTruthy();
  });
});
