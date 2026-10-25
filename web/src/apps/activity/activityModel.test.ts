/* Regras puras da janela Atividade (spec 093, T07): ação ao vivo e histórico de informativos. */
import { describe, expect, it } from "vitest";
import type { SpaceData } from "../../desktop/useSpaceData";
import type { ActivityEntry, ChatMessage } from "../../store";
import { countsTowardAttention, INFO_KINDS } from "../attention/attentionModel";
import { setLanguage } from "../../i18n";
import { activitySummary, cardAgents, elapsedText, historyItems, imageManifestPath, latestByAgent, liveActionOf, parseImageManifest } from "./activityModel";

function entry(extra: Partial<ActivityEntry>): ActivityEntry {
  return { key: "k", agent: "developer", activityId: "a1", reasoning: "", text: "", tool: "", startedAt: 1, lastAt: 1, ...extra };
}

describe("ação ao vivo", () => {
  it("gerando imagem, ajustando o ritmo e repetindo viram estados próprios", () => {
    // Spec 102 T06: o servidor manda `code`/`params` junto do texto de reserva.
    const image = { code: "tool.generate_image", params: { path: "web/img/gato.webp" } };
    expect(liveActionOf(entry({ tool: "gerando imagem web/img/gato.webp", toolCode: image }))).toMatchObject({ kind: "image", state: "gerando imagem", detail: "web/img/gato.webp" });
    const pace = { code: "pace.waiting", params: { seconds: "12" } };
    expect(liveActionOf(entry({ text: "ajustando o ritmo ao limite do provedor: 12 s", textCode: pace }))).toMatchObject({
      kind: "pacing",
      state: "ajustando o ritmo",
      detail: "ajustando o ritmo ao limite do provedor: 12 s",
    });
    const throttled = { code: "pace.throttled", params: { label: "HTTP 429", seconds: 5, attempt: 2, total: 6 } };
    expect(liveActionOf(entry({ text: "x", textCode: throttled }))).toMatchObject({ kind: "pacing", detail: "provedor no limite (HTTP 429); aguardando 5 s (tentativa 2/6)" });
    // Sem `code` (servidor sem a T06), o rótulo aparece como veio, como qualquer ferramenta.
    expect(liveActionOf(entry({ tool: "gerando imagem web/img/gato.webp" }))).toMatchObject({ kind: "tool", detail: "gerando imagem web/img/gato.webp" });
    expect(liveActionOf(entry({ text: "resposta do provedor falhou (incompleta: content_filter); repetindo (1/2)" }))).toMatchObject({ kind: "retry", state: "repetindo" });
  });

  it("comando, testes, ferramenta, texto e raciocínio", () => {
    expect(liveActionOf(entry({ command: "npm test" }))).toMatchObject({ kind: "command", detail: "npm test" });
    expect(liveActionOf(entry({ agent: "tests", command: "make test" }))).toMatchObject({ kind: "tests", state: "rodando os testes" });
    expect(liveActionOf(entry({ tool: "escrevendo src/a.ts" }))).toMatchObject({ kind: "tool", detail: "escrevendo src/a.ts" });
    expect(liveActionOf(entry({ text: "linha 1\nlinha 2" }))).toMatchObject({ kind: "writing", detail: "linha 1 linha 2" });
    expect(liveActionOf(entry({ reasoning: "pensando em algo" }))).toMatchObject({ kind: "thinking", detail: "pensando em algo" });
    expect(liveActionOf(entry({ stage: "final_verification" })).stage).toBe("Verificação final em andamento");
  });

  it("um cartão por agente: a atividade mais recente de cada um; outros papéis só quando ativos", () => {
    const latest = latestByAgent([
      entry({ key: "a", agent: "developer", taskId: "T1", lastAt: 1 }),
      entry({ key: "b", agent: "developer", taskId: "T2", lastAt: 5 }),
      entry({ key: "c", agent: "tests", lastAt: 3 }),
      entry({ key: "d", agent: "tests", command: "make test", lastAt: 4 }),
    ]);
    expect(latest.get("developer")?.taskId).toBe("T2");
    expect(latest.get("tests")?.command).toBe("make test");
    expect(cardAgents(latest)).toEqual(["developer", "reviewer", "architect", "tests"]);
    expect(cardAgents(new Map())).toEqual(["developer", "reviewer", "architect"]);
  });
});

