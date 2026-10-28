/*
 * Spec 133 T01: a falha do provedor na Conversa diz o papel, a conexão, o modelo e o motivo pela
 * categoria, nos dois idiomas; os códigos crus do provedor ficam num detalhe recolhível.
 * Spec 133 T02: o cartão traz o papel, a conexão e o modelo (com o cabeçalho do papel afetado) e a
 * mensagem do sistema da parada atual fica só no cartão, com a evidência uma vez só.
 */
import { act, cleanup, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { looksPortuguese } from "../../i18n/ptLeftovers";
import { providerStopText } from "../../i18n/providerEvidence";
import { systemMessageText } from "../../i18n/serverCodes";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const KEY: ConnectionView = { id: "conn", name: "OpenAI", base_url: "https://api.openai.com/v1", api_key_masked: "****" };
const STOPPED = makeSpec("s1", "Plano", "IN_PROGRESS", [makeTask("T03", "A", "IN_PROGRESS")]);

const EVIDENCE = {
  provider: "provider_http_400",
  role: "reviewer",
  model: "gemini-x",
  connection_id: "gem",
  connection_name: "Gemini da Ana",
  connection_kind: "google_gemini",
  task: "T03",
  attempt: 1,
  category: "billing",
  status: 400,
  error_type: "invalid_argument",
  error_code: "credit_low",
  request_id: "req_77",
};

async function open() {
  const view = await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Loja")], states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [STOPPED] }) }, connections: [KEY] });
  await settle();
  return view;
}

function stopWith(code: string, params: Record<string, string | number>): void {
  act(() => {
    appStore.appendMessage("p1", { role: "agent", agent: "system", specId: "s1", text: "provider_http_400", code, params });
  });
}

function card(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-paused-card]");
  if (el === null) throw new Error("cartão ausente");
  return el;
}

/** Papel, conexão e modelo do cartão. */
function facts(): string {
  return card().querySelector("[data-role-facts]")?.textContent ?? "";
}

/** Texto visível sem os detalhes recolhíveis. */
function mainText(el: HTMLElement): string {
  const copy = el.cloneNode(true) as HTMLElement;
  for (const d of copy.querySelectorAll("details")) d.remove();
  return copy.textContent ?? "";
}

