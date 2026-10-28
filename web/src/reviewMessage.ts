/*
 * Parser puro da mensagem do Revisor (spec 078, formato de `render_review_message` no backend),
 * usado pela janela Conversa e pelo Quadro. O próprio formato usa caracteres que a interface não
 * mostra (setas e marcas de critério): aqui eles são só texto de entrada, e a tela os troca por
 * ícones.
 *
 *   [<verificação final: N de M>\n\n]
 *   <aprovado>|<pediu ajustes>: resumo
 *   [\n\n<apontamentos>\n- F-01 [MEDIUM] local: descrição (seta) recomendação]
 *   [\n  <cenário de falha> gatilho (seta) resultado errado]
 *   [\n\n<observações>\n- F-02 [LOW] local: descrição]
 *   [\n\n<resolvidos>\n- F-03 [HIGH] local: descrição]
 *   [\n\n<respostas do Desenvolvedor>\n- F-01 [fixed] local | F-03 [contested, environment] motivo]
 *   [\n\n<decisões do Arquiteto>\n- F-03 [manual] título: motivo]
 *   [\n\n<capturas avaliadas> t01-1-a.png, t01-1-b.png]
 *   [\n\n<critérios>:\n(marca) critério: evidência]
 *
 * Spec 129 T04: o servidor escreve as marcas no idioma do projeto (`runner.marker.*`). Aqui elas
 * vêm das chaves `marker.*` do dicionário, e o parser aceita a forma de TODOS os idiomas (iterando
 * `DICTIONARIES`), pela linha inteira e sem diferenciar maiúsculas: as mensagens já gravadas em pt
 * continuam legíveis num projeto em inglês e vice-versa. A tela mostra os rótulos `review.*` no
 * idioma ativo.
 *
 * Desde a 095/T08 o separador é ": " (sem travessão). As mensagens gravadas antes usam o
 * travessão entre espaços, e o parser aceita os dois.
 */
import { DICTIONARIES, type MessageKey } from "./i18n";

