/*
 * Preços por modelo nas Configurações (`PricingEditor`): só linhas preenchidas, validação, origem
 * manual ou estimada, "US$ por imagem", estimativas do catálogo e a formatação dos valores.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { formatTokens, formatUsd } from "../../usageFormat";
import { installFetch, makeConfig, resetStore } from "../../test-utils";
import { PricingEditor } from "./PricingEditor";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("PricingEditor", () => {
  const agent = { connection_id: "c", model: "gpt-x", effort: "low" as const };
  const agents = { architect: agent, developer: agent, reviewer: agent };
  const bodyOf = (call: unknown[] | undefined): unknown => JSON.parse(((call ?? [])[1] as RequestInit).body as string);
  const puts = (m: ReturnType<typeof installFetch>) =>
    m.mock.calls.filter(([, init]) => (init as RequestInit | undefined)?.method === "PUT");

  it("envia só linhas preenchidas, valida números e marca digitado como manual", async () => {
    resetStore({ config: makeConfig([]) });
    const fetchMock = installFetch((method, _u, body) => ({ body: method === "PUT" ? body : {} }));
    render(<PricingEditor agents={agents} pricing={undefined} />);
    await userEvent.click(screen.getByRole("button", { name: "Salvar preços" }));
    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Preços salvos."));
    expect(bodyOf(puts(fetchMock)[0])).toEqual({});

    await userEvent.type(screen.getByLabelText("Entrada de gpt-x"), "3");
    await userEvent.click(screen.getByRole("button", { name: "Salvar preços" }));
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Preço inválido"));
    expect(puts(fetchMock)).toHaveLength(1);

    await userEvent.type(screen.getByLabelText("Saída de gpt-x"), "15,5");
    await userEvent.type(screen.getByLabelText("Cache de gpt-x"), "0,3");
    await userEvent.click(screen.getByRole("button", { name: "Salvar preços" }));
    await waitFor(() => expect(puts(fetchMock)).toHaveLength(2));
    expect(bodyOf(puts(fetchMock)[1])).toEqual({
      "gpt-x": { input_per_mtok: 3, output_per_mtok: 15.5, cached_input_per_mtok: 0.3, source: "manual" },
    });
  });

  // Spec 092 T03: "US$ por imagem" opcional; não muda a origem dos preços de tokens.
  it("US$ por imagem é opcional, validado e enviado como image_per_unit", async () => {
    resetStore({ config: makeConfig([]) });
    const est = { input_per_mtok: 2.5, output_per_mtok: 10, source: "estimated" as const, estimated_at: "2026-10-15" };
    const fetchMock = installFetch((method, _u, body) => (method === "POST" ? { body: { "gpt-x": est } } : { body }));
    render(<PricingEditor agents={agents} pricing={undefined} />);
    await waitFor(() => expect((screen.getByLabelText("Entrada de gpt-x") as HTMLInputElement).value).toBe("2,5"));
    const field = screen.getByLabelText("US$ por imagem de gpt-x") as HTMLInputElement;
    expect(field.value).toBe("");
    expect(screen.getByText("US$ por imagem (opcional)")).toBeTruthy();
    await userEvent.type(field, "x");
    await userEvent.click(screen.getByRole("button", { name: "Salvar preços" }));
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Preço inválido"));
    expect(puts(fetchMock)).toHaveLength(0);
    await userEvent.clear(field);
    await userEvent.type(field, "0,04");
    expect(screen.getByText("estimado (2026-10-15)")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Salvar preços" }));
    await waitFor(() => expect(puts(fetchMock)).toHaveLength(1));
    expect(bodyOf(puts(fetchMock)[0])).toEqual({ "gpt-x": { ...est, image_per_unit: 0.04 } });
    cleanup();
    // Valor gravado aparece no campo.
    installFetch(() => ({ body: {} }));
    render(<PricingEditor agents={agents} pricing={{ "gpt-x": { input_per_mtok: 1, output_per_mtok: 2, image_per_unit: 0.25 } }} />);
    expect((screen.getByLabelText("US$ por imagem de gpt-x") as HTMLInputElement).value).toBe("0,25");
  });

  it("falha do autofill mostra aviso discreto em vez de silencio", async () => {
    resetStore({ config: makeConfig([]) });
    installFetch((method) => (method === "POST" ? { status: 415, body: { error: "x" } } : { body: {} }));
    render(<PricingEditor agents={agents} pricing={undefined} />);
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("Não foi possível buscar estimativas"));
  });

  it("pré-cadastra a estimativa do modelo em uso, mostra o selo e preserva a origem ao salvar", async () => {
    resetStore({ config: makeConfig([]) });
    const est = { input_per_mtok: 2.5, output_per_mtok: 10, source: "estimated" as const, estimated_at: "2026-10-15" };
    const fetchMock = installFetch((method, url, body) => {
      if (method === "POST") {
        expect(url).toContain("/api/config/pricing/autofill");
        return { body: { "gpt-x": est } };
      }
      return { body };
    });
    render(<PricingEditor agents={agents} pricing={undefined} />);
    await waitFor(() => expect((screen.getByLabelText("Entrada de gpt-x") as HTMLInputElement).value).toBe("2,5"));
    expect(screen.getByText("estimado (2026-10-15)")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Salvar preços" }));
    await waitFor(() => expect(puts(fetchMock)).toHaveLength(1));
    expect(bodyOf(puts(fetchMock)[0])).toEqual({ "gpt-x": est });
    // Editar vira manual.
    await userEvent.type(screen.getByLabelText("Entrada de gpt-x"), "1");
    expect(screen.getByText("manual")).toBeTruthy();
  });

  it("Usar estimativa preenche a linha; sem estimativa avisa e não inventa valores", async () => {
    resetStore({ config: makeConfig([]) });
    let found = true;
    installFetch((method, url) => {
      if (method === "POST") return { body: {} };
      expect(url).toContain("/api/config/pricing/estimate?models=gpt-x");
      return {
        body: found ? { "gpt-x": { input_per_mtok: 1, output_per_mtok: 4, cached_input_per_mtok: 0.1, source: "estimated", estimated_at: "2026-10-15" } } : {},
      };
    });
    render(<PricingEditor agents={agents} pricing={undefined} />);
    expect(screen.getByText("sem preço")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Usar estimativa de gpt-x" }));
    await waitFor(() => expect((screen.getByLabelText("Saída de gpt-x") as HTMLInputElement).value).toBe("4"));
    expect((screen.getByLabelText("Cache de gpt-x") as HTMLInputElement).value).toBe("0,1");
    cleanup();
    found = false;
    render(<PricingEditor agents={agents} pricing={undefined} />);
    await screen.findByText("sem estimativa no catálogo");
    expect(screen.queryByRole("button", { name: "Usar estimativa de gpt-x" })).toBeNull();
    expect((screen.getByLabelText("Entrada de gpt-x") as HTMLInputElement).value).toBe("");
  });

  it("Atualizar estimativas não troca preço manual digitado", async () => {
    resetStore({ config: makeConfig([]) });
    const fetchMock = installFetch((method, url) => {
      if (method === "POST" && url.includes("refresh=true")) {
        return { body: { "gpt-x": { input_per_mtok: 9, output_per_mtok: 9, source: "estimated", estimated_at: "d" } } };
      }
      return { body: {} };
    });
    render(<PricingEditor agents={agents} pricing={{ "gpt-x": { input_per_mtok: 1, output_per_mtok: 2 } }} />);
    expect(screen.getByText("manual")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Atualizar estimativas" }));
    await waitFor(() => expect(screen.getByRole("status").textContent).toContain("atualizadas"));
    expect((screen.getByLabelText("Entrada de gpt-x") as HTMLInputElement).value).toBe("1");
    expect(fetchMock.mock.calls.some(([u]) => String(u).includes("refresh=true"))).toBe(true);
  });
});

describe("valores pequenos", () => {
  it("custo abaixo de 1 centavo não vira US$ 0,00", () => {
    expect(formatUsd(0.0054)).toBe("US$ 0,0054");
    expect(formatUsd(0)).toBe("US$ 0,00");
  });
});

describe("formatação dos tokens", () => {
  it("compacta tokens", () => {
    expect(formatTokens(1_200_000)).toBe("1,2M");
  });
});
