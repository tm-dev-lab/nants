import { afterEach, describe, expect, it, vi } from "vitest";
import { getConfig, getRequestLanguage, LANG_HEADER, postChat } from "../api/client";
import { isMessageCode, parseCodeParams } from "../api/codes";
import { parseApiEvent } from "../api/events";
import { setLanguage } from "./index";
import { apiFailureText, noticeText, systemMessageText, translateCode } from "./serverCodes";
import { en } from "./en";
import { ptBR } from "./ptBR";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

function stubFetch(status: number, body: unknown) {
  vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(status, body)));
}

afterEach(() => {
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false, storage: null });
});

/** Códigos que o servidor envia (spec 102 T01): cada um tem chave nos dois dicionários. */
const SERVER_ERROR_CODES = [
  "host_forbidden",
  "origin_forbidden",
  "content_type_json",
  "request_too_large",
  "http_error",
  "app_port_in_use",
  "view_ports_exhausted",
  "app_port_remapped",
  "prereq_missing",
  "provider_failed",
];
const SYSTEM_CODES = [
  "run.stopped",
  "run.failed",
  "run.failed_cause",
  "run.provider_failed",
  "run.provider_failed_detail",
  "run.relaunch_failed",
  "run.docker_migration",
  "run.yolo_ignored",
  "run.plan_limit",
  "run.spend_limit",
  "run.daily_quota",
  "run.billing",
  "image.unavailable",
  "image.failed",
  "image.suspended",
];

describe("códigos do servidor", () => {
  it("todo code do servidor tem texto nos dois idiomas", () => {
    for (const code of SERVER_ERROR_CODES) {
      expect(Object.keys(ptBR), code).toContain(`serverError.${code}`);
      expect(Object.keys(en), code).toContain(`serverError.${code}`);
      expect(isMessageCode(code)).toBe(true);
    }
    for (const code of SYSTEM_CODES) {
      expect(Object.keys(ptBR), code).toContain(`system.${code}`);
      expect(isMessageCode(code)).toBe(true);
    }
  });

  it("formato do code e dos params", () => {
    expect(isMessageCode("Run")).toBe(false);
    expect(isMessageCode("1x")).toBe(false);
    expect(isMessageCode("a".repeat(65))).toBe(false);
    expect(parseCodeParams({ n: 1, s: "x", bad: { x: 1 }, "Bad-Name": "y", nil: null })).toEqual({ n: 1, s: "x" });
    expect(parseCodeParams([1])).toBeNull();
  });

  it("traduz pelo code e cai para null sem chave ou sem parâmetro", () => {
    expect(translateCode("serverError", "http_error", { status: 405 }, "en")).toBe("The server answered with an error (HTTP 405).");
    expect(translateCode("serverError", "http_error", {}, "en")).toBeNull();
    expect(translateCode("serverError", "nao_existe", {}, "en")).toBeNull();
  });
});