/** Formas da marca em todos os idiomas do dicionário, sem repetição. */
export function markerForms(key: MessageKey): string[] {
  const out: string[] = [];
  for (const dictionary of Object.values(DICTIONARIES)) {
    const form = dictionary[key];
    if (!out.includes(form)) out.push(form);
  }
  return out;
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Alternativas (escapadas) das formas da marca, com `suffix` em cada uma. */
function alternatives(key: MessageKey, suffix = ""): string {
  return markerForms(key)
    .map((form) => escapeRegExp(form + suffix))
    .join("|");
}

/** Linha inteira de uma marca de seção, depois de uma linha em branco: `\n\n<marca>\n`. */
function sectionMark(key: MessageKey, suffix = ""): RegExp {
  return new RegExp(`\\n\\n(?:${alternatives(key, suffix)})\\n`, "i");
}

/** Igualdade sem diferenciar maiúsculas com alguma forma da marca. */
function isForm(text: string, key: MessageKey): boolean {
  const lower = text.toLowerCase();
  return markerForms(key).some((form) => form.toLowerCase() === lower);
}

/** Molde do dicionário (`... {met} ... {total} ...`) como expressão, com cada parâmetro em `(\d+)`. */
function templateRegExp(template: string): RegExp {
  const source = escapeRegExp(template).replace(/\\\{[a-z]+\\\}/gi, "(\\d+)");
  return new RegExp(`^${source}$`, "i");
}

const CRITERIA_MARK = sectionMark("marker.criteria", ":");
/** Cabeçalhos das seções da mensagem, na ordem de `ReviewMessage` (cada um em linha própria). */
const SECTION_MARKS: readonly RegExp[] = [
  sectionMark("marker.findings"),
  sectionMark("marker.observations"),
  sectionMark("marker.resolved"),
  sectionMark("marker.responses"),
  sectionMark("marker.decisions"),
];
/** Linha recuada do cenário de falha, logo abaixo do finding. */
const SCENARIO_MARK = new RegExp(`^  (?:${alternatives("marker.scenario")}) `, "i");
/** Linha das capturas vistas pelo Revisor, a última antes dos critérios. */
const CAPTURES_MARK = new RegExp(`\\n\\n(?:${alternatives("marker.captures")}) `, "gi");
/**
 * Marcas junto da severidade, depois de ", ": observação rebaixada porque o arquivo não mudou
 * (spec 092 T04), baixa confiança (spec 087 T05) e os estados `novo` e `aberto há N revisões`
 * (spec 124 T01). A tela mostra `review.unchangedFile`, `review.lowConfidence`, `review.stateNew`
 * e `review.stateOpen` no idioma ativo.
 */
const UNCHANGED_FILE_MARK = new RegExp(`, (?:${alternatives("marker.unchanged_file")})(?=,|$)`, "i");
const LOW_CONFIDENCE_MARK = new RegExp(`, (?:${alternatives("marker.low_confidence")})(?=,|$)`, "i");
const STATE_OPEN_MARKS: readonly RegExp[] = [...markerForms("marker.state_open.other"), ...markerForms("marker.state_open.one")].map(
  (template) => new RegExp(`, ${templateRegExp(template).source.slice(1, -1)}(?=,|$)`, "i"),
);
/** Nome de captura servido por `/captures/:name` (`sdd::screenshots::copy_name`). */
const CAPTURE_NAME = /^[a-z0-9_-][a-z0-9._-]{0,119}$/;
const CAPTURE_EXT = /\.(png|jpe?g)$/;
/** Separador atual (095/T08) e o antigo, com travessão (U+2014), das mensagens já gravadas. */
const SEPARATOR = ": ";
const LEGACY_SEPARATOR = " — ";

/** Linha da verificação final (`marker.final_verification`) em qualquer idioma. */
const FINAL_COUNTS: readonly RegExp[] = markerForms("marker.final_verification").map(templateRegExp);

/** Contagem da linha da verificação final que precede o veredito (para exibir no idioma ativo). */
export function finalVerificationCounts(prefix: string): { met: number; total: number } | null {
  for (const pattern of FINAL_COUNTS) {
    const match = pattern.exec(prefix.trim());
    if (match !== null) return { met: Number(match[1]), total: Number(match[2]) };
  }
  return null;
}

export interface ChecklistItem {
  met: boolean;
  /** Spec 133 T06: `?` na matriz, a evidência citou caminhos que não existem (distinto de não atendido). */
  unverified?: true;
  text: string;
}

/** Separa o resumo do checklist; `null` quando a mensagem não traz matriz. */
export function splitChecklist(text: string): { summary: string; items: ChecklistItem[] } | null {
  const found = CRITERIA_MARK.exec(text);
  if (found === null) return null;
  const items: ChecklistItem[] = [];
  for (const line of text.slice(found.index + found[0].length).split("\n")) {
    const mark = line[0];
    if (mark === "?") {
      items.push({ met: false, unverified: true, text: line.slice(1).trim() });
      continue;
    }
    if (mark !== "✓" && mark !== "✗") continue;
    items.push({ met: mark === "✓", text: line.slice(1).trim() });
  }
  if (items.length === 0) return null;
  return { summary: text.slice(0, found.index), items };
}

export type ReviewVerdict = "approved" | "changes";

export interface ReviewFinding {
  id: string;
  severity: string;
  location: string;
  description: string;
  recommendation: string;
  /** Cenário de falha (gatilho → resultado errado); ausente em mensagens sem a linha. */
  scenario?: string;
  /** Spec 092 T04: observação rebaixada porque o arquivo não mudou nesta rodada. */
  unchangedFile?: boolean;
  /** Spec 087 T05: finding de baixa confiança (vira observação). */
  lowConfidence?: boolean;
  /** Spec 124 T01: estado apurado pelo runner; ausente em mensagens antigas. */
  state?: "new" | "open" | "resolved";
  /** Spec 124 T01: revisões seguidas em que está aberto (só com `state` "open"). */
  recurrence?: number;
}

/** Spec 124 T02: resposta do Desenvolvedor a um apontamento. */
export interface ReviewResponse {
  id: string;
  /** `fixed`, `test_added` ou `contested` (código; desconhecido aparece como veio). */
  kind: string;
  /** Só em `contested`: `environment`, `out_of_scope` ou `incorrect`. */
  reason?: string;
  /** Local, teste ou motivo, como o Desenvolvedor escreveu. */
  detail: string;
}

/** Spec 124 T02: decisão do Arquiteto sobre um apontamento contestado. */
export interface ReviewDecision {
  id: string;
  /** `keep`, `adjust` ou `manual` (código). */
  decision: string;
  /** Título do apontamento e motivo, como o servidor gravou. */
  detail: string;
}

export interface ReviewMessage {
  /** Linha da verificação final ("N de M critérios atendidos") que precede o veredito, quando houver. */
  prefix: string;
  /** `null` em mensagens antigas (sem veredito na primeira linha). */
  verdict: ReviewVerdict | null;
  summary: string;
  findings: ReviewFinding[];
  observations: ReviewFinding[];
  /** Spec 124 T01: apontamentos anteriores resolvidos nesta revisão; vazio sem a seção. */
  resolved: ReviewFinding[];
  /** Spec 124 T02: respostas do Desenvolvedor conferidas nesta revisão; vazio sem a seção. */
  responses: ReviewResponse[];
  /** Spec 124 T02: decisões do Arquiteto sobre contestações; vazio sem a seção. */
  decisions: ReviewDecision[];
  items: ChecklistItem[];
  /** Nomes das capturas avaliadas (linha das capturas), na ordem; vazio sem a linha. */
  captures: string[];
}

/** Nome aceito pelo endpoint das capturas (mesma regra do servidor). */
function isCaptureName(name: string): boolean {
  return CAPTURE_NAME.test(name) && CAPTURE_EXT.test(name) && !name.includes("..");
}

/**
 * Tira a linha das capturas (`<capturas avaliadas> a, b`, em qualquer idioma) do fim de `head`
 * (ela vem por último, antes dos critérios). Só reconhece a linha final, para um resumo que cite o
 * rótulo não ser cortado.
 */
export function splitCaptures(head: string): { rest: string; captures: string[] } {
  let last: RegExpExecArray | null = null;
  for (const match of head.matchAll(CAPTURES_MARK)) last = match as RegExpExecArray;
  if (last === null) return { rest: head, captures: [] };
  const at = last.index;
  const line = head.slice(at + last[0].length).trimEnd();
  if (line.includes("\n")) return { rest: head, captures: [] };
  const captures = line.split(", ").map((n) => n.trim()).filter(isCaptureName);
  return { rest: head.slice(0, at), captures };
}

/** Local e resto de `local: descrição` (ou `local (travessão) descrição`, nas mensagens antigas). */
function splitLocation(rest: string): { location: string; tail: string } {
  // O local (`src/a.rs:10`) tem dois-pontos sem espaço: vale o separador que aparece primeiro, para a
  // descrição poder conter o outro.
  const found = [SEPARATOR, LEGACY_SEPARATOR]
    .map((separator) => ({ separator, at: rest.indexOf(separator) }))
    .filter((f) => f.at >= 0)
    .sort((a, b) => a.at - b.at)[0];
  if (found === undefined) return { location: "", tail: rest };
  return { location: rest.slice(0, found.at), tail: rest.slice(found.at + found.separator.length) };
}

/** Uma linha `ID [SEV] local: descrição → recomendação`; fora desse formato, vira só descrição. */
function parseFindingLine(line: string): ReviewFinding {
  const head = /^(\S+) \[([^\]]*)\] (.*)$/.exec(line);
  if (head === null) return { id: "", severity: "", location: "", description: line, recommendation: "" };
  const { location, tail } = splitLocation(head[3] ?? "");
  const arrow = tail.indexOf(" → ");
  const rawSeverity = head[2] ?? "";
  // Os rótulos podem vir juntos (`, observação (...), baixa confiança`): tira cada um, onde estiver.
  const unchangedFile = UNCHANGED_FILE_MARK.test(rawSeverity);
  const lowConfidence = LOW_CONFIDENCE_MARK.test(rawSeverity);
  const openPattern = STATE_OPEN_MARKS.find((pattern) => pattern.test(rawSeverity));
  const openMatch = openPattern === undefined ? null : openPattern.exec(rawSeverity);
  // Depois da severidade, as marcas vêm separadas por ", "; "novo" é uma delas.
  let rest = rawSeverity.replace(UNCHANGED_FILE_MARK, "").replace(LOW_CONFIDENCE_MARK, "");
  if (openPattern !== undefined) rest = rest.replace(openPattern, "");
  const parts = rest.split(", ");
  const isNew = parts.slice(1).some((p) => isForm(p, "marker.state_new"));
  const finding: ReviewFinding = {
    id: head[1] ?? "",
    severity: parts.filter((p, i) => i === 0 || !isForm(p, "marker.state_new")).join(", "),
    location,
    description: arrow < 0 ? tail : tail.slice(0, arrow),
    recommendation: arrow < 0 ? "" : tail.slice(arrow + 3),
  };
  if (unchangedFile) finding.unchangedFile = true;
  if (lowConfidence) finding.lowConfidence = true;
  if (openMatch !== null) {
    finding.state = "open";
    finding.recurrence = Number(openMatch[1]);
  } else if (isNew) {
    finding.state = "new";
  }
  return finding;
}

