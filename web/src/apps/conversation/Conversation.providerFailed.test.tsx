/*
 * Spec 133 T11 (D5): a falha do provedor num turno da conversa fica no histórico como mensagem do
 * sistema (`chat.provider_failed`) com a evidência da T01. A tela diz o papel, a conexão, o modelo e
 * o motivo traduzido, nos dois idiomas, e os códigos crus ficam no detalhe recolhível. O 502 com
 * `reason: failure_recorded` recarrega o histórico em vez de repetir o aviso.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { appStore, useAppStore, type ChatMessage } from "../../store";
import { postChat } from "../../api/client";
import type { ApiFailure } from "../../api/client";
import { loadChatHistory } from "../../chatHistory";
import { chatFailureMessage } from "../../chatFailure";
import { setLanguage } from "../../i18n";
import { looksPortuguese } from "../../i18n/ptLeftovers";
import type { ChatHistoryTurn } from "../../api/types";
import { en } from "../../i18n/en";
import { ptBR } from "../../i18n/ptBR";

vi.mock("../../api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../api/client")>()),
  postChat: vi.fn(),
  postChatContinue: vi.fn(),
}));
vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));
vi.mock("../../chatHistory", () => ({ loadChatHistory: vi.fn(async () => {}) }));

const postChatMock = vi.mocked(postChat);
const historyMock = vi.mocked(loadChatHistory);
const NONE: ChatMessage[] = [];
const TURN = "0123456789abcdef0123456789abcdef";

/** Os `params` do servidor para o 404 de um modelo inexistente (chat.rs, `failed_turn`). */
const EVIDENCE = {
  provider: "provider_http_404",
  role: "architect",
  model: "gemini-9-missing",
  connection_id: "gem",
  connection_name: "Gemini da Ana",
  connection_kind: "google_gemini",
  category: "bad_request",
  status: 404,
  error_type: "not_found",
  error_code: "404",
  request_id: "req-404",
};

afterEach(() => {
  cleanup();
  postChatMock.mockReset();
  historyMock.mockClear();
  setLanguage("pt-BR", { persist: false });
});

function Harness({ id }: { id: string }) {
  const messages = useAppStore((s) => s.messages[id] ?? NONE);
  return <Conversation projectId={id} messages={messages} onDraftUpdated={async () => {}} />;
}

function hydrate(id: string, turns: ChatHistoryTurn[]) {
  act(() => appStore.hydrateHistory(id, turns));
}

function systemText(): string {
  return document.querySelector(".nd-system-text")?.textContent ?? "";
}

describe("Conversa: a falha do provedor no turno do Arquiteto (spec 133 T11)", () => {
  it("mostra o papel, a conexão, o modelo e o motivo, nos dois idiomas; os códigos ficam no detalhe", () => {
    const id = "failed-404";
    hydrate(id, [
      { role: "user", text: "oi", ts: 100, turn_id: TURN },
      { role: "system", text: "reserva do servidor", ts: 200, turn_id: TURN, code: "chat.provider_failed", params: EVIDENCE },
    ]);
    render(<Harness id={id} />);
    const pt = systemText();
    expect(pt).toContain("Arquiteto");
    expect(pt).toContain("Gemini da Ana");
    expect(pt).toContain("gemini-9-missing");
    expect(pt).not.toContain("provider_http_404");
    expect(pt).not.toContain("reserva do servidor");
    const details = document.querySelector("[data-provider-evidence]");
    expect(details?.textContent).toContain("provider_http_404");
    expect(details?.textContent).toContain("not_found");
    expect(details?.textContent).toContain("req-404");
    // Sem "Tentar de novo": não é a queda da conexão.
    expect(document.querySelector("[data-retry-turn]")).toBeNull();
    act(() => setLanguage("en", { persist: false }));
    const en = systemText();
    expect(en).toContain("Architect");
    expect(en).toContain("Gemini da Ana");
    expect(looksPortuguese(en.replace("Gemini da Ana", ""))).toBe(false);
  });

  it("o 502 com failure_recorded recarrega o histórico em vez de repetir o aviso", async () => {
    const id = "failed-post";
    postChatMock.mockResolvedValue({
      ok: false,
      error: { kind: "http", status: 502, message: "x", providerCode: "provider_http_404", reason: "failure_recorded", params: EVIDENCE },
    });
    render(<Harness id={id} />);
    fireEvent.change(screen.getByLabelText("Mensagem para o Arquiteto"), { target: { value: "oi" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    });
    expect(historyMock).toHaveBeenCalledWith(id);
    const systems = (appStore.getState().messages[id] ?? []).filter((m) => m.role === "system");
    expect(systems).toHaveLength(0);
  });

  it.each([
    ["provider_plan_needs_login", "providerMsg.planNeedsLogin", ""],
    ["provider_http_429", "providerMsg.throttled", "providerMsg.throttleAdvice"],
  ] as const)("F-01: %s mantém a orientação específica e diz quem falhou, no 502 e na bolha, nos dois idiomas", (provider, key, extra) => {
    const params = { ...EVIDENCE, provider, status: provider === "provider_http_429" ? 429 : 401, category: provider === "provider_http_429" ? "rate" : "auth" };
    const failure: ApiFailure = { kind: "http", status: 502, message: "x", providerCode: provider, params };
    for (const lang of ["pt-BR", "en"] as const) {
      setLanguage(lang, { persist: false });
      const dict = lang === "en" ? en : ptBR;
      const specific = extra === "" ? dict[key] : `${dict[key]} ${dict[extra]}`;
      const fromPost = chatFailureMessage(failure, null);
      expect(fromPost.split("\n")[0]).toBe(specific);
      expect(fromPost).toContain("Gemini da Ana");
      const id = `failed-${provider}-${lang}`;
      hydrate(id, [
        { role: "user", text: "oi", ts: 100, turn_id: TURN },
        { role: "system", text: "reserva do servidor", ts: 200, turn_id: TURN, code: "chat.provider_failed", params },
      ]);
      render(<Harness id={id} />);
      const bubble = systemText();
      expect(bubble).toContain(specific);
      expect(bubble).toContain("Gemini da Ana");
      expect(bubble).toContain(lang === "en" ? "Architect" : "Arquiteto");
      cleanup();
    }
  });

  it("sem o histórico (ex.: o Plano), a mensagem do 502 também diz quem falhou e por quê", () => {
    const failure: ApiFailure = { kind: "http", status: 502, message: "x", providerCode: "provider_http_404", params: EVIDENCE };
    const pt = chatFailureMessage(failure, null);
    expect(pt).toContain("Gemini da Ana");
    expect(pt).not.toContain("provider_http_404");
    setLanguage("en", { persist: false });
    const en = chatFailureMessage(failure, null);
    expect(en).toContain("Architect");
    expect(looksPortuguese(en.replace("Gemini da Ana", ""))).toBe(false);
    // Sem a evidência (servidor antigo), a mensagem de sempre.
    const bare = chatFailureMessage({ kind: "http", status: 502, message: "x", providerCode: "provider_http_404" }, null);
    expect(bare).not.toContain("Architect");
  });
});
