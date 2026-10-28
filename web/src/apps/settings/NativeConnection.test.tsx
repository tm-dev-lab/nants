/*
 * Conexão nativa na interface (spec 132 T10), montada pelo shell: os cartões de provedor, o
 * formulário só com o nome e a chave (endereço avançado recolhido, link oficial da chave), o
 * "Testar" com a lista de modelos, o envio com o `kind`, a sugestão de modelo por papel na primeira
 * conexão do provedor (editável) e a edição de uma conexão nativa salva.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { appStore } from "../../store";
import { makeProject, makeProjectState } from "../../test-utils";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const KEY: ConnectionView = { id: "openai", name: "OpenAI", base_url: "https://api.openai.com/v1", api_key_masked: "********abcd", kind: "api_key" };
const SECRET = "sk-ant-SEGREDO-123";
/** Lista na ordem do provedor; o C não raciocina, e o catálogo dá ao A o menor preço entre A e B. */
const MODELS = [
  { id: "modelo-a", name: "Alpha", output_tokens: 64000, thinking: true, vision: true },
  { id: "modelo-b", name: "Beta", output_tokens: 128000, thinking: true, vision: true },
  { id: "modelo-c", name: "Gamma", output_tokens: 256000, thinking: false, vision: true },
];

/** Catálogo de preços: o C (sem raciocínio) é o mais barato, mas não é candidato. */
const PRICES = {
  "modelo-a": { input_per_mtok: 1, output_per_mtok: 5, source: "estimated" },
  "modelo-b": { input_per_mtok: 3, output_per_mtok: 15, source: "estimated" },
  "modelo-c": { input_per_mtok: 0.1, output_per_mtok: 0.4, source: "estimated" },
};

function savedView(body: unknown): ConnectionView {
  const input = body as { id: string; name: string; kind: ConnectionView["kind"]; base_url_override?: string };
  const base = input.kind === "anthropic" ? "https://api.anthropic.com" : "https://generativelanguage.googleapis.com";
  return {
    id: input.id,
    name: input.name,
    base_url: base,
    api_key_masked: "********-123",
    wire_api: "chat",
    kind: input.kind,
    ...(input.base_url_override !== undefined && input.base_url_override !== "" ? { base_url_override: input.base_url_override } : {}),
  };
}

async function openSettings(extra: RenderShellOptions = {}) {
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) }, connections: [KEY], ...extra });
  view.server.reply("GET", /^\/api\/config\/connections\/[^/]+\/models/, () => ({ status: 200, body: { models: MODELS, truncated: false } }));
  view.server.reply("POST", /^\/api\/config\/connections\/test$/, () => ({ status: 200, body: { ok: true, latency_ms: 80, models_count: MODELS.length, models: MODELS } }));
  view.server.reply("PUT", /^\/api\/config\/connections$/, (body) => ({ status: 200, body: savedView(body) }));
  view.server.reply("PUT", /^\/api\/config\/agents$/, (body) => ({ status: 200, body }));
  view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: [] }));
  view.server.reply("GET", /^\/api\/config\/pricing\/estimate/, () => ({ status: 200, body: PRICES }));
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: /^(Configurações|Settings)$/ }));
  });
  await settle();
  return view;
}

function settings(): HTMLElement {
  return screen.getByRole("region", { name: /^(Configurações|Settings)$/ });
}

async function chooseProvider(name: RegExp, newLabel = "Nova conexão"): Promise<HTMLElement> {
  const newButton = within(settings()).queryByRole("button", { name: newLabel });
  if (newButton !== null) {
    await act(async () => {
      fireEvent.click(newButton);
    });
  }
  fireEvent.click(within(settings()).getByRole("button", { name }));
  const form = settings().querySelector<HTMLElement>("[data-native-form]");
  if (form === null) throw new Error("formulário nativo ausente");
  return form;
}

