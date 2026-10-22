/* Horas e dias da Conversa (095/T04; C-16: mensagem sem hora mostra só "Você"). */
import { describe, expect, it } from "vitest";
import { clockLabel, dayKey, dayLabel, hasTime, userMeta } from "./chatTime";

const NOW = new Date(2026, 9, 20, 15, 30).getTime();

describe("chatTime", () => {
  it("hora hh:mm no fuso local", () => {
    expect(clockLabel(new Date(2026, 9, 20, 9, 5).getTime())).toBe("09:05");
  });

  it("'Você · hh:mm' e o fallback 'Você' sem hora (C-16)", () => {
    expect(userMeta(new Date(2026, 9, 20, 9, 12).getTime())).toBe("Você · 09:12");
    expect(userMeta(0)).toBe("Você");
    expect(userMeta(Number.NaN)).toBe("Você");
    expect(userMeta(undefined)).toBe("Você");
    expect(hasTime(0)).toBe(false);
    expect(hasTime(1)).toBe(true);
  });

  it("separador: Hoje, Ontem, data no ano e data com ano", () => {
    expect(dayLabel(new Date(2026, 9, 20, 0, 1).getTime(), NOW)).toBe("Hoje");
    expect(dayLabel(new Date(2026, 9, 19, 23, 59).getTime(), NOW)).toBe("Ontem");
    expect(dayLabel(new Date(2026, 9, 12, 8, 0).getTime(), NOW)).toBe("12 de outubro");
    expect(dayLabel(new Date(2025, 0, 3, 8, 0).getTime(), NOW)).toBe("3 de janeiro de 2025");
    // Virada de mês: o dia 1 tem "Ontem" no último dia do mês anterior.
    expect(dayLabel(new Date(2026, 8, 30, 10).getTime(), new Date(2026, 9, 1, 9).getTime())).toBe("Ontem");
  });

  it("chave do dia muda à meia-noite local", () => {
    expect(dayKey(new Date(2026, 9, 20, 23, 59).getTime())).not.toBe(dayKey(new Date(2026, 9, 21, 0, 0).getTime()));
  });
});
