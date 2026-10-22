/*
 * Parser puro da mensagem do Revisor (spec 078, formato de `render_review_message` no backend),
 * usado pela janela Conversa e pelo Quadro. O próprio formato usa caracteres que a interface não
 * mostra (setas e marcas de critério): aqui eles são só texto de entrada, e a tela os troca por
 * ícones.
 *
 *   [Verificação final: N de M critérios atendidos\n\n]
 *   Aprovado|Pediu ajustes: resumo
 *   [\n\nApontamentos:\n- F-01 [MEDIUM] local: descrição (seta) recomendação]
 *   [\n  Cenário de falha: gatilho (seta) resultado errado]
 *   [\n\nObservações (não bloqueiam):\n- F-02 [LOW] local: descrição]
 *   [\n\nCapturas avaliadas: t01-1-a.png, t01-1-b.png]
 *   [\n\nCritérios:\n(marca) critério: evidência]
 *
 * Desde a 095/T08 o separador é ": " (sem travessão). As mensagens gravadas antes usam o
 * travessão entre espaços, e o parser aceita os dois.
 */
const CRITERIA_HEADER = "Critérios:";
/*
 * Marcas do formato gravado pelo servidor (sempre em pt-BR): só servem para ler a mensagem; a tela
 * mostra os rótulos do dicionário no idioma ativo.
 */
const FINDINGS_HEADER = "Apontamentos:";
const OBSERVATIONS_HEADER = "Observações (não bloqueiam):";
/** Linha recuada do cenário de falha (`runner::SCENARIO_PREFIX`), logo abaixo do finding. */
const SCENARIO_PREFIX = "Cenário de falha:";
/**
 * Rótulo, junto da severidade, da observação rebaixada porque o arquivo não mudou desde a revisão
 * anterior (`runner::UNCHANGED_FILE_LABEL`, spec 092 T04): `[MEDIUM, observação (arquivo não mudou nesta rodada)]`.
 */
const UNCHANGED_FILE_LABEL = "observação (arquivo não mudou nesta rodada)";
/** Linha das capturas vistas pelo Revisor (`runner::CAPTURES_HEADER`), última antes dos critérios. */
const CAPTURES_HEADER = "Capturas avaliadas:";
/** Nome de captura servido por `/captures/:name` (`sdd::screenshots::copy_name`). */
const CAPTURE_NAME = /^[a-z0-9_-][a-z0-9._-]{0,119}$/;
const CAPTURE_EXT = /\.(png|jpe?g)$/;
/** Vereditos do formato gravado (marcas de leitura; a tela usa `review.approved`/`review.changes`). */
const VERDICT_APPROVED_LABEL = "Aprovado";
const VERDICT_CHANGES_LABEL = "Pediu ajustes";
const FINAL_VERIFICATION_PREFIX = "Verificação final:";
/** Separador atual (095/T08) e o antigo, com travessão (U+2014), das mensagens já gravadas. */
const SEPARATOR = ": ";
const LEGACY_SEPARATOR = " \u2014 ";

/** "Verificação final: N de M critérios atendidos" (linha do servidor) em números, ou `null`. */
const FINAL_COUNTS = /^Verificação final: (\d+) de (\d+) critérios atendidos$/;

/** Contagem da linha da verificação final que precede o veredito (para exibir no idioma ativo). */
export function finalVerificationCounts(prefix: string): { met: number; total: number } | null {
  const match = FINAL_COUNTS.exec(prefix.trim());
  if (match === null) return null;
  return { met: Number(match[1]), total: Number(match[2]) };
}

export interface ChecklistItem {
  met: boolean;
  text: string;
}

/** Separa o resumo do checklist; `null` quando a mensagem não traz matriz. */
export function splitChecklist(text: string): { summary: string; items: ChecklistItem[] } | null {
  const marker = `\n\n${CRITERIA_HEADER}\n`;
  const at = text.indexOf(marker);
  if (at < 0) return null;
  const items: ChecklistItem[] = [];
  for (const line of text.slice(at + marker.length).split("\n")) {
    const mark = line[0];
    if (mark !== "✓" && mark !== "✗") continue;
    items.push({ met: mark === "✓", text: line.slice(1).trim() });
  }
  if (items.length === 0) return null;
  return { summary: text.slice(0, at), items };
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
}

export interface ReviewMessage {
  /** Linha "Verificação final: N de M critérios atendidos" que precede o veredito, quando houver. */
  prefix: string;
  /** `null` em mensagens antigas (sem veredito na primeira linha). */
  verdict: ReviewVerdict | null;
  summary: string;
  findings: ReviewFinding[];
  observations: ReviewFinding[];
  items: ChecklistItem[];
  /** Nomes das capturas avaliadas (linha `Capturas avaliadas:`), na ordem; vazio sem a linha. */
  captures: string[];
}