function parseFindingList(body: string): ReviewFinding[] {
  const out: ReviewFinding[] = [];
  for (const line of body.split("\n")) {
    if (line.startsWith("- ")) {
      out.push(parseFindingLine(line.slice(2).trim()));
      continue;
    }
    const last = out[out.length - 1];
    const scenarioMark = SCENARIO_MARK.exec(line);
    if (last !== undefined && scenarioMark !== null) {
      const scenario = line.slice(scenarioMark[0].length).trim();
      if (scenario !== "") last.scenario = scenario;
    }
  }
  return out;
}

/** Linhas `- ID [código(, código)] detalhe` de uma seção (respostas ou decisões). */
function parseCodedLines(body: string): { id: string; codes: string[]; detail: string }[] {
  const out: { id: string; codes: string[]; detail: string }[] = [];
  for (const line of body.split("\n")) {
    const match = /^- (\S+) \[([a-z_]+(?:, [a-z_]+)?)\] ?(.*)$/.exec(line.trim());
    if (match === null) continue;
    out.push({ id: match[1] ?? "", codes: (match[2] ?? "").split(", "), detail: (match[3] ?? "").trim() });
  }
  return out;
}

function parseResponses(body: string): ReviewResponse[] {
  return parseCodedLines(body).map(({ id, codes, detail }) => {
    const response: ReviewResponse = { id, kind: codes[0] ?? "", detail };
    if (codes[1] !== undefined) response.reason = codes[1];
    return response;
  });
}

