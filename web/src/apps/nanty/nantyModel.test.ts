/*
 * Modelo da janela do Nanty (spec 138 T04): a leitura da API, o selo sem duplicar o sino (C-07) e
 * o subtítulo vivo.
 */
import { describe, expect, it } from "vitest";
import type { Attention, AttentionProject } from "../../desktop/api";
import { NANTY_EMPTY_FEED, NANTY_STATUS } from "../../desktop/shell-test-utils";
import { parseNantyConversation, parseNantyFeed, parseNantyStatus, parseNantyUsage, type NantyCard, type NantyDone, type NantyFeed } from "./nantyApi";
import { cardButtons, coveredByAttention, doneWho, factTexts, feedSections, linkWindow, nantyBadgeCount, nantySummary, nantyTabByKey } from "./nantyModel";

function card(group: string, projectId: string | null): NantyCard {
  return { id: `c-${group}`, project_id: projectId, group, code: `nanty.why.${group.split(":")[1] ?? ""}`, expires_at_ms: 9e12 };
}

function project(id: string, patch: Partial<AttentionProject> = {}): AttentionProject {
  return { project_id: id, name: id, pending_permissions: [], pending_choices: false, plan_awaiting_approval: false, paused_or_stalled: null, ...patch };
}

const attention = (...projects: AttentionProject[]): Attention => ({ setup_incomplete: false, projects });

const feed = (...waiting: NantyCard[]): NantyFeed => ({ waiting, today: { spend_usd: 0.06, budget_usd: 0.5 } });

describe("leitura da API", () => {
  it("status válido; estado ou instância fora da lista descarta", () => {
    expect(parseNantyStatus(NANTY_STATUS)).toEqual({
      instance: "active",
      avatar: "attentive",
      model_configured: false,
      telegram_configured: false,
      telegram_paired: false,
    });
    expect(parseNantyStatus({ ...NANTY_STATUS, avatar: "dancing" })).toBeNull();
    expect(parseNantyStatus({ ...NANTY_STATUS, instance: "x" })).toBeNull();
    expect(parseNantyStatus(null)).toBeNull();
  });

  it("feed: os cartões válidos e o gasto de hoje; cartão incompleto sai", () => {
    const raw = {
      ...NANTY_EMPTY_FEED,
      waiting: [
        { id: "a1", project_id: "p1", group: "p1:plan_waiting", code: "nanty.why.plan_waiting", params: {}, created_at_ms: 1, expires_at_ms: 2 },
        { id: "a2", group: "*:disk_low", code: "nanty.why.disk_low", created_at_ms: 1, expires_at_ms: 2 },
        { id: "", group: "x", code: "y", expires_at_ms: 2 },
      ],
    };
    expect(parseNantyFeed(raw)).toEqual({
      waiting: [
        { id: "a1", project_id: "p1", group: "p1:plan_waiting", code: "nanty.why.plan_waiting", created_at_ms: 1, expires_at_ms: 2 },
        { id: "a2", project_id: null, group: "*:disk_low", code: "nanty.why.disk_low", created_at_ms: 1, expires_at_ms: 2 },
      ],
      done: [],
      planned: [],
      today: { spend_usd: 0, budget_usd: 0.5 },
    });
    expect(parseNantyFeed({ waiting: "x" })).toBeNull();
  });
});