/** Nome aceito pelo endpoint das capturas (mesma regra do servidor). */
function isCaptureName(name: string): boolean {
  return CAPTURE_NAME.test(name) && CAPTURE_EXT.test(name) && !name.includes("..");
}

/**
 * Tira a linha `Capturas avaliadas: a, b` do fim de `head` (ela vem por último, antes dos
 * critérios). Só reconhece a linha final, para um resumo que cite o rótulo não ser cortado.
 */
export function splitCaptures(head: string): { rest: string; captures: string[] } {
  const marker = `\n\n${CAPTURES_HEADER} `;
  const at = head.lastIndexOf(marker);
  if (at < 0) return { rest: head, captures: [] };
  const line = head.slice(at + marker.length).trimEnd();
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
  const unchangedMark = `, ${UNCHANGED_FILE_LABEL}`;
  // O rótulo pode vir seguido de outro (`, baixa confiança`): tira só ele, onde estiver.
  const unchangedFile = rawSeverity.includes(unchangedMark);
  const finding: ReviewFinding = {
    id: head[1] ?? "",
    severity: unchangedFile ? rawSeverity.replace(unchangedMark, "") : rawSeverity,
    location,
    description: arrow < 0 ? tail : tail.slice(0, arrow),
    recommendation: arrow < 0 ? "" : tail.slice(arrow + 3),
  };
  if (unchangedFile) finding.unchangedFile = true;
  return finding;
}

function parseFindingList(body: string): ReviewFinding[] {
  const out: ReviewFinding[] = [];
  const scenarioMark = `  ${SCENARIO_PREFIX} `;
  for (const line of body.split("\n")) {
    if (line.startsWith("- ")) {
      out.push(parseFindingLine(line.slice(2).trim()));
      continue;
    }
    const last = out[out.length - 1];
    if (last !== undefined && line.startsWith(scenarioMark)) {
      const scenario = line.slice(scenarioMark.length).trim();
      if (scenario !== "") last.scenario = scenario;
    }
  }
  return out;
}

function parseVerdictHead(head: string): { prefix: string; verdict: ReviewVerdict; rest: string } | null {
  const candidates: [string, ReviewVerdict][] = [
    [VERDICT_APPROVED_LABEL, "approved"],
    [VERDICT_CHANGES_LABEL, "changes"],
  ];
  for (const [label, verdict] of candidates) {
    for (const separator of [SEPARATOR, LEGACY_SEPARATOR]) {
      const start = `${label}${separator}`;
      if (head.startsWith(start)) return { prefix: "", verdict, rest: head.slice(start.length) };
      const at = head.indexOf(`\n\n${start}`);
      if (at < 0) continue;
      const prefix = head.slice(0, at).trim();
      if (!prefix.startsWith(FINAL_VERIFICATION_PREFIX) || prefix.includes("\n")) continue;
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
  const marker = `\n\n${CRITERIA_HEADER}\n`;
  const at = text.indexOf(marker);
  const items = at < 0 ? [] : (splitChecklist(text)?.items ?? []);
  const { rest: head, captures } = splitCaptures(at < 0 ? text : text.slice(0, at));
  const parsed = parseVerdictHead(head);
  if (parsed === null) {
    if (items.length === 0) return null;
    return { prefix: "", verdict: null, summary: head, findings: [], observations: [], items, captures };
  }
  const findingsMark = `\n\n${FINDINGS_HEADER}\n`;
  const observationsMark = `\n\n${OBSERVATIONS_HEADER}\n`;
  const fAt = parsed.rest.indexOf(findingsMark);
  const oAt = parsed.rest.indexOf(observationsMark);
  const ends = [fAt, oAt].filter((n) => n >= 0);
  const summaryEnd = ends.length === 0 ? parsed.rest.length : Math.min(...ends);
  const findingsBody = fAt < 0 ? "" : parsed.rest.slice(fAt + findingsMark.length, oAt > fAt ? oAt : undefined);
  const observationsBody = oAt < 0 ? "" : parsed.rest.slice(oAt + observationsMark.length, fAt > oAt ? fAt : undefined);
  return {
    prefix: parsed.prefix,
    verdict: parsed.verdict,
    summary: parsed.rest.slice(0, summaryEnd).trim(),
    findings: parseFindingList(findingsBody),
    observations: parseFindingList(observationsBody),
    items,
    captures,
  };
}