describe("erros da API no idioma ativo (en)", () => {
  it("code traduzido vence a mensagem do servidor", async () => {
    stubFetch(403, { error: "Origin não permitida", code: "origin_forbidden" });
    const r = await getConfig();
    if (r.ok) throw new Error("esperava falha");
    expect(r.error.code).toBe("origin_forbidden");
    expect(r.error.message).toBe("operação não permitida");
    expect(apiFailureText(r.error, "en")).toBe(
      "Request from another origin refused. Open Nants at the address shown in the terminal.",
    );
    expect(apiFailureText(r.error, "pt-BR")).toContain("Pedido de outra origem recusado");
  });

  it("413 do finalize e params numéricos", async () => {
    stubFetch(413, { error: "Payload Too Large", code: "request_too_large" });
    const r = await getConfig();
    if (r.ok) throw new Error("esperava falha");
    setLanguage("en", { persist: false, storage: null });
    expect(apiFailureText(r.error)).toBe("The request is too large.");
  });

  it("o code traduz nos dois idiomas; o texto do servidor nunca é casado (spec 115 T02)", async () => {
    stubFetch(404, { error: "projeto não encontrado", code: "project_not_found" });
    const r = await getConfig();
    if (r.ok) throw new Error("esperava falha");
    expect(r.error.code).toBe("project_not_found");
    expect(r.error.message).toBe("recurso não encontrado");
    expect(apiFailureText(r.error, "en")).toBe("project not found");
    expect(apiFailureText(r.error, "pt-BR")).toBe("projeto não encontrado");
    // Sem code, o texto conhecido do servidor não vale mais: sai a reserva por status.
    stubFetch(404, { error: "projeto não encontrado" });
    const bare = await getConfig();
    if (bare.ok) throw new Error("esperava falha");
    expect(bare.error.messageKey).toBe("api.status.404");
    expect(apiFailureText(bare.error, "en")).toBe(en["api.status.404"]);
  });

  it("sem code nem mensagem conhecida: texto de reserva por status no idioma ativo", async () => {
    stubFetch(422, { error: "texto qualquer" });
    const r = await getConfig();
    if (r.ok) throw new Error("esperava falha");
    expect(r.error.message).toBe("dados inválidos");
    expect(r.error.messageKey).toBe("api.status.422");
    setLanguage("en", { persist: false, storage: null });
    expect(apiFailureText(r.error)).toBe("invalid data");
    setLanguage("pt-BR", { persist: false, storage: null });
    expect(apiFailureText(r.error)).toBe("dados inválidos");
  });

  it("rede e erro 500 também usam a reserva traduzida", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new TypeError("rede"))));
    const net = await getConfig();
    if (net.ok) throw new Error("esperava falha");
    expect(apiFailureText(net.error, "en")).toBe("could not reach the server");
    stubFetch(500, {});
    const internal = await getConfig();
    if (internal.ok) throw new Error("esperava falha");
    expect(apiFailureText(internal.error, "en")).toBe("internal server error");
  });

  it("falha do provedor no chat traz code e params", async () => {
    stubFetch(502, {
      error: "o provedor do modelo falhou (provider_bad_response)",
      detail: "incompleta: content_filter",
      code: "provider_failed",
      params: { provider: "provider_bad_response", kind: "incomplete", provider_code: "content_filter" },
    });
    const r = await postChat("p1", "oi");
    if (r.ok) throw new Error("esperava falha");
    expect(r.error.params).toEqual({ provider: "provider_bad_response", kind: "incomplete", provider_code: "content_filter" });
    expect(apiFailureText(r.error, "en")).toBe("The model provider failed (provider_bad_response (incomplete: content_filter)).");
    expect(apiFailureText(r.error, "pt-BR")).toBe("O provedor do modelo falhou (provider_bad_response (incompleta: content_filter)).");
    stubFetch(502, { error: "o provedor do modelo falhou (provider_connect_failed)", code: "provider_failed", params: { provider: "provider_connect_failed" } });
    const plain = await postChat("p1", "oi");
    if (plain.ok) throw new Error("esperava falha");
    expect(apiFailureText(plain.error, "en")).toBe("The model provider failed (provider_connect_failed).");
  });

  it("412 prereq_missing guarda ids e code", async () => {
    stubFetch(412, { error: "prereq_missing", missing: ["git"], code: "prereq_missing", params: { count: 1 } });
    const r = await getConfig();
    if (r.ok) throw new Error("esperava falha");
    expect(r.error.missing).toEqual(["git"]);
    expect(apiFailureText(r.error, "en")).toBe("This computer is not ready for this action yet. See Getting started.");
  });
});