describe("selo sem duplicar o sino (C-07)", () => {
  it("o plano esperando aprovação só conta se o sino não o mostra", () => {
    const plan = card("p1:plan_waiting", "p1");
    expect(coveredByAttention(plan, attention(project("p1", { plan_awaiting_approval: true })))).toBe(true);
    expect(coveredByAttention(plan, attention(project("p1")))).toBe(false);
    expect(coveredByAttention(plan, attention(project("p2", { plan_awaiting_approval: true })))).toBe(false);
    expect(coveredByAttention(plan, null)).toBe(false);
  });

  it.each(["no_credit", "provider_stopped", "run_blocked", "budget", "review_loop"])("%s: coberto pela pausa ou parada do mesmo projeto", (kind) => {
    const c = card(`p1:${kind}`, "p1");
    expect(coveredByAttention(c, attention(project("p1", { paused_or_stalled: { kind: "stalled", spec_id: "s1" } })))).toBe(true);
    expect(coveredByAttention(c, attention(project("p1", { plan_awaiting_approval: true })))).toBe(false);
  });

  it.each(["cost_alert", "task_delivered", "spec_done", "app_ready"])("%s: o sino nunca mostra; sempre conta", (kind) => {
    const all = project("p1", { plan_awaiting_approval: true, pending_choices: true, paused_or_stalled: { kind: "stalled", spec_id: "s1" } });
    expect(coveredByAttention(card(`p1:${kind}`, "p1"), attention(all))).toBe(false);
  });

  it("avisos do sistema (disco, Docker) não têm projeto: sempre contam", () => {
    expect(coveredByAttention(card("*:disk_low", null), attention(project("p1", { plan_awaiting_approval: true })))).toBe(false);
  });

  it("a contagem soma só o que o sino não cobre", () => {
    const f = feed(card("p1:plan_waiting", "p1"), card("p1:cost_alert", "p1"), card("*:docker_orphans", null), card("p2:no_credit", "p2"));
    const bell = attention(project("p1", { plan_awaiting_approval: true }), project("p2", { paused_or_stalled: { kind: "paused", spec_id: "s", task_id: "T01" } }));
    expect(nantyBadgeCount(f, bell)).toBe(2);
    expect(nantyBadgeCount(f, null)).toBe(4);
    expect(nantyBadgeCount(null, bell)).toBe(0);
  });
});

describe("subtítulo e selo", () => {
  it("o estado do avatar; com o modelo escolhido, o gasto de hoje", () => {
    const status = { ...NANTY_STATUS, avatar: "thinking" as const, instance: "active" as const };
    expect(nantySummary({ status, feed: feed(), attention: null })).toEqual({ subtitle: "pensando" });
    expect(nantySummary({ status: { ...status, model_configured: true }, feed: feed(), attention: null }).subtitle).toBe("pensando · US$ 0,06 de US$ 0,50 hoje");
  });

  it("sem leitura, atento; em outra instância, diz onde está ativo", () => {
    expect(nantySummary({ status: null, feed: null, attention: null })).toEqual({ subtitle: "atento" });
    const other = { ...NANTY_STATUS, instance: "other_instance" as const, avatar: "sleeping" as const };
    expect(nantySummary({ status: other, feed: null, attention: null }).subtitle).toBe("ativo em outra instância do Nants");
  });

  it("o selo leva a contagem e o texto acessível no plural certo", () => {
    expect(nantySummary({ status: null, feed: feed(card("*:disk_low", null)), attention: null }).badge).toEqual({ count: 1, label: "1 aviso esperando você" });
    expect(nantySummary({ status: null, feed: feed(card("*:disk_low", null), card("*:tmp_low", null)), attention: null }).badge?.label).toBe("2 avisos esperando você");
  });

  it("abas pelas setas, Home e End, com volta", () => {
    expect(nantyTabByKey("feed", "ArrowRight")).toBe("rules");
    expect(nantyTabByKey("feed", "ArrowLeft")).toBe("settings");
    expect(nantyTabByKey("rules", "End")).toBe("settings");
    expect(nantyTabByKey("settings", "Home")).toBe("feed");
    expect(nantyTabByKey("feed", "a")).toBeNull();
  });
});

describe("cabeçalho (T10): o estado, os projetos, o gasto e o Telegram", () => {
  const budget = { mode: "usd", day: "d", spent_usd: 0.06, budget_usd: 0.5, tokens: 900, budget_tokens: 200000, unpriced_turns: 0, unpriced: false, exhausted: false };
  const full = { ...NANTY_STATUS, model_configured: true, telegram_configured: true, telegram_paired: true, telegram: "connected", projects_observed: 4, budget };

  it("lê os campos novos do status; campo estranho fica de fora", () => {
    const status = parseNantyStatus(full);
    expect(status?.projects_observed).toBe(4);
    expect(status?.telegram).toBe("connected");
    expect(status?.budget?.spent_usd).toBe(0.06);
    expect(parseNantyStatus({ ...full, telegram: "x", projects_observed: -1, budget: { mode: "x" } })).toEqual({
      instance: "active",
      avatar: "attentive",
      model_configured: true,
      telegram_configured: true,
      telegram_paired: true,
    });
  });

  it("atento · 4 projetos · gasto em US$ · Telegram conectado", () => {
    expect(nantySummary({ status: parseNantyStatus(full), feed: null, attention: null }).subtitle).toBe("atento · 4 projetos · US$ 0,06 de US$ 0,50 hoje · Telegram conectado");
  });

  it("em tokens sem preço; Telegram sem pareamento, sem token ou com problema", () => {
    const tokens = parseNantyStatus({ ...full, projects_observed: 1, budget: { ...budget, mode: "tokens", tokens: 1500 }, telegram_paired: false });
    expect(nantySummary({ status: tokens, feed: null, attention: null }).subtitle).toBe("atento · 1 projeto · 1.500 de 200.000 tokens hoje · Telegram sem pareamento");
    const off = parseNantyStatus({ ...full, telegram_configured: false, telegram: "off", model_configured: false, budget: { ...budget, spent_usd: 0 } });
    expect(nantySummary({ status: off, feed: null, attention: null }).subtitle).toBe("atento · 4 projetos · sem Telegram");
    const bad = parseNantyStatus({ ...full, telegram: "invalid_token" });
    expect(nantySummary({ status: bad, feed: null, attention: null }).subtitle).toContain("Telegram recusou o token");
  });
});

