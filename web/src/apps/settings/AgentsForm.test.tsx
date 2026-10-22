/*
 * Agentes nas Configurações e nos Primeiros passos (`AgentsForm`): lista de modelos por conexão
 * (carga única compartilhada, filtro, entrada manual, erros, recargas e respostas atrasadas),
 * combobox por teclado, esforço e limite de passos.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { Agents, ConnectionView } from "../../api/types";
import { installFetch } from "../../test-utils";
import { AgentsForm } from "./AgentsForm";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const openai: ConnectionView = {
  id: "openai-direct",
  name: "OpenAI direto",
  base_url: "https://api.openai.com/v1",
  api_key_masked: "********abcd",
};

const corp: ConnectionView = {
  id: "corp-gateway",
  name: "Gateway corporativo",
  base_url: "https://corp.example.com/v1",
  api_key_masked: "env:CORP_KEY",
};

const extra: ConnectionView = {
  id: "extra",
  name: "Extra",
  base_url: "https://extra.example.com/v1",
  api_key_masked: "********zzzz",
};

// JSON literal de GET /api/config/connections/:id/models (contrato do servidor).
const OPENAI_MODELS = {
  connection_id: "openai-direct",
  models: [{ id: "gpt-4o" }, { id: "gpt-4o-mini" }, { id: "o3-mini" }],
  truncated: false,
  fetched_at: 1760000000,
};
const CORP_MODELS = {
  connection_id: "corp-gateway",
  models: [{ id: "corp-large" }, { id: "corp-small" }],
  truncated: false,
  fetched_at: 1760000001,
};

function agents(overrides: Partial<Record<"architect" | "developer" | "reviewer", Partial<Agents["architect"]>>> = {}): Agents {
  return {
    architect: { connection_id: "openai-direct", model: "gpt-4o", effort: "low", ...overrides.architect },
    developer: { connection_id: "openai-direct", model: "gpt-4o-mini", effort: "low", ...overrides.developer },
    reviewer: { connection_id: "corp-gateway", model: "corp-large", effort: "medium", ...overrides.reviewer },
  };
}

// Após editar a base_url da conexão `openai-direct` (JSON literal do servidor).
const NEW_OPENAI_MODELS = {
  connection_id: "openai-direct",
  models: [{ id: "novo-base" }],
  truncated: false,
  fetched_at: 1760000002,
};

const modelsUrl = (id: string, refresh = false) =>
  `/api/config/connections/${id}/models${refresh ? "?refresh=true" : ""}`;

/** Respostas padrão do GET de modelos por conexão; PUT de agentes ecoa o corpo. */
function routes(overrides: Record<string, { status?: number; body: unknown }> = {}) {
  return (method: string, url: string, body: unknown) => {
    if (method === "PUT" && url === "/api/config/agents") return { body };
    const hit = overrides[url];
    if (hit !== undefined) return hit;
    if (url.startsWith("/api/config/connections/openai-direct/models")) return { body: OPENAI_MODELS };
    if (url.startsWith("/api/config/connections/corp-gateway/models")) return { body: CORP_MODELS };
    return { body: {} };
  };
}

function renderPanel(
  agentsValue: Agents = agents(),
  conns: ConnectionView[] = [openai, corp],
) {
  return render(<AgentsForm agents={agentsValue} connections={conns} />);
}

function roleRegion(label: string) {
  return screen.getByRole("region", { name: label });
}

/** Opções do listbox do combobox (não as do `<select>` de conexão). */
function optionsOf(region: HTMLElement): HTMLElement[] {
  return within(within(region).getByRole("listbox")).getAllByRole("option");
}

/** Espera a carga terminar, abre o combobox e confere a quantidade de opções. */
async function waitForOptions(region: HTMLElement, count: number) {
  await waitFor(() => expect(within(region).queryByText("Carregando modelos…")).toBeNull());
  const user = userEvent.setup();
  await user.click(within(region).getByRole("combobox", { name: /Modelo do/ }));
  await waitFor(() => expect(optionsOf(region)).toHaveLength(count));
  return user;
}

