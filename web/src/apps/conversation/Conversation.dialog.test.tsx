/*
 * A conversa única da janela Conversa durante a execução: o diálogo de todas as tarefas, sistema,
 * mensagens sem tarefa e o usuário, em ordem cronológica; selo da tarefa e da tentativa; agentes em
 * Markdown restrito; identidade de spec (outra spec nunca aparece; linha sem spec só na spec da
 * chegada) e a nota discreta quando o diálogo não carrega.
 */
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SpecView, TaskView } from "../../api/types";
import type { ChatMessage } from "../../store";
import { Conversation } from "./Conversation";

vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

afterEach(cleanup);

const TASKS: TaskView[] = [
  { id: "T01", title: "Modelo de usuário", status: "DONE", attempts: 0, max_attempts: 3, depends_on: [], last_review_verdict: "APPROVED" },
  { id: "T02", title: "Middleware de autenticação", status: "IN_PROGRESS", attempts: 1, max_attempts: 3, depends_on: ["T01"], last_review_verdict: "NONE" },
];
const AT = Date.now();
const DIALOG: ChatMessage[] = [
  { id: 1, role: "agent", agent: "developer", text: "dev do T01", at: AT, origin: "history", dialogSeq: 1, taskId: "T01", attempt: 1 },
  { id: 2, role: "agent", agent: "developer", text: "dev do T02", at: AT, origin: "history", dialogSeq: 2, taskId: "T02", attempt: 1 },
  { id: 3, role: "agent", agent: "tests", text: "make test falhou", at: AT, origin: "history", dialogSeq: 3, taskId: "T02", attempt: 1 },
  { id: 4, role: "agent", agent: "system", text: "sistema sem tarefa", at: AT, origin: "history", dialogSeq: 4 },
  { id: 5, role: "agent", agent: "reviewer", text: "mensagem sem tarefa", at: AT, origin: "history" },
  { id: 6, role: "user", text: "pergunta do usuário", at: AT },
];

const specOf = (id = "s1"): SpecView => ({ id, title: "Plano", status: "IN_PROGRESS", tasks: TASKS });

function view(messages: readonly ChatMessage[], extra: { spec?: SpecView; dialogError?: boolean; onRetryDialog?: () => void } = {}) {
  return render(<Conversation projectId="p1" spec={extra.spec ?? specOf()} messages={messages} onDraftUpdated={async () => {}} {...extra} />);
}

function logTexts(): string[] {
  return within(screen.getByRole("list", { name: "Mensagens da conversa" }))
    .getAllByRole("listitem")
    .map((li) => li.textContent ?? "");
}