const NOW = new Date(2026, 9, 30, 15, 0).getTime();
const done = (patch: Partial<NantyDone>): NantyDone => ({
  at_ms: NOW - 60_000,
  project_id: "p1",
  code: "nanty.action.swap_reserve",
  params: {},
  error_params: {},
  action: "swap_reserve",
  decided_by: "auto",
  result: "ok",
  undo: null,
  ...patch,
});

describe("feed (T10): seções, botões e fatos", () => {
  it("lê o feito, o previsto e o desfazer; item incompleto sai", () => {
    const feed = parseNantyFeed({
      ...NANTY_EMPTY_FEED,
      done: [
        { at_ms: 5, code: "nanty.action.swap_reserve", result: "ok", decided_by: "auto", params: { role: "developer" }, undo: { id: "0123456789abcdef", token: "t", expires_at_ms: 9 } },
        { at_ms: 6, code: "nanty.action.retry_provider", result: "failed", decided_by: "eu", params: { error: "internal", error_params: { a: 1 } } },
        { code: "sem hora", result: "ok" },
      ],
      planned: [{ action: "retry_provider", code: "nanty.action.retry_provider", at_ms: 7, params: { attempt: 2 } }, { action: "x" }],
    });
    expect(feed?.done).toEqual([
      { at_ms: 5, project_id: null, code: "nanty.action.swap_reserve", params: { role: "developer" }, error_params: {}, action: null, decided_by: "auto", result: "ok", undo: { id: "0123456789abcdef", token: "t", expires_at_ms: 9 } },
      { at_ms: 6, project_id: null, code: "nanty.action.retry_provider", params: { error: "internal" }, error_params: { a: 1 }, action: null, decided_by: null, result: "failed", undo: null },
    ]);
    expect(feed?.planned).toEqual([{ project_id: null, action: "retry_provider", code: "nanty.action.retry_provider", params: { attempt: 2 }, at_ms: 7 }]);
  });

  it("feito sozinho hoje (ou com desfazer válido) em Feito por conta própria; o resto do dia em Hoje", () => {
    const yesterday = NOW - 24 * 3600_000;
    const feed: NantyFeed = {
      waiting: [card("p1:plan_waiting", "p1")],
      done: [
        done({}),
        done({ at_ms: yesterday, undo: { id: "0123456789abcdef", token: "t", expires_at_ms: NOW + 1000 } }),
        done({ at_ms: yesterday }),
        done({ decided_by: "web", code: "nanty.action.approve_plan" }),
        done({ decided_by: null, result: "resolved", code: "nanty.why.disk_low" }),
      ],
      planned: [
        { project_id: null, action: "b", code: "c", params: {}, at_ms: NOW + 2000 },
        { project_id: null, action: "a", code: "c", params: {}, at_ms: NOW + 1000 },
      ],
      today: { spend_usd: 0, budget_usd: 0 },
    };
    const s = feedSections(feed, NOW);
    expect(s.waiting).toHaveLength(1);
    expect(s.auto).toHaveLength(2);
    expect(s.today.map((d) => d.result)).toEqual(["ok", "resolved"]);
    expect(s.planned.map((p) => p.action)).toEqual(["a", "b"]);
    expect(feedSections(null, NOW)).toEqual({ waiting: [], auto: [], planned: [], today: [] });
  });

  it("os botões do Telegram: a ação, não e lembrar depois; sem token, nenhum; faixa da pessoa sem o sim", () => {
    const base = { ...card("p1:plan_waiting", "p1"), token: "tk" };
    expect(cardButtons({ ...base, action: "approve_plan" }).map((b) => [b.choice, b.label])).toEqual([
      ["yes", "Aprovar o plano"],
      ["no", "Não"],
      ["later", "Lembrar depois"],
    ]);
    expect(cardButtons({ ...base }).map((b) => b.choice)).toEqual(["later"]);
    expect(cardButtons({ ...base, action: "change_channel" }).map((b) => b.choice)).toEqual(["no", "later"]);
    expect(cardButtons({ ...base, action: "open_nants" }).map((b) => b.choice)).toEqual(["open", "no", "later"]);
    expect(cardButtons({ ...base, project_id: null, action: "open_nants" }).map((b) => b.choice)).toEqual(["no", "later"]);
    expect(cardButtons(card("p1:plan_waiting", "p1"))).toEqual([]);
    expect(linkWindow(base)).toBe("plan");
    expect(linkWindow(card("p1:cost_alert", "p1"))).toBe("costs");
    expect(linkWindow(card("p1:run_blocked", "p1"))).toBe("execution");
  });

  it("os fatos só com contagens e códigos (C-18), e quem decidiu", () => {
    const role = (r: string): string => `papel-${r}`;
    expect(factTexts({ spec_id: "s1", task_id: "T03", attempts: 2, role: "developer", stop: "run.billing", free_gb: 8, age_min: 130, observer: "attention", since: "10:00" }, role)).toEqual([
      "Spec s1",
      "Tarefa T03",
      "2 tentativas",
      "Papel: papel-developer",
      "Parada: run.billing",
      "8 GB livres",
      "há 2 h",
    ]);
    expect(factTexts({ projects: 1, networks: 2, age_min: 5 }, role)).toEqual(["1 projeto", "2 redes", "há 5 min"]);
    expect(doneWho(done({}))).toBe("Feito, pelo Nanty, sozinho");
    expect(doneWho(done({ decided_by: "telegram", result: "failed" }))).toBe("Não deu certo, por você, no Telegram");
    expect(doneWho(done({ decided_by: null, result: "resolved" }))).toBe("Resolvido");
    expect(doneWho(done({ decided_by: "web", result: "outro" }))).toBe("Registrado, por você, na janela");
  });
});