function parseDecisions(body: string): ReviewDecision[] {
  return parseCodedLines(body).map(({ id, codes, detail }) => ({ id, decision: codes[0] ?? "", detail }));
}

function parseVerdictHead(head: string): { prefix: string; verdict: ReviewVerdict; rest: string } | null {
  const candidates: [string, ReviewVerdict][] = [
    ...markerForms("marker.verdict_approved").map((label): [string, ReviewVerdict] => [label, "approved"]),
    ...markerForms("marker.verdict_changes").map((label): [string, ReviewVerdict] => [label, "changes"]),
  ];
  const lower = head.toLowerCase();
  for (const [label, verdict] of candidates) {
    for (const separator of [SEPARATOR, LEGACY_SEPARATOR]) {
      const start = `${label}${separator}`.toLowerCase();
      if (lower.startsWith(start)) return { prefix: "", verdict, rest: head.slice(start.length) };
      const at = lower.indexOf(`\n\n${start}`);
      if (at < 0) continue;
      const prefix = head.slice(0, at).trim();
      if (finalVerificationCounts(prefix) === null) continue;
      return { prefix, verdict, rest: head.slice(at + 2 + start.length) };
    }
  }
  return null;
}

/**
 * Interpreta a mensagem do Revisor; `null` quando não traz veredito nem matriz de critérios
 * (texto comum, renderizado como qualquer mensagem de agente).
 */