describe("AgentsForm: lista de modelos", () => {
  it("carrega os modelos da conexão salva e lista como opções filtráveis", async () => {
    const fetchMock = installFetch(routes());
    renderPanel();
    const region = roleRegion("Arquiteto");
    const user = await waitForOptions(region, 3);

    const urls = fetchMock.mock.calls.map(([url]) => String(url));
    expect(urls).toContain(modelsUrl("openai-direct"));
    expect(within(region).getByRole("listbox")).not.toBeNull();
    expect(within(region).getByRole("combobox", { name: /Modelo do/ }).getAttribute("aria-expanded")).toBe("true");

    const input = within(region).getByRole("combobox", { name: /Modelo do/ });
    await user.clear(input);
    await user.type(input, "GPT-4O-M");
    const options = optionsOf(region);
    expect(options.map((o) => o.textContent)).toEqual(["gpt-4o-mini"]);
  });

  it("uma única carga por conexão, compartilhada entre papéis (sem repetir ao trocar de papel)", async () => {
    const fetchMock = installFetch(routes());
    const user = userEvent.setup();
    renderPanel();
    await waitFor(() => expect(within(roleRegion("Desenvolvedor")).queryByText("Carregando modelos…")).toBeNull());
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    await user.click(within(roleRegion("Desenvolvedor")).getByRole("combobox", { name: /Modelo do/ }));
    await waitFor(() => expect(optionsOf(roleRegion("Desenvolvedor"))).toHaveLength(3));
    const openaiCalls = fetchMock.mock.calls.filter(([url]) => String(url) === modelsUrl("openai-direct"));
    expect(openaiCalls).toHaveLength(1);
  });

  it("escolher um item preenche o campo e salva com esse id", async () => {
    const fetchMock = installFetch(routes());
    renderPanel();
    const region = roleRegion("Arquiteto");
    const user = await waitForOptions(region, 3);
    await user.click(within(region).getByRole("option", { name: "o3-mini" }));
    expect((within(region).getByLabelText("Modelo do Arquiteto") as HTMLInputElement).value).toBe("o3-mini");
    expect(within(region).queryByRole("listbox")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(true));
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body)).architect.model).toBe("o3-mini");
  });

  it("digitar um id fora da lista e salvar funciona (entrada manual)", async () => {
    const fetchMock = installFetch(routes());
    renderPanel();
    const region = roleRegion("Arquiteto");
    const user = await waitForOptions(region, 3);
    const input = within(region).getByLabelText("Modelo do Arquiteto");
    await user.clear(input);
    await user.type(input, "modelo-novo-x1");
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(true));
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body)).architect.model).toBe("modelo-novo-x1");
  });

  it("erro http_error 401 mostra mensagem traduzida e mantém o campo manual", async () => {
    // JSON literal de falha do provedor: 200 com lista vazia e código fixo.
    installFetch(
      routes({
        [modelsUrl("openai-direct")]: { body: { models: [], error: "http_error", status: 401 } },
      }),
    );
    const user = userEvent.setup();
    renderPanel();
    const region = roleRegion("Arquiteto");
    await waitFor(() => expect(within(region).getByText("O provedor respondeu com erro HTTP 401.")).not.toBeNull());
    const input = within(region).getByLabelText("Modelo do Arquiteto");
    expect(input.hasAttribute("disabled")).toBe(false);
    await user.clear(input);
    await user.type(input, "digitado");
    expect((input as HTMLInputElement).value).toBe("digitado");
  });

  it("erro de rede/timeout do servidor vira mensagem fixa sem perder a entrada manual", async () => {
    installFetch(routes({ [modelsUrl("openai-direct")]: { body: { models: [], error: "timeout" } } }));
    renderPanel();
    const region = roleRegion("Arquiteto");
    await waitFor(() => expect(within(region).getByText("Tempo esgotado ao falar com o provedor.")).not.toBeNull());
    expect(within(region).getByLabelText("Modelo do Arquiteto")).not.toBeNull();
  });

  it("unsupported mostra a mensagem de provedor sem listagem e mantém o campo", async () => {
    installFetch(routes({ [modelsUrl("openai-direct")]: { body: { models: [], error: "unsupported" } } }));
    renderPanel();
    const region = roleRegion("Arquiteto");
    await waitFor(() => expect(within(region).getByText("Este provedor não lista modelos. Digite o id.")).not.toBeNull());
    expect(within(region).queryByRole("listbox")).toBeNull();
    expect(within(region).getByLabelText("Modelo do Arquiteto")).not.toBeNull();
  });

  it("lista vazia mostra a orientação para digitar o id", async () => {
    installFetch(
      routes({
        [modelsUrl("openai-direct")]: {
          body: { connection_id: "openai-direct", models: [], truncated: false, fetched_at: 1760000000 },
        },
      }),
    );
    renderPanel();
    const region = roleRegion("Arquiteto");
    await waitFor(() =>
      expect(within(region).getByText("O servidor não retornou modelos; digite o id.")).not.toBeNull(),
    );
  });

  it("lista truncada mostra aviso de lista parcial e continua filtrável", async () => {
    installFetch(
      routes({
        [modelsUrl("openai-direct")]: {
          body: { connection_id: "openai-direct", models: [{ id: "gpt-4o" }], truncated: true, fetched_at: 1760000000 },
        },
      }),
    );
    renderPanel();
    const region = roleRegion("Arquiteto");
    const user = await waitForOptions(region, 1);
    expect(within(region).getByText("Lista parcial; digite o id se não aparecer.")).not.toBeNull();
    await user.clear(within(region).getByLabelText("Modelo do Arquiteto"));
    await user.type(within(region).getByLabelText("Modelo do Arquiteto"), "zz");
    expect(within(region).queryByRole("listbox")).toBeNull();
  });

  it("sem conexão salva selecionada não chama a listagem", () => {
    const fetchMock = installFetch(routes());
    renderPanel(agents({ architect: { connection_id: "", model: "" } }), [openai, corp]);
    // Desenvolvedor e Revisor ainda carregam; só o Arquiteto fica sem conexão.
    expect(fetchMock.mock.calls.map(([url]) => String(url)).some((url) => url.includes("//models"))).toBe(false);
    const region = roleRegion("Arquiteto");
    expect(within(region).queryByRole("listbox")).toBeNull();
    expect(within(region).queryByRole("button", { name: "Atualizar lista" })).toBeNull();
  });

  it("primeira execução (sem conexões) não faz nenhuma chamada", () => {
    const fetchMock = installFetch(routes());
    renderPanel(
      {
        architect: { connection_id: "", model: "", effort: "low" },
        developer: { connection_id: "", model: "", effort: "low" },
        reviewer: { connection_id: "", model: "", effort: "medium" },
      },
      [],
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("Atualizar lista chama a listagem com refresh=true", async () => {
    const fetchMock = installFetch(routes());
    const user = userEvent.setup();
    renderPanel();
    const region = roleRegion("Arquiteto");
    await waitForOptions(region, 3);
    await user.click(within(region).getByRole("button", { name: "Atualizar lista" }));
    await waitFor(() =>
      expect(fetchMock.mock.calls.map(([url]) => String(url))).toContain(modelsUrl("openai-direct", true)),
    );
  });

  it("trocar a conexão carrega a nova lista, mantém o modelo digitado e avisa se ele não consta", async () => {
    const fetchMock = installFetch(routes());
    const user = userEvent.setup();
    renderPanel(agents(), [openai, corp, extra]);
    const region = roleRegion("Arquiteto");
    await waitForOptions(region, 3);
    await user.selectOptions(within(region).getByLabelText("Conexão do Arquiteto"), "corp-gateway");
    const input = within(region).getByLabelText("Modelo do Arquiteto") as HTMLInputElement;
    expect(input.value).toBe("gpt-4o");
    await user.click(input);
    await waitFor(() => expect(optionsOf(region).map((o) => o.textContent)).toEqual(["corp-large", "corp-small"]));
    expect(within(region).getByText("Modelo atual não consta nesta lista.")).not.toBeNull();
    expect(fetchMock.mock.calls.filter(([url]) => String(url) === modelsUrl("corp-gateway"))).toHaveLength(1);
  });

  it("troca recarrega a lista de conexão ainda não carregada", async () => {
    const fetchMock = installFetch(
      routes({
        [modelsUrl("extra")]: { body: { connection_id: "extra", models: [{ id: "extra-1" }], truncated: false, fetched_at: 1 } },
      }),
    );
    const user = userEvent.setup();
    renderPanel(agents(), [openai, corp, extra]);
    const region = roleRegion("Arquiteto");
    await waitForOptions(region, 3);
    await user.selectOptions(within(region).getByLabelText("Conexão do Arquiteto"), "extra");
    await waitFor(() => expect(fetchMock.mock.calls.map(([url]) => String(url))).toContain(modelsUrl("extra")));
    await user.click(within(region).getByLabelText("Modelo do Arquiteto"));
    await waitFor(() => expect(optionsOf(region).map((o) => o.textContent)).toEqual(["extra-1"]));
  });

  it("ignora resposta atrasada da conexão anterior depois de trocar a conexão do papel", async () => {
    let releaseOpenai: (() => void) | undefined;
    const openaiGate = new Promise<void>((resolve) => {
      releaseOpenai = resolve;
    });
    installFetch(async (method, url) => {
      if (method === "GET" && url === modelsUrl("openai-direct")) {
        await openaiGate;
        return { body: OPENAI_MODELS };
      }
      return routes()(method, url, undefined);
    });
    const user = userEvent.setup();
    renderPanel(agents({ developer: { connection_id: "corp-gateway", model: "corp-large" } }), [openai, corp]);
    const region = roleRegion("Arquiteto");
    // Arquiteto está carregando openai (preso no gate); troca para corp antes da resposta.
    expect(within(region).getByText("Carregando modelos…")).not.toBeNull();
    await user.selectOptions(within(region).getByLabelText("Conexão do Arquiteto"), "corp-gateway");
    await user.click(within(region).getByLabelText("Modelo do Arquiteto"));
    await waitFor(() => expect(optionsOf(region).map((o) => o.textContent)).toEqual(["corp-large", "corp-small"]));

    releaseOpenai?.();
    await new Promise((resolve) => setTimeout(resolve, 0));
    // Resultado obsoleto de openai não substitui a lista de corp no papel.
    expect(optionsOf(region).map((o) => o.textContent)).toEqual(["corp-large", "corp-small"]);
    expect(within(region).queryByText("gpt-4o-mini")).toBeNull();
  });

  it("navegação por teclado: setas, Enter escolhe, Escape fecha", async () => {
    installFetch(routes());
    const user = userEvent.setup();
    renderPanel();
    const region = roleRegion("Arquiteto");
    await waitForOptions(region, 3);
    const input = within(region).getByRole("combobox", { name: /Modelo do/ });
    await user.clear(input);
    await user.type(input, "gpt");
    expect(optionsOf(region)).toHaveLength(2);

    await user.keyboard("{ArrowDown}");
    expect(input.getAttribute("aria-activedescendant")).toBe(optionsOf(region)[0]?.id);
    await user.keyboard("{ArrowDown}");
    expect(input.getAttribute("aria-activedescendant")).toBe(optionsOf(region)[1]?.id);
    await user.keyboard("{ArrowUp}");
    expect(input.getAttribute("aria-activedescendant")).toBe(optionsOf(region)[0]?.id);
    await user.keyboard("{Enter}");
    expect((input as HTMLInputElement).value).toBe("gpt-4o");
    expect(input.getAttribute("aria-expanded")).toBe("false");

    await user.keyboard("{ArrowDown}");
    expect(input.getAttribute("aria-expanded")).toBe("true");
    await user.keyboard("{Escape}");
    expect(input.getAttribute("aria-expanded")).toBe("false");
    expect((input as HTMLInputElement).value).toBe("gpt-4o");
  });

  /** F-01: mudar o endpoint salvo (base_url ou chave) do mesmo id recarrega a listagem. */
  async function expectReloadOnEndpointChange(changed: ConnectionView) {
    let openaiCalls = 0;
    const fetchMock = installFetch((method, url) => {
      if (method === "GET" && url === modelsUrl("openai-direct")) {
        openaiCalls += 1;
        return { body: openaiCalls === 1 ? OPENAI_MODELS : NEW_OPENAI_MODELS };
      }
      return routes()(method, url, undefined);
    });
    const { rerender } = render(<AgentsForm agents={agents()} connections={[openai, corp]} />);
    await waitForOptions(roleRegion("Arquiteto"), 3);
    rerender(<AgentsForm agents={agents()} connections={[changed, corp]} />);
    await waitFor(() => expect(openaiCalls).toBe(2));
    const region = roleRegion("Arquiteto");
    await waitForOptions(region, 1);
    expect(optionsOf(region).map((o) => o.textContent)).toEqual(["novo-base"]);
    expect(fetchMock.mock.calls.filter(([url]) => String(url) === modelsUrl("openai-direct"))).toHaveLength(2);
  }

  it("F-01: editar base_url do mesmo id recarrega a listagem e mostra a nova", async () => {
    await expectReloadOnEndpointChange({ ...openai, base_url: "https://outro.example.org/v1" });
  });

  it("F-01: trocar a chave (api_key_masked) do mesmo id recarrega a listagem", async () => {
    await expectReloadOnEndpointChange({ ...openai, api_key_masked: "********zzzz" });
  });

  it("F-02: falha não fica em cache: trocar para outra conexão e voltar refaz a chamada", async () => {
    let openaiCalls = 0;
    installFetch((method, url) => {
      if (method === "GET" && url === modelsUrl("openai-direct")) {
        openaiCalls += 1;
        return openaiCalls === 1
          ? { body: { models: [], error: "http_error", status: 401 } }
          : { body: OPENAI_MODELS };
      }
      return routes()(method, url, undefined);
    });
    const user = userEvent.setup();
    renderPanel();
    const region = roleRegion("Arquiteto");
    await waitFor(() => expect(within(region).getByText("O provedor respondeu com erro HTTP 401.")).not.toBeNull());
    await user.selectOptions(within(region).getByLabelText("Conexão do Arquiteto"), "corp-gateway");
    await user.selectOptions(within(region).getByLabelText("Conexão do Arquiteto"), "openai-direct");
    await waitFor(() => expect(openaiCalls).toBe(2));
    await waitForOptions(region, 3);
  });

  it("F-03: Enter com lista aberta não submete; sem destaque só fecha; com lista fechada submete", async () => {
    const fetchMock = installFetch(routes());
    const user = userEvent.setup();
    renderPanel();
    const region = roleRegion("Arquiteto");
    await waitForOptions(region, 3);
    const input = within(region).getByRole("combobox", { name: /Modelo do/ });
    await user.clear(input);
    await user.type(input, "gpt-4o-m");
    expect(optionsOf(region)).toHaveLength(1);

    await user.keyboard("{Enter}");
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(false);
    expect(within(region).queryByRole("listbox")).toBeNull();
    expect((input as HTMLInputElement).value).toBe("gpt-4o-m");

    await user.keyboard("{Enter}");
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(true));
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body)).architect.model).toBe("gpt-4o-m");
  });

  it("esforço continua fixo em low|medium|high e é salvo como antes", async () => {
    const fetchMock = installFetch(routes());
    const user = userEvent.setup();
    renderPanel();
    const region = roleRegion("Desenvolvedor");
    const group = within(region).getByRole("radiogroup", { name: "Esforço do Desenvolvedor" });
    expect(within(group).getAllByRole("radio").map((r) => (r as HTMLInputElement).value)).toEqual(["low", "medium", "high"]);
    await user.click(within(group).getByRole("radio", { name: "High" }));
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(true));
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body)).developer.effort).toBe("high");
  });
});

