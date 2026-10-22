/*
 * Conexões e agentes das Configurações, montados direto: o cartão da conexão ("Testar", inclusive
 * o teste por chat quando o provedor não lista modelos), o formulário (validação, chave só no
 * campo password, regras da chave ao editar, id duplicado, env:NOME) e o roteamento dos agentes
 * (conexão que não existe mais, primeira execução, modelo vazio, erro fixo, sugestões e o store).
 */
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Agents, ConfigView, ConnectionView } from "../../api/types";
import { appStore } from "../../store";
import { installFetch, makeConfig, makeProject, resetStore } from "../../test-utils";
import { AgentsForm } from "./AgentsForm";
import { ConnectionRow, connectionDotTone } from "./ConnectionRow";
import { ConnectionForm } from "./ConnectionForm";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const openai: ConnectionView = { id: "openai-direct", name: "OpenAI direto", base_url: "https://api.openai.com/v1", api_key_masked: "********abcd" };
const corp: ConnectionView = { id: "corp-gateway", name: "Gateway corporativo", base_url: "https://corp.example.com/v1", api_key_masked: "env:CORP_KEY" };

function agents(overrides: Partial<Record<"architect" | "developer" | "reviewer", Partial<Agents["architect"]>>> = {}): Agents {
  return {
    architect: { connection_id: "openai-direct", model: "gpt-4o", effort: "low", ...overrides.architect },
    developer: { connection_id: "openai-direct", model: "gpt-4o-mini", effort: "low", ...overrides.developer },
    reviewer: { connection_id: "corp-gateway", model: "gpt-4.1", effort: "medium", ...overrides.reviewer },
  };
}

function config(overrides: Partial<ConfigView> = {}): ConfigView {
  return { ...makeConfig([makeProject("p1", "api-pagamentos")]), connections: [openai, corp], agents: agents(), ...overrides };
}

/** Escritas (não GET), sem a listagem de modelos. */
function mutations(fetchMock: ReturnType<typeof installFetch>) {
  return fetchMock.mock.calls.filter(([, init]) => init?.method !== undefined && init.method !== "GET");
}

/** Respostas: listagem de modelos vazia; o resto pelo `handler`. */
function routes(handler: (method: string, url: string, body: unknown) => { status?: number; body?: unknown } = () => ({ body: {} })) {
  return installFetch((method, url, body) => (url.includes("/models") ? { body: { models: [], truncated: false } } : handler(method, url, body)));
}

