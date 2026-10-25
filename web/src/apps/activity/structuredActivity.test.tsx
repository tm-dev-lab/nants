// @vitest-environment jsdom
/*
 * Spec 121: a Atividade nunca mostra a saída estruturada crua. O tipo da chamada vem do servidor
 * (`structured`) e vira um rótulo do dicionário; sem código, o texto que começa como JSON vira
 * "preparando a resposta" só na prévia da Atividade. O texto normal não muda.
 */
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ActivitySnapshot, AgentActivityPayload, ApiEvent } from "../../api/types";
import { setLanguage, type Lang } from "../../i18n";
import { en } from "../../i18n/en";
import { ptBR } from "../../i18n/ptBR";
import { appStore, createAppStore, mergeServerActivities, type ActivityEntry } from "../../store";
import { parseChoices } from "../../choices";
import { ActivityBubbles } from "../shared/ActivityBubble";
import { draftText, looksStructured, structuredText } from "../shared/activityText";
import { liveActionOf } from "./activityModel";

afterEach(() => {
  cleanup();
  appStore.setState({ activities: {} });
  setLanguage("pt-BR", { persist: false });
});

function entry(extra: Partial<ActivityEntry>): ActivityEntry {
  return { key: "k", agent: "reviewer", activityId: "a1", reasoning: "", text: "", tool: "", startedAt: 1, lastAt: 1, ...extra };
}

const CODES = ["plan_review", "verdict", "final_check", "diagnosis", "recovery", "summary", "arbitration"] as const;
const EXPECTED: Record<Lang, Record<(typeof CODES)[number] | "default", string>> = {
  "pt-BR": {
    plan_review: "preparando a revisão do plano",
    verdict: "preparando o parecer",
    final_check: "conferindo os critérios",
    diagnosis: "preparando o diagnóstico",
    recovery: "preparando a decisão sobre a tarefa",
    summary: "resumindo o histórico",
    arbitration: "decidindo os apontamentos contestados",
    default: "preparando a resposta",
  },
  en: {
    plan_review: "preparing the plan review",
    verdict: "preparing the verdict",
    final_check: "checking the criteria",
    diagnosis: "preparing the diagnosis",
    recovery: "preparing the decision on the task",
    summary: "summarizing the history",
    arbitration: "deciding the contested findings",
    default: "preparing the response",
  },
};

/** Textos crus de saída estruturada que já passaram pela prévia. */
const RAW = ['{"issues": []}', '{"verdict":"APPROVED","summary_message":"ok","findings":[]}', '[{"id":"F-01"}]', '```json\n{"decision": "ESCALATE"}\n```', '"root_cause": "x", "what_to_do": "y"}'];