describe("cartões de provedor", () => {
  it("quatro cartões, uma linha cada, na ordem; sem glifos proibidos", async () => {
    await openSettings();
    await act(async () => {
      fireEvent.click(within(settings()).getByRole("button", { name: "Nova conexão" }));
    });
    const step = settings().querySelector<HTMLElement>("[data-connection-method]");
    if (step === null) throw new Error("passo ausente");
    const cards = [...step.querySelectorAll<HTMLElement>("[data-method]")];
    expect(cards.map((c) => c.querySelector(".nd-strong")?.textContent)).toEqual([
      "OpenAI ou compatível (chave de API)",
      "Anthropic (Claude)",
      "Google Gemini (AI Studio)",
      "Plano ChatGPT",
    ]);
    for (const card of cards) expect(card.querySelectorAll(".nd-field-hint")).toHaveLength(1);
    expect(findForbiddenGlyphs(step)).toEqual([]);
  });
});

describe("Gemini pela camada compatível", () => {
  it("o formulário por chave avisa que funciona com limites e recomenda a conexão nativa", async () => {
    await openSettings();
    await act(async () => {
      fireEvent.click(within(settings()).getByRole("button", { name: "Nova conexão" }));
    });
    fireEvent.click(within(settings()).getByRole("button", { name: /^OpenAI ou compatível/ }));
    const url = within(settings()).getByLabelText("URL base");
    expect(settings().querySelector("[data-gemini-compat-note]")).toBeNull();
    fireEvent.change(url, { target: { value: "https://generativelanguage.googleapis.com/v1beta/openai/" } });
    expect(settings().querySelector("[data-gemini-compat-note]")?.textContent).toBe(
      "Este é o Gemini pela camada compatível com a OpenAI: funciona, mas sem busca web e sem o raciocínio na tela. O caminho recomendado é a conexão Google Gemini (AI Studio).",
    );
  });
});

describe("formulário nativo", () => {
  it("Anthropic: só nome e chave, endereço avançado recolhido e o link oficial da chave", async () => {
    await openSettings();
    const form = await chooseProvider(/^Anthropic \(Claude\)/);
    expect(form.getAttribute("data-native-form")).toBe("anthropic");
    expect((within(form).getByLabelText("Nome") as HTMLInputElement).value).toBe("Anthropic");
    expect(within(form).getByLabelText("Chave de API").getAttribute("type")).toBe("password");
    expect(within(form).queryByLabelText("URL base")).toBeNull();
    expect(within(form).queryByText("Protocolo")).toBeNull();
    expect(within(form).queryByLabelText("Identificador")).toBeNull();
    const advanced = form.querySelector<HTMLDetailsElement>("[data-native-advanced]");
    expect(advanced?.open).toBe(false);
    expect(within(form).getByLabelText("Endereço da API").getAttribute("placeholder")).toBe("https://api.anthropic.com");
    const link = within(form).getByRole("link", { name: "Criar a chave no console da Anthropic" });
    expect(link.getAttribute("href")).toBe("https://console.anthropic.com/settings/keys");
    expect(link.getAttribute("target")).toBe("_blank");
    expect(form.textContent).toContain("env:ANTHROPIC_API_KEY");
    expect(findForbiddenGlyphs(form)).toEqual([]);
  });

  it("Gemini: link do AI Studio e endereço oficial do Google", async () => {
    await openSettings();
    const form = await chooseProvider(/^Google Gemini \(AI Studio\)/);
    expect(within(form).getByRole("link", { name: "Criar a chave no Google AI Studio" }).getAttribute("href")).toBe("https://aistudio.google.com/apikey");
    expect(within(form).getByLabelText("Endereço da API").getAttribute("placeholder")).toBe("https://generativelanguage.googleapis.com");
    expect((within(form).getByLabelText("Nome") as HTMLInputElement).value).toBe("Google Gemini");
  });

  it("validação local: chave obrigatória, env: inválida e endereço avançado inválido abrem o erro", async () => {
    const { server } = await openSettings();
    const form = await chooseProvider(/^Anthropic \(Claude\)/);
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar conexão" }));
    });
    expect(form.textContent).toContain("Informe a chave de API ou uma referência env:NOME.");
    fireEvent.change(within(form).getByLabelText("Chave de API"), { target: { value: "env:1X" } });
    fireEvent.change(within(form).getByLabelText("Endereço da API"), { target: { value: "ftp://proxy" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Testar" }));
    });
    expect(form.querySelector<HTMLDetailsElement>("[data-native-advanced]")?.open).toBe(true);
    expect(server.calls.filter((c) => c.url === "/api/config/connections/test")).toEqual([]);
    expect(server.calls.filter((c) => c.method === "PUT" && c.url === "/api/config/connections")).toEqual([]);
  });

  it("Testar: envia o tipo e a chave, sem salvar, e lista os modelos na ordem do provedor", async () => {
    const { server } = await openSettings();
    const form = await chooseProvider(/^Anthropic \(Claude\)/);
    fireEvent.change(within(form).getByLabelText("Chave de API"), { target: { value: SECRET } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Testar" }));
    });
    await settle();
    const call = server.calls.find((c) => c.url === "/api/config/connections/test");
    expect(call?.body).toEqual({ kind: "anthropic", base_url_override: "", api_key: SECRET });
    expect(form.querySelector("[data-native-test]")?.getAttribute("data-native-test")).toBe("ok");
    expect(form.textContent).toContain("Conexão ok · 3 modelos (80 ms).");
    const pills = [...form.querySelectorAll("[data-native-models] li")].map((li) => li.textContent);
    expect(pills).toEqual(["Alpha", "Beta", "Gamma"]);
    expect(server.calls.filter((c) => c.method === "PUT" && c.url === "/api/config/connections")).toEqual([]);
  });

  it("Testar com erro do provedor mostra o código traduzido", async () => {
    const { server } = await openSettings();
    server.reply("POST", /^\/api\/config\/connections\/test$/, () => ({ status: 200, body: { ok: false, error: "auth_invalid" } }));
    const form = await chooseProvider(/^Google Gemini/);
    fireEvent.change(within(form).getByLabelText("Chave de API"), { target: { value: "AIza-x" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Testar" }));
    });
    await settle();
    expect(form.querySelector("[data-native-test]")?.getAttribute("data-native-test")).toBe("failed");
    expect(form.querySelector("[data-native-models]")).toBeNull();
  });
});