export function parseReviewMessage(text: string): ReviewMessage | null {
  const criteria = CRITERIA_MARK.exec(text);
  const at = criteria === null ? -1 : criteria.index;
  const items = at < 0 ? [] : (splitChecklist(text)?.items ?? []);
  const { rest: head, captures } = splitCaptures(at < 0 ? text : text.slice(0, at));
  const parsed = parseVerdictHead(head);
  if (parsed === null) {
    if (items.length === 0) return null;
    return { prefix: "", verdict: null, summary: head, findings: [], observations: [], resolved: [], responses: [], decisions: [], items, captures };
  }
  // Cada seção vai do seu cabeçalho até o próximo cabeçalho encontrado (em qualquer ordem).
  const matches = SECTION_MARKS.map((mark) => mark.exec(parsed.rest));
  const starts = matches.map((m) => (m === null ? -1 : m.index));
  const found = starts.filter((n) => n >= 0);
  const summaryEnd = found.length === 0 ? parsed.rest.length : Math.min(...found);
  const bodyOf = (i: number): string => {
    const match = matches[i] ?? null;
    if (match === null) return "";
    const next = found.filter((n) => n > match.index);
    return parsed.rest.slice(match.index + match[0].length, next.length === 0 ? undefined : Math.min(...next));
  };
  const resolved = parseFindingList(bodyOf(2)).map((f): ReviewFinding => ({ ...f, state: "resolved" }));
  return {
    prefix: parsed.prefix,
    verdict: parsed.verdict,
    summary: parsed.rest.slice(0, summaryEnd).trim(),
    findings: parseFindingList(bodyOf(0)),
    observations: parseFindingList(bodyOf(1)),
    resolved,
    responses: parseResponses(bodyOf(3)),
    decisions: parseDecisions(bodyOf(4)),
    items,
    captures,
  };
}

/** Spec 124 T01: contagem dos estados dos apontamentos de uma revisão (bloqueantes e resolvidos). */
interface ReviewStateCounts {
  new: number;
  open: number;
  resolved: number;
}

export function reviewStateCounts(review: ReviewMessage): ReviewStateCounts {
  return {
    new: review.findings.filter((f) => f.state === "new").length,
    open: review.findings.filter((f) => f.state === "open").length,
    resolved: review.resolved.length,
  };
}

/**
 * Resumo "2 novos · 1 ainda aberto · 1 resolvido" no idioma de `tp`; vazio quando a mensagem não
 * traz estados (mensagens antigas).
 */
export function reviewStateLine(
  counts: ReviewStateCounts,
  tp: (base: "review.countNew" | "review.countOpen" | "review.countResolved", count: number) => string,
): string {
  const parts: string[] = [];
  if (counts.new > 0) parts.push(tp("review.countNew", counts.new));
  if (counts.open > 0) parts.push(tp("review.countOpen", counts.open));
  if (counts.resolved > 0) parts.push(tp("review.countResolved", counts.resolved));
  return parts.join(" · ");
}