describe("falha do provedor com evidência", () => {
  it("o cartão diz papel, conexão, modelo e motivo; os códigos ficam no detalhe, uma vez só", async () => {
    await open();
    stopWith("run.provider_failed", EVIDENCE);
    await settle();
    const article = card();
    const shown = mainText(article);
    expect(shown).toContain("O provedor do modelo falhou: um problema de cobrança no provedor");
    expect(facts()).toContain("Revisor");
    expect(facts()).toContain("Gemini da Ana");
    expect(facts()).toContain("Google Gemini");
    expect(facts()).toContain("gemini-x");
    // O cabeçalho é do papel afetado, com a tarefa.
    expect(article.querySelector(".nd-agent-name")?.textContent).toBe("Revisor");
    expect(article.querySelector("[data-stop-task]")?.textContent).toBe("T03");
    for (const raw of ["provider_http_400", "invalid_argument", "credit_low", "req_77"]) expect(shown).not.toContain(raw);
    const details = within(article).getByText("Detalhes do provedor").closest("details");
    expect(details?.textContent).toContain("invalid_argument");
    expect(details?.textContent).toContain("req_77");
    expect(details?.textContent).toContain("Status HTTP400");
    expect(findForbiddenGlyphs(article)).toEqual([]);
    // A mensagem do sistema da parada fica só no cartão: a evidência aparece uma vez.
    expect(document.querySelector("[data-agent-card='system']")).toBeNull();
    expect(document.querySelectorAll("[data-provider-evidence]")).toHaveLength(1);
  });

  it("em en, sem português", async () => {
    setLanguage("en", { persist: false });
    await open();
    stopWith("run.provider_failed", EVIDENCE);
    await settle();
    const shown = mainText(card());
    expect(shown).toContain("The model provider failed: a billing problem at the provider");
    expect(facts()).toContain("Reviewer");
    expect(facts()).toContain("Gemini da Ana");
    expect(looksPortuguese(shown.replace("Gemini da Ana", ""))).toBe(false);
    expect(within(card()).getByText("Provider details")).toBeTruthy();
    const text = systemMessageText({ text: "x", code: "run.provider_failed", params: EVIDENCE });
    expect(looksPortuguese(text.replace("Gemini da Ana", ""))).toBe(false);
  });

  it("a pausa do limite mantém o texto dela e diz quem parou", async () => {
    await open();
    stopWith("run.spend_limit", { ...EVIDENCE, provider: "provider_spend_limit" });
    await settle();
    const article = card();
    expect(within(article).getByRole("heading", { name: "Teto de gasto atingido" })).toBeTruthy();
    expect(facts()).toContain("Gemini da Ana");
    expect(facts()).toContain("gemini-x");
    const text = systemMessageText({ text: "x", code: "run.spend_limit", params: EVIDENCE });
    expect(text.split("\n")[1]).toBe("Revisor na T03, conexão Gemini da Ana, modelo gemini-x.");
  });

  it("o motivo detalhado da resposta ruim continua na tela, nos dois idiomas, e vai ao detalhe", async () => {
    const detail = { ...EVIDENCE, provider: "provider_bad_response", kind: "image_unreadable", category: "server" };
    await open();
    stopWith("run.provider_failed_detail", detail);
    await settle();
    const shown = mainText(card());
    expect(shown).toContain("O provedor do modelo falhou: o provedor não devolveu uma imagem legível (pode ter cobrado).");
    expect(facts()).toContain("Revisor");
    expect(shown).not.toContain("image_unreadable");
    const details = within(card()).getByText("Detalhes do provedor").closest("details");
    expect(details?.textContent).toContain("Motivoimage_unreadable");
    expect(systemMessageText({ text: "x", code: "run.provider_failed_detail", params: detail })).toBe(
      "O provedor do modelo falhou: o provedor não devolveu uma imagem legível (pode ter cobrado).\nRevisor na T03, conexão Gemini da Ana, modelo gemini-x.",
    );
    const coded = { ...detail, kind: "incomplete", provider_code: "content_filter" };
    expect(systemMessageText({ text: "x", code: "run.provider_failed_detail", params: coded }).split("\n")[0]).toBe(
      "O provedor do modelo falhou: incompleta: content_filter.",
    );
    setLanguage("en", { persist: false });
    expect(systemMessageText({ text: "x", code: "run.provider_failed_detail", params: detail })).toBe(
      "The model provider failed: the provider did not return a readable image (it may have charged).\nReviewer in T03, connection Gemini da Ana, model gemini-x.",
    );
  });

  it("parada antiga, sem papel: a mensagem do sistema continua na tela ao lado do cartão", async () => {
    await open();
    stopWith("run.provider_failed", { provider: "provider_http_400" });
    await settle();
    expect(card()).toBeTruthy();
    const bubble = document.querySelector<HTMLElement>("[data-agent-card='system']");
    expect(bubble).not.toBeNull();
    expect(bubble?.textContent).toContain("provider_http_400");
  });

  it("mensagem antiga, sem papel, continua como era", () => {
    expect(providerStopText("run.provider_failed", { provider: "provider_http_400" }, null)).toBeNull();
    expect(systemMessageText({ text: "x", code: "run.provider_failed", params: { provider: "provider_http_400" } })).toBe(
      "O provedor do modelo falhou (provider_http_400).",
    );
    const noTask = systemMessageText({ text: "x", code: "run.provider_failed", params: { role: "developer", model: "m", category: "nova" } });
    expect(noTask).toBe("O provedor do modelo falhou: uma falha que o provedor não identificou.\nDesenvolvedor, conexão desconhecida, modelo m.");
  });
});