describe("Atividade sem JSON cru (spec 121)", () => {
  for (const lang of ["pt-BR", "en"] as const) {
    it(`cada tipo marcado mostra o rótulo do dicionário, nunca o texto (${lang})`, () => {
      setLanguage(lang, { persist: false });
      for (const code of CODES) {
        for (const text of RAW) {
          const live = liveActionOf(entry({ text, structured: code }));
          expect(live.kind).toBe("writing");
          expect(live.state).toBe(lang === "pt-BR" ? "escrevendo" : "writing");
          expect(live.detail).toBe(EXPECTED[lang][code]);
          expect(live.detail).not.toContain("{");
          expect(draftText(entry({ text, structured: code }))).toBe(EXPECTED[lang][code]);
        }
      }
      // Código desconhecido: o rótulo genérico, nunca o texto.
      expect(liveActionOf(entry({ text: '{"x": 1}', structured: "outro" })).detail).toBe(EXPECTED[lang].default);
    });

    it(`reserva sem código: texto que começa como JSON vira "preparando a resposta" (${lang})`, () => {
      setLanguage(lang, { persist: false });
      for (const text of ['{"issues": []}', '  [1, 2]', '```json\n{"a": 1}', "```JSON\n", '```\n{"a": 1}']) {
        const live = liveActionOf(entry({ text }));
        expect(live).toMatchObject({ kind: "writing", detail: EXPECTED[lang].default });
        expect(live.detail).not.toContain("{");
      }
    });
  }

  it("o texto normal não muda: prosa, bloco nants-choices e código que não é JSON", () => {
    const prose = "Vou montar o plano em três tarefas.";
    expect(liveActionOf(entry({ agent: "architect", text: prose })).detail).toBe(prose);
    const choices = 'Qual banco?\n\n```nants-choices\n{"questions": [{"id": "db", "text": "Banco", "options": ["Postgres", "SQLite"]}]}\n```\n';
    expect(looksStructured(choices)).toBe(false);
    expect(liveActionOf(entry({ agent: "architect", text: choices })).detail).toContain("nants-choices");
    // A Conversa continua lendo o bloco como pergunta (a reserva é só da prévia da Atividade).
    expect(parseChoices(choices).questions?.length).toBe(1);
    expect(looksStructured("```ts\nconst a = 1;\n```")).toBe(false);
    expect(looksStructured("O objeto {a} fica no fim")).toBe(false);
    expect(structuredText(entry({}))).toBeNull();
  });

  it("aviso fixo com code vence o tipo da chamada (ritmo e nova tentativa continuam visíveis)", () => {
    const pace = { code: "pace.waiting", params: { seconds: "12" } };
    expect(liveActionOf(entry({ text: "x", textCode: pace, structured: "verdict" }))).toMatchObject({ kind: "pacing" });
    const queued = { code: "pace.queued", params: {} };
    expect(draftText(entry({ text: "aguardando", textCode: queued, structured: "verdict" }))).not.toBe("preparando o parecer");
  });

  it("o resto da linha não muda: etapa, ferramenta, raciocínio e tempo do modelo", () => {
    const now = 100_000;
    const live = liveActionOf(entry({ text: '{"issues": []}', structured: "plan_review", stage: "Revisão do plano", stageCode: { code: "plan_review", params: {} }, modelStartedAt: now - 4_000, modelBytes: 2048 }), now);
    expect(live).toMatchObject({ kind: "writing", detail: "preparando a revisão do plano", model: "recebendo a resposta há 4 s · 2 KB recebidos" });
    expect(live.stage).not.toBeNull();
    expect(liveActionOf(entry({ tool: "lendo src/a.rs", text: '{"a": 1}', structured: "verdict" }))).toMatchObject({ kind: "tool", detail: "lendo src/a.rs" });
    expect(liveActionOf(entry({ reasoning: "conferindo a T01", structured: "verdict" }))).toMatchObject({ kind: "thinking", detail: "conferindo a T01" });
  });

  it("a bolha da Conversa mostra o rótulo do tipo, não o JSON", () => {
    render(<ActivityBubbles activities={[entry({ text: '{"issues": []}', structured: "plan_review" })]} />);
    const text = screen.getByTestId("activity-text").textContent ?? "";
    expect(text).toContain("preparando a revisão do plano");
    expect(text).not.toContain("{");
  });

  it("o store guarda o tipo do evento e do instantâneo; o passo novo sem código o limpa", () => {
    const store = createAppStore();
    const ev = (payload: Partial<AgentActivityPayload>): ApiEvent =>
      ({ event_type: "agent_activity", project_id: "p1", payload: { agent: "reviewer", activity_id: "a1", ...payload } }) as unknown as ApiEvent;
    store.applyEvent(ev({ kind: "start", structured: "plan_review" }));
    store.applyEvent(ev({ kind: "text", text: '{"issues": []}', structured: "plan_review" }));
    const first = store.getState().activities.p1?.[0];
    expect(first?.structured).toBe("plan_review");
    store.applyEvent(ev({ kind: "start", activity_id: "a2" }));
    expect(store.getState().activities.p1?.[0]?.structured).toBeUndefined();
    const snap: ActivitySnapshot = { agent: "reviewer", task_id: "T01", spec_id: "s1", activity_id: "b1", reasoning: "", text: '{"a":1}', tool: "", structured: "verdict", age_ms: 0 };
    const merged = mergeServerActivities([], [snap], 10_000);
    expect(merged[0]?.structured).toBe("verdict");
    expect(liveActionOf(merged[0] as ActivityEntry).detail).toBe("preparando o parecer");
  });

  it("os rótulos existem nos dois idiomas, sem travessão nem emoji", () => {
    for (const code of [...CODES, "default"]) {
      const key = `activity.structured.${code}` as keyof typeof ptBR;
      for (const dict of [ptBR, en] as Record<string, string>[]) {
        const value = dict[key] ?? "";
        expect(value).not.toBe("");
        expect(value).not.toMatch(/[–—]|\p{Extended_Pictographic}/u);
      }
    }
  });
});