describe("chamada ao modelo em curso (spec 114 T04)", () => {
  const NOW = 10_000_000;
  it("antes do primeiro byte: 'aguardando o modelo há 2 min'", () => {
    const live = liveActionOf(entry({ modelStartedAt: NOW - 125_000 }), NOW);
    expect(live).toMatchObject({ kind: "model", state: "aguardando o modelo", detail: "há 2 min", model: null });
  });

  it("com bytes chegando e nada de texto: o tempo e o tamanho reais", () => {
    const live = liveActionOf(entry({ modelStartedAt: NOW - 510_000, modelBytes: 1_258_291 }), NOW);
    expect(live).toMatchObject({ kind: "model", state: "recebendo a resposta", detail: "há 8 min · 1,2 MB recebidos" });
  });

  it("com raciocínio ou texto: a linha mostra o que chegou e a chamada vai à parte", () => {
    const thinking = liveActionOf(entry({ reasoning: "lendo o plano", modelStartedAt: NOW - 30_000, modelBytes: 2048 }), NOW);
    expect(thinking).toMatchObject({ kind: "thinking", detail: "lendo o plano", model: "recebendo a resposta há 30 s · 2 KB recebidos" });
    const writing = liveActionOf(entry({ text: "pronto", modelStartedAt: NOW - 4_000 }), NOW);
    expect(writing).toMatchObject({ kind: "writing", model: "aguardando o modelo há 4 s" });
    // Sem chamada em curso, nada muda.
    expect(liveActionOf(entry({ reasoning: "x" }), NOW).model).toBeNull();
    expect(liveActionOf(entry({}), NOW)).toMatchObject({ kind: "thinking", model: null });
  });

  it("tempo curto nos dois idiomas", () => {
    expect(elapsedText(59_999)).toBe("59 s");
    expect(elapsedText(60_000)).toBe("1 min");
    expect(elapsedText(3_900_000)).toBe("1 h 5 min");
    setLanguage("en", { persist: false });
    try {
      expect(liveActionOf(entry({ modelStartedAt: NOW - 125_000 }), NOW)).toMatchObject({ state: "waiting for the model", detail: "for 2 min" });
      expect(liveActionOf(entry({ modelStartedAt: NOW - 510_000, modelBytes: 1_258_291 }), NOW)).toMatchObject({ state: "receiving the response", detail: "for 8 min · 1.2 MB received" });
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });
});

describe("comando e testes em curso: sinal de vida real (spec 123 T01)", () => {
  const NOW = 10_000_000;
  it("comando do agente: há quanto tempo, quanto saiu e a última linha", () => {
    const live = liveActionOf(entry({ command: "npx playwright test", commandStartedAt: NOW - 185_000, commandBytes: 14_336, commandLine: "  ok 12 cena.spec.ts:40 desenha o voxel (2.1s)" }), NOW);
    expect(live).toMatchObject({
      kind: "command",
      state: "executando",
      detail: "npx playwright test",
      progress: "há 3 min · 14 KB de saída",
      output: "ok 12 cena.spec.ts:40 desenha o voxel (2.1s)",
    });
  });

  it("rodada de testes do runner: o mesmo sinal; antes da saída, só o tempo", () => {
    const live = liveActionOf(entry({ agent: "tests", command: "npm test", commandStartedAt: NOW - 420_000 }), NOW);
    expect(live).toMatchObject({ kind: "tests", state: "rodando os testes", progress: "há 7 min", output: null });
    const fed = liveActionOf(entry({ agent: "tests", command: "npm test", commandStartedAt: NOW - 420_000, commandBytes: 900, commandLine: "Tests: 41 passed" }), NOW);
    expect(fed).toMatchObject({ progress: "há 7 min · 900 B de saída", output: "Tests: 41 passed" });
  });

  it("fora do comando nada muda; em inglês", () => {
    expect(liveActionOf(entry({ reasoning: "x" }), NOW)).toMatchObject({ progress: null, output: null });
    setLanguage("en", { persist: false });
    try {
      const live = liveActionOf(entry({ command: "cargo test", commandStartedAt: NOW - 45_000, commandBytes: 2_048, commandLine: "test a::b ... ok" }), NOW);
      expect(live).toMatchObject({ state: "running", progress: "for 45 s · 2 KB of output", output: "test a::b ... ok" });
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });
});

describe("rodada de testes com o limite do servidor (spec 123 T02)", () => {
  const NOW = 10_000_000;
  const tests = (elapsed: number, limit: number | undefined) =>
    liveActionOf(entry({ agent: "tests", command: "npm test", commandStartedAt: NOW - elapsed, commandLine: "ok 30 cena", ...(limit !== undefined ? { commandLimitMs: limit } : {}) }), NOW);

  it("'rodando os testes há 7 min (limite de 10 min)', com a última linha; sem aviso longe do limite", () => {
    expect(tests(420_000, 600_000)).toMatchObject({ state: "rodando os testes", progress: "há 7 min (limite de 10 min)", output: "ok 30 cena", warning: null });
  });

  it("a partir de 80% do limite, o aviso discreto; o limite é o que veio, não um fixo", () => {
    expect(tests(479_000, 600_000).warning).toBeNull();
    expect(tests(480_000, 600_000)).toMatchObject({ progress: "há 8 min (limite de 10 min)", warning: "perto do limite de tempo" });
    expect(tests(100_000, 120_000)).toMatchObject({ progress: "há 1 min (limite de 2 min)", warning: "perto do limite de tempo" });
    // Sem limite conhecido: só o tempo, nunca o aviso.
    expect(tests(3_000_000, undefined)).toMatchObject({ progress: "há 50 min", warning: null });
  });

  it("comando do agente com o limite do servidor (5 min): o mesmo tempo contra o limite e o aviso", () => {
    const cmd = (elapsed: number) =>
      liveActionOf(entry({ command: "npx playwright test", commandStartedAt: NOW - elapsed, commandLimitMs: 300_000, commandLine: "ok 3 cena" }), NOW);
    expect(cmd(130_000)).toMatchObject({ kind: "command", state: "executando", progress: "há 2 min (limite de 5 min)", warning: null });
    expect(cmd(250_000)).toMatchObject({ progress: "há 4 min (limite de 5 min)", warning: "perto do limite de tempo" });
  });

  it("em inglês", () => {
    setLanguage("en", { persist: false });
    try {
      expect(tests(540_000, 600_000)).toMatchObject({ state: "running the tests", progress: "for 9 min (limit 10 min)", warning: "close to the time limit" });
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });
});

describe("histórico de informativos", () => {
  const msg = (extra: Partial<ChatMessage>): ChatMessage => ({ id: 1, role: "agent", agent: "reviewer", text: "", at: 1000, specId: "s1", ...extra }) as ChatMessage;
  const line = (path: string, task: string, at: string, extra: Record<string, unknown> = {}): string =>
    JSON.stringify({ path, hash: "a".repeat(64), prompt: "gato", bytes: 10, task, at, ...extra });

  it("manifesto: lê as linhas do servidor e ignora as inválidas", () => {
    const text = [
      line("web/a.webp", "T2", "2026-10-19T00:00:10Z"),
      line("web/b.png", "T2", "2026-10-19T00:00:20Z", { reused: true }),
      line("web/c.png", "T3", "2026-10-19T00:00:30Z", { stored: false }),
      "{lixo",
      JSON.stringify({ path: 3 }),
      "",
    ].join("\n");
    expect(parseImageManifest(text)).toEqual([
      { path: "web/a.webp", task: "T2", at: Date.parse("2026-10-19T00:00:10Z"), stored: true, reused: false },
      { path: "web/b.png", task: "T2", at: Date.parse("2026-10-19T00:00:20Z"), stored: true, reused: true },
      { path: "web/c.png", task: "T3", at: Date.parse("2026-10-19T00:00:30Z"), stored: false, reused: false },
    ]);
    expect(imageManifestPath("093-x")).toBe(".sdd/specs/093-x/images.jsonl");
  });

  it("aprovações do Revisor e só as imagens gravadas (stored), do mais novo ao mais antigo", () => {
    const images = parseImageManifest(
      [line("web/a.webp", "T2", "1970-01-01T00:00:09Z"), line("web/grande.png", "T2", "1970-01-01T00:00:10Z", { stored: false })].join("\n"),
    );
    const items = historyItems(
      [
        msg({ id: 1, text: "Aprovado — tudo certo", taskId: "T1", attempt: 2, at: 5_000 }),
        msg({ id: 2, text: "Pediu ajustes — falta teste", taskId: "T2", at: 6_000 }),
        msg({ id: 3, text: "Aprovado — de outra spec", taskId: "T9", specId: "s0", at: 7_000 }),
      ],
      images,
      "s1",
    );
    expect(items.map((i) => [i.kind, i.title, i.detail])).toEqual([
      ["image_generated", "Imagem gerada · T2", "web/a.webp"],
      ["reviewer_approved", "Revisor aprovou T1", "tentativa 2"],
    ]);
  });

  it("sem linha gravada no manifesto não há imagem (chamada em curso, falha só na trilha, 4ª falha sem aviso)", () => {
    // A trilha teria "gerando imagem web/x.png" (e talvez "geração de imagem falhou (...)"), mas
    // nenhuma delas é evidência: o histórico só lê o manifesto.
    expect(historyItems([], [], "s1")).toEqual([]);
    expect(historyItems([], parseImageManifest(line("web/x.png", "T1", "2026-10-19T00:00:00Z", { stored: false })), "s1")).toEqual([]);
  });

  it("nenhum informativo conta no sino", () => {
    for (const kind of INFO_KINDS) expect(countsTowardAttention(kind)).toBe(false);
    const items = historyItems([msg({ text: "Aprovado — ok", taskId: "T1" })], parseImageManifest(line("x.png", "T1", "2026-10-19T00:00:00Z")), "s1");
    expect(items.length).toBe(2);
    for (const item of items) expect(countsTowardAttention(item.kind)).toBe(false);
  });
});

describe("resumo da Atividade", () => {
  it("subtítulo pelo estado da execução", () => {
    const s = (extra: Partial<SpaceData>): SpaceData => ({ running: false, architectTurnActive: false, paused: false, stalled: false, ...extra }) as SpaceData;
    expect(activitySummary({ space: null, readiness: null, projectCount: 0 })).toEqual({});
    expect(activitySummary({ space: s({ running: true }), readiness: null, projectCount: 1 }).subtitle).toBe("ao vivo");
    expect(activitySummary({ space: s({ architectTurnActive: true }), readiness: null, projectCount: 1 }).subtitle).toBe("Arquiteto trabalhando");
    expect(activitySummary({ space: s({ stalled: true }), readiness: null, projectCount: 1 }).subtitle).toBe("execução parada");
    expect(activitySummary({ space: s({}), readiness: null, projectCount: 1 }).subtitle).toBe("agentes parados");
  });
});