describe("salvar e sugestão por papel", () => {
  it("primeira conexão do provedor: envia o kind, a chave sai do estado e a sugestão (editável) vai aos papéis", async () => {
    const { server } = await openSettings();
    const form = await chooseProvider(/^Anthropic \(Claude\)/);
    fireEvent.change(within(form).getByLabelText("Nome"), { target: { value: "Claude da Equipe" } });
    fireEvent.change(within(form).getByLabelText("Chave de API"), { target: { value: SECRET } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar conexão" }));
    });
    await settle();
    const put = server.calls.find((c) => c.method === "PUT" && c.url === "/api/config/connections");
    expect(put?.body).toEqual({ id: "claude-da-equipe", name: "Claude da Equipe", base_url: "", api_key: SECRET, kind: "anthropic", base_url_override: "" });
    expect(document.body.textContent).not.toContain(SECRET);
    expect([...document.querySelectorAll("input")].some((input) => input.value === SECRET)).toBe(false);

    const suggestion = settings().querySelector<HTMLElement>("[data-role-suggestion]");
    if (suggestion === null) throw new Error("sugestão ausente");
    expect(suggestion.textContent).toContain("É a primeira conexão Anthropic. A sugestão é o modelo de menor custo com raciocínio e imagem; troque o que quiser.");
    // O preço é pedido pelo tipo da conexão, só para os candidatos.
    expect(server.calls.find((c) => c.url.startsWith("/api/config/pricing/estimate"))?.url).toBe("/api/config/pricing/estimate?models=modelo-a%2Cmodelo-b&kind=anthropic");
    const select = (role: string): HTMLSelectElement => {
      const el = suggestion.querySelector<HTMLSelectElement>(`[data-suggestion-role="${role}"] select`);
      if (el === null) throw new Error(`seletor ${role} ausente`);
      return el;
    };
    // A regra: com raciocínio e imagem, o de menor preço (o C não raciocina).
    expect(["architect", "developer", "reviewer"].map((r) => select(r).value)).toEqual(["modelo-a", "modelo-a", "modelo-a"]);
    fireEvent.change(select("developer"), { target: { value: "modelo-b" } });
    fireEvent.change(select("reviewer"), { target: { value: "" } });
    await act(async () => {
      fireEvent.click(within(suggestion).getByRole("button", { name: "Usar nos papéis" }));
    });
    await settle();
    const agents = server.calls.find((c) => c.method === "PUT" && c.url === "/api/config/agents")?.body as Record<string, { connection_id: string; model: string; effort: string }>;
    expect(agents.architect).toEqual({ connection_id: "claude-da-equipe", model: "modelo-a", effort: "low" });
    expect(agents.developer).toEqual({ connection_id: "claude-da-equipe", model: "modelo-b", effort: "low" });
    // "Manter o atual": o papel fica como estava.
    expect(agents.reviewer).toEqual({ connection_id: "conn", model: "model", effort: "low" });
    expect(appStore.getState().config?.agents.architect.model).toBe("modelo-a");
    expect(settings().querySelector("[data-role-suggestion]")).toBeNull();
    expect(settings().querySelector('[data-connection="claude-da-equipe"]')).not.toBeNull();
  });

  it("Agora não fecha sem mexer nos papéis", async () => {
    const { server } = await openSettings();
    const form = await chooseProvider(/^Google Gemini/);
    fireEvent.change(within(form).getByLabelText("Chave de API"), { target: { value: "env:GEMINI_API_KEY" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar conexão" }));
    });
    await settle();
    expect((server.calls.find((c) => c.method === "PUT" && c.url === "/api/config/connections")?.body as { id: string }).id).toBe("google-gemini");
    await act(async () => {
      fireEvent.click(within(settings()).getByRole("button", { name: "Agora não" }));
    });
    expect(server.calls.filter((c) => c.url === "/api/config/agents" && c.method === "PUT")).toEqual([]);
    expect(settings().querySelector("[data-role-suggestion]")).toBeNull();
  });

  it("não é a primeira do provedor: salva sem sugestão; edição usa o formulário nativo e mantém a chave", async () => {
    const saved: ConnectionView = { id: "anthropic", name: "Anthropic", base_url: "https://api.anthropic.com", api_key_masked: "********-999", kind: "anthropic", wire_api: "chat" };
    const { server } = await openSettings({ connections: [KEY, saved] });
    const form = await chooseProvider(/^Anthropic \(Claude\)/);
    fireEvent.change(within(form).getByLabelText("Chave de API"), { target: { value: SECRET } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar conexão" }));
    });
    await settle();
    expect((server.calls.find((c) => c.method === "PUT" && c.url === "/api/config/connections")?.body as { id: string }).id).toBe("anthropic-2");
    expect(settings().querySelector("[data-role-suggestion]")).toBeNull();
    expect(server.calls.filter((c) => /\/models/.test(c.url) && c.url.includes("anthropic-2"))).toEqual([]);

    // Edição da conexão nativa salva.
    const line = settings().querySelector<HTMLElement>('[data-connection="anthropic"]');
    if (line === null) throw new Error("linha ausente");
    expect(line.querySelector("[data-conn-detail]")?.textContent).toBe("api.anthropic.com · Anthropic · chave ********-999");
    await act(async () => {
      fireEvent.click(within(line).getByRole("button", { name: /Mais ações/ }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("menuitem", { name: "Editar" }));
    });
    const edit = settings().querySelector<HTMLElement>('[data-native-form="anthropic"]');
    if (edit === null) throw new Error("edição ausente");
    expect(edit.textContent).toContain("Vazia mantém a chave salva.");
    fireEvent.change(within(edit).getByLabelText("Nome"), { target: { value: "Claude" } });
    await act(async () => {
      fireEvent.click(within(edit).getByRole("button", { name: "Testar" }));
    });
    await settle();
    expect(server.calls.filter((c) => c.url === "/api/config/connections/test").at(-1)?.body).toEqual({ kind: "anthropic", base_url_override: "", id: "anthropic" });
    await act(async () => {
      fireEvent.click(within(edit).getByRole("button", { name: "Salvar conexão" }));
    });
    await settle();
    expect(server.calls.filter((c) => c.method === "PUT" && c.url === "/api/config/connections").at(-1)?.body).toEqual({ id: "anthropic", name: "Claude", base_url: "", api_key: "", kind: "anthropic", base_url_override: "" });
    expect(settings().querySelector("[data-role-suggestion]")).toBeNull();
  });
});

describe("endereço avançado trocado sem a chave (spec 132 T11)", () => {
  it("o Testar e o Salvar mostram a mensagem do código, não o 422 genérico", async () => {
    const saved: ConnectionView = { id: "anthropic", name: "Anthropic", base_url: "https://api.anthropic.com", api_key_masked: "********-999", kind: "anthropic", wire_api: "chat" };
    const { server } = await openSettings({ connections: [KEY, saved] });
    server.reply("POST", /^\/api\/config\/connections\/test$/, () => ({ status: 422, body: { error: "api_key", code: "api_key_required_other_url" } }));
    server.reply("PUT", /^\/api\/config\/connections$/, () => ({ status: 422, body: { error: "api_key", code: "api_key_required_base_url" } }));
    const line = settings().querySelector<HTMLElement>('[data-connection="anthropic"]');
    if (line === null) throw new Error("linha ausente");
    await act(async () => {
      fireEvent.click(within(line).getByRole("button", { name: /Mais ações/ }));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("menuitem", { name: "Editar" }));
    });
    const edit = settings().querySelector<HTMLElement>('[data-native-form="anthropic"]');
    if (edit === null) throw new Error("edição ausente");
    fireEvent.change(within(edit).getByLabelText("Endereço da API"), { target: { value: "https://proxy.example.com" } });
    await act(async () => {
      fireEvent.click(within(edit).getByRole("button", { name: "Testar" }));
    });
    await settle();
    expect(edit.textContent).toContain("api_key é obrigatória quando base_url difere da conexão salva");
    await act(async () => {
      fireEvent.click(within(edit).getByRole("button", { name: "Salvar conexão" }));
    });
    await settle();
    const alert = edit.querySelector('[role="alert"]');
    expect(alert?.textContent).toBe("api_key é obrigatória quando base_url muda");
    expect(edit.textContent).not.toContain("Dados inválidos");
  });
});

describe("sugestão sem catálogo de preços", () => {
  it("falha ou nenhum preço: o primeiro candidato na ordem do provedor", async () => {
    const { server } = await openSettings();
    server.reply("GET", /^\/api\/config\/pricing\/estimate/, () => ({ status: 500, body: { error: "x" } }));
    const form = await chooseProvider(/^Google Gemini/);
    fireEvent.change(within(form).getByLabelText("Chave de API"), { target: { value: "AIza-x" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar conexão" }));
    });
    await settle();
    const select = settings().querySelector<HTMLSelectElement>('[data-suggestion-role="architect"] select');
    expect(select?.value).toBe("modelo-a");
    expect(server.calls.find((c) => c.url.startsWith("/api/config/pricing/estimate"))?.url).toContain("&kind=google_gemini");
  });
});

describe("em en", () => {
  it("cartões, formulário e sugestão sem português", async () => {
    setLanguage("en", { persist: false });
    await openSettings();
    await act(async () => {
      fireEvent.click(within(settings()).getByRole("button", { name: "New connection" }));
    });
    const step = settings().querySelector<HTMLElement>("[data-connection-method]");
    expect([...(step?.querySelectorAll("[data-method] .nd-strong") ?? [])].map((el) => el.textContent)).toEqual([
      "OpenAI or compatible (API key)",
      "Anthropic (Claude)",
      "Google Gemini (AI Studio)",
      "ChatGPT plan",
    ]);
    fireEvent.click(within(settings()).getByRole("button", { name: /^Anthropic \(Claude\)/ }));
    const form = settings().querySelector<HTMLElement>("[data-native-form]");
    if (form === null) throw new Error("formulário ausente");
    expect(within(form).getByRole("link", { name: "Create the key in the Anthropic console" })).toBeTruthy();
    expect(within(form).getByText("Advanced address")).toBeTruthy();
    fireEvent.change(within(form).getByLabelText("API key"), { target: { value: SECRET } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Test" }));
    });
    await settle();
    expect(within(form).getByText("Models the key can reach")).toBeTruthy();
    expect(portugueseLeftovers(form)).toEqual([]);
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Save connection" }));
    });
    await settle();
    const suggestion = settings().querySelector<HTMLElement>("[data-role-suggestion]");
    if (suggestion === null) throw new Error("sugestão ausente");
    expect(within(suggestion).getByText("Suggested model per role")).toBeTruthy();
    expect(within(suggestion).getByRole("button", { name: "Use for the roles" })).toBeTruthy();
    expect(within(suggestion).getByRole("button", { name: "Not now" })).toBeTruthy();
    expect(portugueseLeftovers(suggestion)).toEqual([]);
  });
});
