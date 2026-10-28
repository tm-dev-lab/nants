/* Aprovação automática na janela Conversa: o pedido aprovado sozinho vira linha discreta e a rodada autônoma ganha o selo. */
import { act, cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLanguage } from "../../i18n";
import { appStore, useAppStore } from "../../store";
import type { ChatMessage } from "../../store";
import { makeSpec, resetStore } from "../../test-utils";
import { Conversation } from "./Conversation";

vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

const NONE: ChatMessage[] = [];
const SPEC = makeSpec("s1", "Spec", "IN_PROGRESS");

function Harness() {
  const messages = useAppStore((s) => s.messages.p1 ?? NONE);
  return <Conversation projectId="p1" spec={SPEC} messages={messages} onDraftUpdated={async () => {}} />;
}

function live(seq: number, agent: string, text: string): void {
  act(() => {
    appStore.applyEvent({ event_type: "agent_message", project_id: "p1", payload: { agent, text, seq, task_id: "T01", spec_id: "s1" } });
  });
}

beforeEach(() => resetStore());
afterEach(cleanup);

describe("Aprovação automática na Conversa", () => {
  it("aprovação automática vira linha discreta, com o comando em código, em ordem cronológica", () => {
    render(<Harness />);
    live(1, "developer", "antes");
    live(2, "developer", "aprovada automaticamente: npm test");
    live(3, "developer", "depois");
    const line = screen.getByTestId("yolo-auto");
    expect(within(line).getByText("npm test").tagName).toBe("CODE");
    expect(line.textContent).toBe("aprovada automaticamentenpm test");
    const all = document.body.textContent ?? "";
    expect(all.indexOf("antes")).toBeLessThan(all.indexOf("aprovada automaticamente"));
    expect(all.indexOf("aprovada automaticamente")).toBeLessThan(all.indexOf("depois"));
  });

  it("spec 131 T06: a linha com o código vira a pílula no idioma ativo, com o pedido de params.detail", () => {
    setLanguage("en", { persist: false });
    try {
      render(<Harness />);
      act(() => {
        appStore.applyEvent({
          event_type: "agent_message",
          project_id: "p1",
          payload: {
            agent: "developer",
            text: "approved automatically: python --version",
            code: "run.auto_approved",
            params: { detail: "python --version" },
            seq: 1,
            task_id: "T03",
            spec_id: "s1",
          },
        });
      });
      const line = screen.getByTestId("yolo-auto");
      expect(within(line).getByText("python --version").tagName).toBe("CODE");
      expect(line.textContent).toBe("approved automaticallypython --version");
      // A linha gravada antes (texto em pt-BR, sem código) continua reconhecida.
      act(() => {
        appStore.mergeDialog("p1", "s1", [{ seq: 2, ts: 1_700_000_002, agent: "developer", task_id: null, attempt: null, text: "aprovada automaticamente: git status --short" }]);
      });
      const lines = screen.getAllByTestId("yolo-auto");
      expect(lines.map((l) => l.textContent).sort()).toEqual(["approved automaticallygit status --short", "approved automaticallypython --version"]);
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });

  it("reidratação do diálogo mostra a mesma linha", () => {
    render(<Harness />);
    act(() => {
      appStore.mergeDialog("p1", "s1", [
        { seq: 1, ts: 1_700_000_001, agent: "developer", task_id: "T01", attempt: 1, text: "antes" },
        { seq: 2, ts: 1_700_000_002, agent: "developer", task_id: "T01", attempt: 1, text: "aprovado automaticamente (YOLO): npm test" },
        { seq: 3, ts: 1_700_000_003, agent: "developer", task_id: "T01", attempt: 1, text: "depois" },
      ]);
    });
    // Mensagem gravada antes da 095/T08 (prefixo com a sigla): a pílula mostra o texto novo.
    const line = screen.getByTestId("yolo-auto");
    expect(within(line).getByText("npm test")).toBeTruthy();
    expect(line.textContent).toContain("aprovada automaticamente");
    expect(document.body.textContent).not.toContain("YOLO");
    expect(screen.getByText("antes")).toBeTruthy();
    expect(screen.getByText("depois")).toBeTruthy();
  });

  it("rodada autônoma do Arquiteto mostra o selo da aprovação automática", () => {
    render(<Harness />);
    expect(screen.queryByTestId("yolo-badge")).toBeNull();
    act(() => appStore.appendMessage("p1", { role: "architect", agent: "architect", text: "Análise de recuperação #1 (RETRY) [rodada autônoma]\ndecidi seguir" }));
    expect(screen.getByTestId("yolo-badge").textContent).toBe("Aprovação automática");
  });

  it("marca antiga da rodada autônoma: selo e texto novos, sem a sigla", () => {
    render(<Harness />);
    act(() => appStore.appendMessage("p1", { role: "architect", agent: "architect", text: "Análise de recuperação #1 (RETRY) [rodada autônoma YOLO]\ndecidi seguir" }));
    expect(screen.getByTestId("yolo-badge")).toBeTruthy();
    expect(document.body.textContent).toContain("[rodada autônoma]");
    expect(document.body.textContent).not.toContain("YOLO");
  });
});