describe("Conexões: cartão e Testar", () => {
  it("linha: nome, detalhe (host, protocolo e só a máscara ou a referência env:), URL inteira na dica; sem chave literal", () => {
    render(
      <ul>
        <ConnectionRow connection={openai} />
        <ConnectionRow connection={corp} />
      </ul>,
    );
    expect(screen.getByText("OpenAI direto")).toBeTruthy();
    expect(screen.getByText("api.openai.com · Chat Completions · chave ********abcd")).toBeTruthy();
    expect(screen.getByText("corp.example.com · Chat Completions · chave env:CORP_KEY").getAttribute("title")).toBe("https://corp.example.com/v1");
    expect(screen.queryByRole("button", { name: /remover/i })).toBeNull();
    expect(document.body.textContent).not.toContain("sk-");
    expect(document.body.textContent).not.toContain("@");
  });

  it("com onEdit: Testar e Editar ficam no menu '…' da linha, com 'usada por' ao lado", async () => {
    installFetch(() => ({ body: { ok: true, latency_ms: 42, models_count: 3 } }));
    const onEdit = vi.fn();
    const user = userEvent.setup();
    render(
      <ul>
        <ConnectionRow connection={openai} users={["Arquiteto", "Revisor"]} onEdit={onEdit} />
        <ConnectionRow connection={corp} users={[]} onEdit={onEdit} />
      </ul>,
    );
    const row = document.querySelector<HTMLElement>("[data-connection='openai-direct']");
    if (row === null) throw new Error("linha ausente");
    expect(within(row).getByText("usada por Arquiteto, Revisor")).toBeTruthy();
    expect(document.querySelector("[data-connection='corp-gateway'] [data-conn-usage]")?.textContent).toBe("sem uso");
    expect(within(row).queryByRole("button", { name: /Testar/ })).toBeNull();
    await user.click(within(row).getByRole("button", { name: "Mais ações de OpenAI direto" }));
    const menu = screen.getByRole("menu", { name: "Ações da conexão OpenAI direto" });
    expect(within(menu).getAllByRole("menuitem").map((i) => i.textContent)).toEqual(["Testar", "Editar"]);
    await user.click(within(menu).getByRole("menuitem", { name: "Testar" }));
    await waitFor(() => expect(within(row).getByText("Conexão ok · 3 modelos (42 ms).")).toBeTruthy());
    expect(row.getAttribute("data-conn-tone")).toBe("green");
    await user.click(within(row).getByRole("button", { name: "Mais ações de OpenAI direto" }));
    await user.click(screen.getByRole("menuitem", { name: "Editar" }));
    expect(onEdit).toHaveBeenCalledWith(openai);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("ponto: o último teste manda; sem teste, em uso (verde) ou sem uso (neutro)", () => {
    expect(connectionDotTone(null, ["Arquiteto"])).toBe("green");
    expect(connectionDotTone(null, [])).toBeNull();
    expect(connectionDotTone(null, undefined)).toBeNull();
    expect(connectionDotTone({ ok: false }, ["Arquiteto"])).toBe("rose");
    expect(connectionDotTone({ ok: true }, [])).toBe("green");
  });

  it("Testar usa o id salvo (sem chave nem modelo) e mostra a contagem; um modelo usa o singular", async () => {
    let count = 3;
    const fetchMock = installFetch(() => ({ body: { ok: true, latency_ms: 42, models_count: count } }));
    const user = userEvent.setup();
    render(<ConnectionRow connection={openai} />);
    await user.click(screen.getByRole("button", { name: "Testar conexão OpenAI direto" }));
    await waitFor(() => expect(screen.getByText("Conexão ok · 3 modelos (42 ms).")).toBeTruthy());
    expect(fetchMock.mock.calls[0]?.[1]?.body).toBe(JSON.stringify({ id: "openai-direct" }));
    count = 1;
    await user.click(screen.getByRole("button", { name: "Testar conexão OpenAI direto" }));
    await waitFor(() => expect(screen.getByText("Conexão ok · 1 modelo (42 ms).")).toBeTruthy());
  });

  it("unsupported mostra o campo de modelo e reenvia o teste com o modelo informado", async () => {
    const fetchMock = installFetch((_m, _u, body) => ((body as { model?: string }).model === "gpt-4o" ? { body: { ok: true, latency_ms: 11 } } : { body: { ok: false, error: "unsupported" } }));
    const user = userEvent.setup();
    render(<ConnectionRow connection={openai} />);
    await user.click(screen.getByRole("button", { name: "Testar conexão OpenAI direto" }));
    await waitFor(() => expect(screen.getByText("Este servidor não lista modelos; informe um modelo para testar por chat")).toBeTruthy());
    await user.type(screen.getByLabelText("Modelo para testar"), "gpt-4o");
    await user.click(screen.getByRole("button", { name: "Testar por chat OpenAI direto" }));
    await waitFor(() => expect(screen.getByText("Conexão ok (11 ms).")).toBeTruthy());
    expect(fetchMock.mock.calls.at(-1)?.[1]?.body).toBe(JSON.stringify({ id: "openai-direct", model: "gpt-4o" }));
  });

  it("nenhum outro erro de teste pede modelo; os códigos viram texto fixo", async () => {
    let reply: unknown = { ok: false, error: "invalid_config" };
    installFetch(() => ({ body: reply }));
    const user = userEvent.setup();
    render(<ConnectionRow connection={openai} />);
    await user.click(screen.getByRole("button", { name: "Testar conexão OpenAI direto" }));
    await waitFor(() => expect(screen.getByText("Configuração inválida: revise a URL e a chave.")).toBeTruthy());
    expect(screen.queryByLabelText("Modelo para testar")).toBeNull();
    reply = { ok: false, error: "timeout" };
    await user.click(screen.getByRole("button", { name: "Testar conexão OpenAI direto" }));
    await waitFor(() => expect(screen.getByText("Tempo esgotado ao falar com o provedor.")).toBeTruthy());
  });

  it("http_error mostra o status e não o corpo remoto", async () => {
    installFetch(() => ({ body: { ok: false, error: "http_error", status: 401, body: "segredo remoto" } }));
    const user = userEvent.setup();
    render(<ConnectionRow connection={openai} />);
    await user.click(screen.getByRole("button", { name: "Testar conexão OpenAI direto" }));
    await waitFor(() => expect(screen.getByText("O provedor respondeu com erro HTTP 401.")).toBeTruthy());
    expect(document.body.textContent).not.toContain("segredo remoto");
  });
});

describe("Conexões: formulário", () => {
  async function fill(user: ReturnType<typeof userEvent.setup>, values: { id?: string; name?: string; url?: string; key?: string }) {
    if (values.id !== undefined) await user.type(screen.getByLabelText("Identificador"), values.id);
    if (values.name !== undefined) await user.type(screen.getByLabelText("Nome"), values.name);
    if (values.url !== undefined) await user.type(screen.getByLabelText("URL base"), values.url);
    if (values.key !== undefined) await user.type(screen.getByLabelText("Chave de API"), values.key);
  }
  const save = () => screen.getByRole("button", { name: "Salvar conexão" });

  it("valida no cliente antes de enviar", async () => {
    const fetchMock = installFetch(() => ({ body: {} }));
    const user = userEvent.setup();
    render(<ConnectionForm existingIds={[]} onSaved={() => undefined} />);
    await fill(user, { url: "https://u:p@x.com" });
    await user.click(save());
    expect(screen.getByText("Informe um identificador.")).toBeTruthy();
    expect(mutations(fetchMock)).toHaveLength(0);
  });

  it("sem seletor de tipo de provedor nem versão da API; env:NOME tem dica e referência inválida é recusada", async () => {
    installFetch(() => ({ body: {} }));
    const user = userEvent.setup();
    render(<ConnectionForm existingIds={[]} onSaved={() => undefined} />);
    expect(screen.queryByLabelText("Provedor")).toBeNull();
    expect(screen.queryByLabelText("Versão da API")).toBeNull();
    expect(screen.getAllByText(/env:NOME/).length).toBeGreaterThan(0);
    await fill(user, { key: "env:1X" });
    await user.click(save());
    expect(screen.getByText(/Referência inválida/)).toBeTruthy();
  });

  it("nova conexão: chave enviada uma vez e fora do estado; o store recebe a conexão", async () => {
    resetStore({ config: config({ connections: [] }) });
    const fetchMock = installFetch((method) => (method === "PUT" ? { body: { id: "novo", name: "Novo", base_url: "https://api.example.com", api_key_masked: "********3456" } } : { body: {} }));
    const saved = vi.fn();
    const user = userEvent.setup();
    render(<ConnectionForm existingIds={[]} onSaved={saved} />);
    expect(screen.getByLabelText("Chave de API").getAttribute("type")).toBe("password");
    await fill(user, { id: "novo", name: "Novo", url: "https://api.example.com", key: "sk-segredo-123456" });
    await user.click(save());
    await waitFor(() => expect(saved).toHaveBeenCalled());
    expect(mutations(fetchMock)).toHaveLength(1);
    expect(JSON.parse(String(mutations(fetchMock)[0]?.[1]?.body))).toMatchObject({ id: "novo", api_key: "sk-segredo-123456" });
    expect((screen.getByLabelText("Chave de API") as HTMLInputElement).value).toBe("");
    expect(appStore.getState().config?.connections.map((c) => c.id)).toEqual(["novo"]);
  });

  it("editar envia só id, nome, URL, chave e protocolo (sem provider_type nem api_version)", async () => {
    const fetchMock = installFetch((method) => (method === "PUT" ? { body: corp } : { body: {} }));
    const user = userEvent.setup();
    render(<ConnectionForm existingIds={["openai-direct", "corp-gateway"]} initial={corp} onSaved={() => undefined} />);
    expect((screen.getByLabelText("Identificador") as HTMLInputElement).disabled).toBe(true);
    await user.click(save());
    await waitFor(() => expect(mutations(fetchMock)).toHaveLength(1));
    const body = JSON.parse(String(mutations(fetchMock)[0]?.[1]?.body)) as Record<string, unknown>;
    expect(body).toMatchObject({ id: "corp-gateway", base_url: "https://corp.example.com/v1" });
    expect(body).not.toHaveProperty("provider_type");
    expect(body).not.toHaveProperty("api_version");
  });

  it("editar trocando a base_url sem informar a chave mostra erro de campo e não envia", async () => {
    const fetchMock = installFetch(() => ({ body: {} }));
    const user = userEvent.setup();
    render(<ConnectionForm existingIds={["openai-direct"]} initial={openai} onSaved={() => undefined} />);
    const url = screen.getByLabelText("URL base");
    await user.clear(url);
    await user.type(url, "https://outro.example.org/v1");
    await user.click(save());
    expect(screen.getByText("informe a chave para o novo endereço")).toBeTruthy();
    expect(mutations(fetchMock)).toHaveLength(0);
  });

  it("editar só a barra final ou as maiúsculas do host não exige chave", async () => {
    const fetchMock = installFetch((method) => (method === "PUT" ? { body: openai } : { body: {} }));
    const user = userEvent.setup();
    render(<ConnectionForm existingIds={["openai-direct"]} initial={openai} onSaved={() => undefined} />);
    const url = screen.getByLabelText("URL base");
    await user.clear(url);
    await user.type(url, "HTTPS://API.OPENAI.COM/v1/");
    await user.click(save());
    await waitFor(() => expect(mutations(fetchMock)).toHaveLength(1));
  });

  it("protocolo: Chat Completions marcado por padrão; setas trocam a opção (grupo de rádios)", async () => {
    installFetch(() => ({ body: {} }));
    const user = userEvent.setup();
    render(<ConnectionForm existingIds={[]} onSaved={() => undefined} />);
    const chat = screen.getByRole("radio", { name: /Chat Completions/ }) as HTMLInputElement;
    const responses = screen.getByRole("radio", { name: /Responses/ }) as HTMLInputElement;
    expect(chat.checked).toBe(true);
    chat.focus();
    await user.keyboard("{ArrowDown}");
    expect(responses.checked).toBe(true);
    expect(document.activeElement).toBe(responses);
    await user.keyboard("{ArrowUp}");
    expect(chat.checked).toBe(true);
  });

  it("nova conexão com id existente é recusada com erro de campo, sem envio", async () => {
    const fetchMock = installFetch(() => ({ body: {} }));
    const user = userEvent.setup();
    render(<ConnectionForm existingIds={["openai-direct"]} onSaved={() => undefined} />);
    await fill(user, { id: "openai-direct", name: "Duplicada", url: "https://api.example.com", key: "sk-nova" });
    await user.click(save());
    expect(screen.getByText("já existe uma conexão com este id; use Editar")).toBeTruthy();
    expect(mutations(fetchMock)).toHaveLength(0);
  });
});

describe("Agentes: roteamento por papel", () => {
  it("três cartões com conexão, modelo e Low|Med|High", () => {
    routes();
    render(<AgentsForm agents={agents()} connections={[openai, corp]} />);
    for (const role of ["Arquiteto", "Desenvolvedor", "Revisor"]) {
      const region = screen.getByRole("region", { name: role });
      expect(within(region).getByLabelText(`Conexão do ${role}`)).toBeTruthy();
      expect(within(region).getByLabelText(`Modelo do ${role}`)).toBeTruthy();
      const group = within(region).getByRole("radiogroup", { name: `Esforço do ${role}` });
      expect(within(group).getAllByRole("radio").map((r) => r.parentElement?.textContent)).toEqual(["Low", "Med", "High"]);
    }
  });

  it("salva com putAgents e atualiza o store", async () => {
    resetStore({ config: config() });
    const fetchMock = routes((method, _url, body) => (method === "PUT" ? { body } : { body: {} }));
    const user = userEvent.setup();
    render(<AgentsForm agents={agents()} connections={[openai, corp]} />);
    await user.click(within(within(screen.getByRole("region", { name: "Desenvolvedor" })).getByRole("radiogroup")).getByRole("radio", { name: "High" }));
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    await waitFor(() => expect(mutations(fetchMock)).toHaveLength(1));
    expect(mutations(fetchMock)[0]?.[0]).toBe("/api/config/agents");
    expect(JSON.parse(String(mutations(fetchMock)[0]?.[1]?.body)).developer.effort).toBe("high");
    await waitFor(() => expect(appStore.getState().config?.agents.developer.effort).toBe("high"));
  });

  it("aplica as sugestões do blueprint (Arquiteto low, Desenvolvedor low, Revisor medium)", async () => {
    routes();
    const user = userEvent.setup();
    render(<AgentsForm agents={agents()} connections={[openai, corp]} />);
    const reviewer = screen.getByRole("region", { name: "Revisor" });
    await user.click(within(reviewer).getByRole("radio", { name: "High" }));
    await user.click(screen.getByRole("button", { name: "Restaurar sugestões de esforço" }));
    expect((within(reviewer).getByRole("radio", { name: "Med" }) as HTMLInputElement).checked).toBe(true);
  });

  it("avisa quando um agente referencia uma conexão que não existe mais", () => {
    routes();
    render(<AgentsForm agents={agents({ architect: { connection_id: "sumiu" } })} connections={[openai, corp]} />);
    expect(within(screen.getByRole("region", { name: "Arquiteto" })).getByRole("alert").textContent).toContain("não existe mais");
  });

  it("primeira execução: connection_id vazio não avisa, mostra o placeholder e salvar pede uma conexão sem chamar nada", async () => {
    const fetchMock = installFetch(() => ({ body: {} }));
    const user = userEvent.setup();
    const empty: Agents = {
      architect: { connection_id: "", model: "", effort: "low" },
      developer: { connection_id: "", model: "", effort: "low" },
      reviewer: { connection_id: "", model: "", effort: "medium" },
    };
    render(<AgentsForm agents={empty} connections={[]} />);
    for (const label of ["Arquiteto", "Desenvolvedor", "Revisor"]) {
      const region = screen.getByRole("region", { name: label });
      expect(within(region).queryByRole("alert")).toBeNull();
      const select = within(region).getByLabelText(`Conexão do ${label}`) as HTMLSelectElement;
      expect(select.value).toBe("");
      expect(select.options[select.selectedIndex]?.textContent).toBe("Escolha uma conexão");
    }
    expect(document.body.textContent).not.toContain("não existe mais");
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    expect(within(screen.getByRole("region", { name: "Arquiteto" })).getByText("Escolha uma conexão salva.")).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("recusa modelo vazio antes de enviar", async () => {
    const fetchMock = routes();
    const user = userEvent.setup();
    render(<AgentsForm agents={agents()} connections={[openai, corp]} />);
    const region = screen.getByRole("region", { name: "Arquiteto" });
    await user.clear(within(region).getByLabelText("Modelo do Arquiteto"));
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    expect(within(region).getByText("Informe o modelo.")).toBeTruthy();
    expect(mutations(fetchMock)).toHaveLength(0);
  });

  it("falha ao salvar mostra o erro fixo (422 vira 'dados inválidos', nunca o corpo do servidor)", async () => {
    routes((method) => (method === "PUT" ? { status: 422, body: { error: "architect: connection_id não existe" } } : { body: {} }));
    const user = userEvent.setup();
    render(<AgentsForm agents={agents()} connections={[openai, corp]} />);
    await user.click(screen.getByRole("button", { name: "Salvar agentes" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toMatch(/dados inválidos/));
    expect(screen.getByRole("alert").textContent).not.toContain("connection_id não existe");
  });
});