describe("motivo da falha do provedor pelo kind (sem rótulo pt-BR em en)", () => {
  const PT_LABELS = Object.entries(ptBR)
    .filter(([key]) => key.startsWith("provider.kind."))
    .map(([, text]) => text);

  it("todo kind do servidor tem rótulo nos dois idiomas", () => {
    const kinds = [
      "invalid_json", "provider_error", "incomplete", "unsupported_status", "empty", "no_final",
      "too_large", "steps_exhausted", "throttled", "image_unsupported", "image_unreadable", "http_code",
    ];
    for (const kind of kinds) {
      expect(Object.keys(ptBR)).toContain(`provider.kind.${kind}`);
      expect(Object.keys(en)).toContain(`provider.kind.${kind}`);
    }
  });

  it("Incomplete com código e ImageUnreadable em en não trazem rótulo pt-BR", () => {
    const incomplete = systemMessageText(
      {
        text: "O provedor do modelo falhou (provider_bad_response (incompleta: content_filter)).",
        code: "run.provider_failed_detail",
        params: { provider: "provider_bad_response", kind: "incomplete", provider_code: "content_filter" },
      },
      "en",
    );
    expect(incomplete).toBe("The model provider failed (provider_bad_response: incomplete: content_filter).");
    const unreadable = systemMessageText(
      {
        text: "Geração de imagem falhou (...): web/a.webp.",
        code: "image.failed",
        params: { provider: "provider_bad_response", kind: "image_unreadable", path: "web/a.webp" },
      },
      "en",
    );
    expect(unreadable).toBe(
      "Image generation failed (provider_bad_response (the provider did not return a readable image (it may have charged))): web/a.webp. The Developer will use SVG/CSS if needed.",
    );
    for (const text of [incomplete, unreadable]) {
      expect(text).not.toContain("incompleta");
      for (const label of PT_LABELS) expect(text).not.toContain(label);
    }
  });
});

describe("idioma da interface a cada turno (X-Nants-Lang)", () => {
  it("toda requisição leva o idioma ativo, e a troca vale para a próxima", async () => {
    const spy = vi.fn(async (_url: string, _init: RequestInit) => jsonResponse(200, { reply: "ok" }));
    vi.stubGlobal("fetch", spy);
    const sent = (i: number) => (spy.mock.calls[i]?.[1]?.headers as Record<string, string> | undefined)?.[LANG_HEADER];
    setLanguage("en", { persist: false, storage: null });
    expect(getRequestLanguage()).toBe("en");
    await postChat("p1", "hi");
    expect(sent(0)).toBe("en");
    setLanguage("pt-BR", { persist: false, storage: null });
    await postChat("p1", "oi");
    expect(sent(1)).toBe("pt-BR");
    await getConfig();
    expect(sent(2)).toBe("pt-BR");
  });
});

describe("mensagens do sistema e avisos", () => {
  it("mostra pelo code no idioma atual e cai para o texto gravado", () => {
    const stored = { text: "Execução parada pelo usuário. Use Continuar para retomar.", code: "run.stopped", params: {} };
    expect(systemMessageText(stored, "en")).toBe("Run stopped by the user. Use Continue to resume.");
    expect(systemMessageText(stored, "pt-BR")).toBe("Execução parada pelo usuário. Use Continuar para retomar.");
    expect(systemMessageText({ text: "antigo" }, "en")).toBe("antigo");
    expect(systemMessageText({ text: "x", code: "image.failed", params: { path: "a.webp" } }, "en")).toBe("x");
    expect(
      systemMessageText({ text: "x", code: "image.failed", params: { path: "a.webp", provider: "provider_timeout" } }, "en"),
    ).toBe("Image generation failed (provider_timeout): a.webp. The Developer will use SVG/CSS if needed.");
    expect(noticeText({ code: "app_port_remapped", params: { wanted: 3000, used: 3001 } }, "Porta 3000 ocupada; usando 3001", "en")).toBe(
      "Port 3000 busy; using 3001",
    );
    expect(noticeText({}, "reserva", "en")).toBe("reserva");
  });

  it("evento agent_message mantém code/params válidos e descarta os inválidos", () => {
    const event = (payload: Record<string, unknown>) =>
      parseApiEvent({ event_type: "agent_message", project_id: "p1", payload, ts: "x" })?.payload;
    const ok = event({ agent: "system", text: "t", code: "run.failed_cause", params: { cause: "x", obj: {} } });
    expect(ok).toMatchObject({ code: "run.failed_cause", params: { cause: "x" } });
    expect(ok).not.toHaveProperty("params.obj");
    const bad = event({ agent: "system", text: "t", code: "RUN", params: { cause: "x" } });
    expect(bad).toBeDefined();
    expect(bad).not.toHaveProperty("code");
    expect(bad).not.toHaveProperty("params");
  });
});