describe("AgentsForm: limite de passos (spec 081/T03)", () => {
  /** Corpo do último PUT de agentes. */
  function lastPut(fetchMock: ReturnType<typeof installFetch>): Agents {
    const puts = fetchMock.mock.calls.filter(([, init]) => init?.method === "PUT");
    return JSON.parse(String(puts[puts.length - 1]?.[1]?.body)) as Agents;
  }

  it("só Desenvolvedor e Revisor têm o campo, com o padrão no placeholder e a ajuda", () => {
    installFetch(routes());
    renderPanel();
    expect(within(roleRegion("Arquiteto")).queryByLabelText(/Limite de passos/)).toBeNull();
    const dev = within(roleRegion("Desenvolvedor")).getByLabelText("Limite de passos do Desenvolvedor") as HTMLInputElement;
    const rev = within(roleRegion("Revisor")).getByLabelText("Limite de passos do Revisor") as HTMLInputElement;
    expect(dev.placeholder).toBe("150");
    expect(rev.placeholder).toBe("40");
    expect(dev.value).toBe("");
    expect(dev.min).toBe("10");
    expect(dev.max).toBe("1000");
    expect(within(roleRegion("Desenvolvedor")).getByText(/rede de segurança; o orçamento controla o custo/i)).not.toBeNull();
  });

  it("editar e salvar envia max_steps; vazio não envia o campo", async () => {
    const fetchMock = installFetch(routes());
    const user = userEvent.setup();
    renderPanel();
    const dev = within(roleRegion("Desenvolvedor")).getByLabelText("Limite de passos do Desenvolvedor");
    await user.type(dev, "300");
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(true));
    const body = lastPut(fetchMock);
    expect(body.developer.max_steps).toBe(300);
    expect("max_steps" in body.reviewer).toBe(false);
    expect("max_steps" in body.architect).toBe(false);
  });

  it("valor salvo aparece no campo; apagar e salvar remove o max_steps do payload", async () => {
    const fetchMock = installFetch(routes());
    const user = userEvent.setup();
    renderPanel(agents({ reviewer: { max_steps: 60 } }));
    const rev = within(roleRegion("Revisor")).getByLabelText("Limite de passos do Revisor") as HTMLInputElement;
    expect(rev.value).toBe("60");
    await user.clear(rev);
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(true));
    expect("max_steps" in lastPut(fetchMock).reviewer).toBe(false);
  });

  it("fora da faixa 10–1000 mostra erro e não salva", async () => {
    const fetchMock = installFetch(routes());
    const user = userEvent.setup();
    renderPanel();
    const dev = within(roleRegion("Desenvolvedor")).getByLabelText("Limite de passos do Desenvolvedor");
    await user.type(dev, "5");
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    expect(within(roleRegion("Desenvolvedor")).getByText(/Use um número inteiro de 10 a 1000/)).not.toBeNull();
    expect(dev.getAttribute("aria-invalid")).toBe("true");
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(false);
    await user.clear(dev);
    await user.type(dev, "1001");
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT")).toBe(false);
  });
});