describe("conversa e gasto (T10): leitura", () => {
  it("mensagens com a origem, as linhas, a voz e o áudio; o que não confere sai", () => {
    const messages = parseNantyConversation({
      messages: [
        { id: "m1", at_ms: 1, from: "person", channel: "telegram", text: "oi", voice: true, audio: { id: "0123456789abcdef", duration_secs: 3, bytes: 900 } },
        { id: "m2", at_ms: 2, from: "nanty", channel: "web", code: "nanty.chat.status", params: { waiting: 1, lines: [{ code: "nanty.chat.status_no_model" }, { code: "X" }] }, card_ids: ["c1", 3] },
        { id: "m3", at_ms: 3, from: "alguem", channel: "web" },
        { id: "m4", at_ms: 4, from: "nanty", channel: "telegram", audio: { id: "../x" } },
      ],
    });
    expect(messages?.map((m) => m.id)).toEqual(["m1", "m2", "m4"]);
    expect(messages?.[0]).toMatchObject({ voice: true, text: "oi", audio: { id: "0123456789abcdef", duration_secs: 3, bytes: 900 } });
    expect(messages?.[1]).toMatchObject({ code: "nanty.chat.status", params: { waiting: 1 }, lines: [{ code: "nanty.chat.status_no_model", params: {} }], card_ids: ["c1"], audio: null });
    expect(messages?.[2]?.audio).toBeNull();
    expect(parseNantyConversation({})).toBeNull();
  });

  it("o gasto de hoje e dos dias anteriores, com as chamadas de voz", () => {
    const usage = parseNantyUsage({ budget: null, today: { day: "2026-10-30", usd: 0.02, input_tokens: 10, output_tokens: 5, voice_calls: 2 }, history: [{ day: "2026-10-29", usd: 0.1 }] });
    expect(usage?.today).toEqual({ day: "2026-10-30", usd: 0.02, input_tokens: 10, output_tokens: 5, unpriced_turns: 0, voice_calls: 2 });
    expect(usage?.history[0]?.usd).toBe(0.1);
    expect(usage?.budget).toBeNull();
    expect(parseNantyUsage({ today: null })).toBeNull();
  });
});
