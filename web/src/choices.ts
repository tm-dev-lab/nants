/*
 * Perguntas estruturadas do Arquiteto: bloco cercado ```nants-choices``` com JSON
 * `{"questions":[{"id","text","multi","options":[...]}]}` (máx. 4 perguntas, 2–6 opções).
 * Parser puro: o bloco nunca aparece como texto cru; inválido/excedente => sem botões.
 */
import { t, translate } from "./i18n";
import { LANGS } from "./i18n/lang";

export interface ChoiceQuestion {
  id: string;
  text: string;
  multi: boolean;
  options: string[];
  /** Opções recomendadas (existentes em `options`); pré-selecionadas na UI. */
  recommended?: string[];
  /** Porquê curto da recomendação. */
  why?: string;
}

const MAX_CHOICE_QUESTIONS = 4;
const MIN_CHOICE_OPTIONS = 2;
const MAX_CHOICE_OPTIONS = 6;

/**
 * O fechamento pode vir colado ao texto seguinte (```` ```Once you… ````, spec 126 T04): o resto vira
 * texto normal, num parágrafo próprio.
 */
const BLOCK = /```nants-choices[ \t]*\r?\n([\s\S]*?)```[ \t]*(\r?\n)?/g;

function validate(raw: unknown): ChoiceQuestion[] | null {
  if (typeof raw !== "object" || raw === null) return null;
  const list = (raw as { questions?: unknown }).questions;
  if (!Array.isArray(list) || list.length < 1 || list.length > MAX_CHOICE_QUESTIONS) return null;
  const out: ChoiceQuestion[] = [];
  for (const item of list) {
    if (typeof item !== "object" || item === null) return null;
    const { id, text, multi, options, recommended, why } = item as Record<string, unknown>;
    if (typeof id !== "string" || typeof text !== "string" || text.trim() === "") return null;
    if (multi !== undefined && typeof multi !== "boolean") return null;
    if (!Array.isArray(options) || options.length < MIN_CHOICE_OPTIONS || options.length > MAX_CHOICE_OPTIONS) return null;
    if (!options.every((o) => typeof o === "string" && o.trim() !== "")) return null;
    const opts = options as string[];
    let rec: string[] = [];
    if (typeof recommended === "string") rec = [recommended];
    else if (Array.isArray(recommended)) rec = recommended.filter((r): r is string => typeof r === "string");
    rec = rec.filter((r, k) => opts.includes(r) && rec.indexOf(r) === k);
    if (multi !== true) rec = rec.slice(0, 1);
    const whyText = typeof why === "string" && why.trim() !== "" ? why.trim() : undefined;
    out.push({ id, text: text.trim(), multi: multi === true, options: opts, recommended: rec, ...(whyText !== undefined && rec.length > 0 ? { why: whyText } : {}) });
  }
  return out;
}

/** Remove todos os blocos do texto; devolve as perguntas do ÚLTIMO bloco válido (ou null). */
export function parseChoices(text: string): { text: string; questions: ChoiceQuestion[] | null } {
  let questions: ChoiceQuestion[] | null = null;
  const stripped = text.replace(BLOCK, (_all, body: string, newline: string | undefined) => {
    try {
      questions = validate(JSON.parse(body));
    } catch {
      questions = null;
    }
    return newline === undefined ? "\n\n" : "";
  });
  return { text: stripped.trim(), questions };
}

/** Uma linha por pergunta: `Pergunta: resposta` (múltiplas separadas por vírgula). */
export function formatAnswers(questions: readonly ChoiceQuestion[], answers: readonly (readonly string[])[]): string {
  return questions
    .map((q, i) => `${q.text}${/[?:]$/.test(q.text) ? " " : ": "}${(answers[i] ?? []).join(", ")}`)
    .join("\n");
}

/** Texto enviado por "Decida você" (mesmo caminho do composer), no idioma ativo. */
export function acceptRecommendationsText(): string {
  return t("choices.acceptText");
}

/** O texto é resposta gerada pelo formulário: "Decida você" (em qualquer idioma) ou uma linha por pergunta (`formatAnswers`). */
export function looksLikeChoicesAnswer(questions: readonly ChoiceQuestion[], text: string): boolean {
  const trimmed = text.trim();
  if (LANGS.some((lang) => translate(lang, "choices.acceptText") === trimmed)) return true;
  const lines = trimmed.split(/\r?\n/);
  if (lines.length !== questions.length) return false;
  return questions.every((q, i) => {
    const prefix = `${q.text}${/[?:]$/.test(q.text) ? " " : ": "}`;
    return (lines[i] ?? "").startsWith(prefix);
  });
}

/** Itens de lista da seção `## Premissas` de um spec.md (vazio se não existir). */
export function extractPremises(markdown: string): string[] {
  const lines = markdown.split(/\r?\n/);
  const start = lines.findIndex((l) => /^##\s+Premissas\s*$/i.test(l.trim()));
  if (start < 0) return [];
  const out: string[] = [];
  for (const line of lines.slice(start + 1)) {
    if (/^#{1,6}\s/.test(line)) break;
    const m = /^\s*(?:[-*+]|\d+\.)\s+(.+?)\s*$/.exec(line);
    if (m?.[1] !== undefined) out.push(m[1]);
  }
  return out;
}