describe("conversa única: todas as tarefas, sem filtro", () => {
  it("mostra o diálogo de todas as tarefas, de sistema, sem tarefa e o usuário", () => {
    view(DIALOG);
    const texts = logTexts().join("|");
    for (const text of ["dev do T01", "dev do T02", "make test falhou", "sistema sem tarefa", "mensagem sem tarefa", "pergunta do usuário"]) {
      expect(texts).toContain(text);
    }
  });

  it("sem mensagens, a conversa continua com o campo do Arquiteto", () => {
    view([]);
    expect(screen.getByLabelText("Mensagem para o Arquiteto")).toBeTruthy();
  });

  it("botão Enviar: ícone com aria-label (sem texto), inativo sem texto e ativo com texto", () => {
    view([]);
    const button = screen.getByRole("button", { name: "Enviar" }) as HTMLButtonElement;
    expect(button.textContent).toBe("");
    expect(button.querySelector("svg")).not.toBeNull();
    expect(button.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Mensagem para o Arquiteto"), { target: { value: "oi" } });
    expect(button.disabled).toBe(false);
  });

  it("selo da tarefa e da tentativa quando maior que 1; consecutivas da mesma fonte agrupam", () => {
    const at = Date.now();
    view([
      { id: 1, role: "agent", agent: "developer", text: "primeira", at, taskId: "T02", attempt: 2, specId: "s1" },
      { id: 2, role: "agent", agent: "developer", text: "segunda", at: at + 1, taskId: "T02", attempt: 2, specId: "s1" },
      { id: 3, role: "agent", agent: "reviewer", text: "outra tarefa", at: at + 2, taskId: "T01", attempt: 1, specId: "s1" },
    ]);
    const list = screen.getByRole("list", { name: "Mensagens da conversa" });
    expect(within(list).getByText("T02 · tentativa 2")).toBeTruthy();
    expect(within(list).getAllByText("Desenvolvedor")).toHaveLength(1);
    expect(within(list).getByText("T01")).toBeTruthy();
    expect(within(list).getByText("segunda")).toBeTruthy();
  });

  it("ordena por instante, intercalando usuário, agentes e Arquiteto (Markdown sem os marcadores)", () => {
    const at = 1_000_000;
    view([
      { id: 1, role: "agent", agent: "developer", text: "B-agente", at: at + 20, taskId: "T02", specId: "s1" },
      { id: 2, role: "user", text: "A-usuario", at: at + 10 },
      { id: 3, role: "architect", agent: "architect", text: "C-arquiteto **forte**", at: at + 30 },
    ]);
    const texts = logTexts();
    expect(texts[0]).toContain("A-usuario");
    expect(texts[1]).toContain("B-agente");
    expect(texts[2]).toContain("C-arquiteto");
    expect(texts[2]).not.toContain("**");
  });
});

describe("avatares e texto", () => {
  it("mensagem do Arquiteto tem o avatar do papel e o nome", () => {
    const { container } = view([{ id: 1, role: "user", text: "oi", at: AT }, { id: 2, role: "architect", agent: "architect", text: "olá, vamos planejar", at: AT + 1 }]);
    expect(container.querySelector('[data-kind="architect"]')).not.toBeNull();
    expect(within(screen.getByRole("list", { name: "Mensagens da conversa" })).getByText("Arquiteto")).toBeTruthy();
  });

  it("mensagem de sistema tem avatar decorativo, o nome só para leitores de tela e o texto em role=status", () => {
    const { container } = view([{ id: 1, role: "system", text: "Falha ao falar com o provedor", at: AT }]);
    expect(container.querySelector('[data-kind="system"]')?.getAttribute("aria-hidden")).toBe("true");
    expect(screen.getByRole("status", { name: "" }).textContent).toBe("Falha ao falar com o provedor");
    expect(screen.getByText("Sistema:", { exact: false }).className).toContain("nd-sr-only");
  });

  it("texto do usuário e do Arquiteto nunca vira HTML", () => {
    const payload = "<img src=x onerror=alert(1)>";
    const { container } = view([{ id: 1, role: "user", text: payload, at: AT }, { id: 2, role: "architect", agent: "architect", text: payload, at: AT + 1 }]);
    expect(screen.getAllByText(payload)).toHaveLength(2);
    expect(container.querySelector("img")).toBeNull();
  });
});

describe("texto dos agentes", () => {
  it("inline code e bloco de código viram Markdown restrito, sem HTML", () => {
    const text = "rode `node --test` <b>x</b>\n\n```sh\nmake test\n```";
    const { container } = view([{ id: 9, role: "agent", agent: "developer", text, at: AT, dialogSeq: 9, taskId: "T02", specId: "s1" }]);
    expect(screen.getByText("node --test").tagName).toBe("CODE");
    expect(container.querySelector("b")).toBeNull();
    expect(screen.getByText("make test").tagName).toBe("CODE");
    expect(container.textContent).not.toContain("`");
  });

  it("o agente 'tests' tem rótulo próprio", () => {
    view(DIALOG);
    expect(within(screen.getByRole("list", { name: "Mensagens da conversa" })).getByText("Testes")).toBeTruthy();
  });
});

describe("identidade de spec", () => {
  it("mensagens da spec C não aparecem na spec D, mesmo com os mesmos ids de tarefa", () => {
    view([{ id: 1, role: "agent", agent: "developer", text: "de C T02", at: AT, dialogSeq: 1, taskId: "T02", specId: "C" }], { spec: specOf("D") });
    expect(screen.queryByText("de C T02")).toBeNull();
  });

  const unscoped: ChatMessage[] = [
    { id: 1, role: "agent", agent: "developer", text: "sem dono T02", at: AT, unscoped: true, arrivedUnderSpec: "s1", taskId: "T02" },
    { id: 2, role: "agent", agent: "developer", text: "sem spec nenhuma T02", at: AT, unscoped: true, taskId: "T02" },
  ];

  it("sem spec_id e sem estado, a linha não aparece em nenhuma spec", () => {
    view(unscoped);
    expect(screen.queryByText("sem spec nenhuma T02")).toBeNull();
  });

  it("linha sem spec aparece só na spec que era ativa na chegada, nunca em outra", () => {
    const { unmount } = view(unscoped);
    expect(screen.getByText("sem dono T02")).toBeTruthy();
    unmount();
    view(unscoped, { spec: specOf("s2") });
    expect(screen.queryByText("sem dono T02")).toBeNull();
  });
});

describe("falha discreta ao carregar o diálogo", () => {
  it("mostra a nota fixa, mantém o que já está na tela e 'Tentar de novo' chama o retry", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    view(DIALOG, { dialogError: true, onRetryDialog: onRetry });
    expect(screen.getByText("Não foi possível carregar o diálogo.")).toBeTruthy();
    expect(logTexts().join("|")).toContain("dev do T02");
    await user.click(screen.getByRole("button", { name: "Tentar de novo" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("sem erro, a nota não aparece", () => {
    view(DIALOG);
    expect(screen.queryByText("Não foi possível carregar o diálogo.")).toBeNull();
  });
});
