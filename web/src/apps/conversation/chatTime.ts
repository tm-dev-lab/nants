/*
 * Horas e dias da Conversa (095/T04, decisão 7 e C-16), no fuso local e no idioma ativo:
 * - `hasTime`: a mensagem tem hora (linhas antigas do histórico podem vir sem `ts`, ou com 0);
 * - `clockLabel`: "09:12";
 * - `userMeta`: "Você · 09:12" acima da bolha, ou só "Você" sem hora (fallback da C-16);
 * - `dayKey`/`dayLabel`: o separador de dia ("Hoje", "Ontem" ou a data por extenso).
 */
import { formatDate, formatTime, t } from "../../i18n";

/** A mensagem tem hora conhecida (ms desde a época, positivo e finito). */
export function hasTime(at: number | null | undefined): at is number {
  return typeof at === "number" && Number.isFinite(at) && at > 0;
}

const pad = (n: number): string => String(n).padStart(2, "0");

/** "hh:mm" no fuso local, no formato do idioma ativo ("09:12"; em inglês, "09:12 AM"). */
export function clockLabel(at: number): string {
  return formatTime(at, { hour: "2-digit", minute: "2-digit" });
}

/** "Você · hh:mm", ou só "Você" quando a mensagem não tem hora (C-16). */
export function userMeta(at: number | null | undefined): string {
  const label = t("role.user");
  return hasTime(at) ? `${label} · ${clockLabel(at)}` : label;
}

/** Chave do dia local (para saber quando começa um dia novo na linha do tempo). */
export function dayKey(at: number): string {
  const date = new Date(at);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** "Hoje", "Ontem" ou "12 de outubro" (com o ano quando não é o ano corrente). */
export function dayLabel(at: number, now: number): string {
  const today = new Date(now);
  const yesterday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1);
  const key = dayKey(at);
  if (key === dayKey(now)) return t("time.today");
  if (key === dayKey(yesterday.getTime())) return t("time.yesterday");
  const date = new Date(at);
  const sameYear = date.getFullYear() === today.getFullYear();
  return formatDate(date, sameYear ? { day: "numeric", month: "long" } : { day: "numeric", month: "long", year: "numeric" });
}
