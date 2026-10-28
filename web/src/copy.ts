/*
 * Textos fixos da interface. Nenhum texto do servidor/agente passa por aqui: mensagens humanas são
 * mapeadas a partir de códigos fixos (`reason`, status HTTP).
 *
 * Spec 102: os textos da Conversa, do Plano, do Quadro, da Atividade, dos Custos, do App e da Execução saem do
 * dicionário (`i18n`) no idioma ativo. As
 * tabelas usam `localized` (cada leitura devolve o texto no idioma ativo) e as funções traduzem na
 * hora. As constantes `ptBR[...]` que restam são a reserva em pt-BR, para quem ainda é só pt-BR
 * (outras janelas) e para os testes que conferem o texto pt-BR.
 */
import type { PermissionDecision, TaskStatus, WireApi } from "./api/types";
import { formatNumber, getLanguage, localized, t, tp, type MessageKey } from "./i18n";
import { ptBR } from "./i18n/ptBR";
import { localizeProviderDetailIn } from "./i18n/serverCodes";

/** Mensagem humana para cada `reason` de 409 de `approve` (runs.rs), no idioma ativo. */
export const APPROVE_REASON_MESSAGES: Readonly<Record<string, string>> = localized({
  no_tasks: "approve.reason.no_tasks",
  invalid_task: "approve.reason.invalid_task",
  dependency_missing: "approve.reason.dependency_missing",
  dependency_cycle: "approve.reason.dependency_cycle",
  base_branch_missing: "approve.reason.base_branch_missing",
  settings_invalid: "approve.reason.settings_invalid",
  not_git_repo: "approve.reason.not_git_repo",
  environment_invalid: "approve.reason.environment_invalid",
  environment_changed: "approve.reason.environment_changed",
  // Todo projeto roda em docker (spec 074): plano ou projeto sem ambiente docker é recusado.
  environment_not_docker: "approve.dockerRequired",
  // Docker ausente no host: pré-requisito (o Nants nunca instala o Docker).
  docker_unavailable: "approve.dockerUnavailable",
  base_dirty: "approve.reason.base_dirty",
  architect_turn_active: "approve.reason.architect_turn_active",
  plan_review_running: "approve.reason.plan_review_running",
  plan_without_tasks: "approve.reason.plan_without_tasks",
});

/** Cartão do plano enquanto o turno do Arquiteto está ativo (reserva pt-BR; a tela usa `plan.drafting`). */
export const PLAN_DRAFTING_NOTICE = ptBR["plan.drafting"];

/** Rótulo fixo de cada pendência da prontidão (412 `prereq_missing`, spec 093 T02), no idioma ativo. */
export const PREREQ_LABELS: Readonly<Record<string, string>> = localized({
  connection: "prereq.label.connection",
  agent_architect: "prereq.label.agent_architect",
  agent_developer: "prereq.label.agent_developer",
  agent_reviewer: "prereq.label.agent_reviewer",
  git: "prereq.label.git",
  docker_cli: "prereq.label.docker_cli",
  docker_daemon: "prereq.label.docker_daemon",
  docker_permission: "prereq.label.docker_permission",
  docker_compose: "prereq.label.docker_compose",
});

/**
 * Mensagem do 412 `prereq_missing`: "Falta configurar: …" com os rótulos fixos dos ids conhecidos
 * (ids desconhecidos não aparecem; sem nenhum conhecido, um texto genérico).
 */
export function prereqMissingMessage(ids: readonly string[]): string {
  const labels = ids.map((id) => PREREQ_LABELS[id]).filter((l): l is string => l !== undefined);
  const unique = [...new Set(labels)];
  return unique.length > 0 ? t("prereq.missing", { items: unique.join(", ") }) : t("prereq.missingGeneric");
}

/** Reserva pt-BR de "não foi possível iniciar" (a tela usa `approve.fallback`). */
export const APPROVE_FALLBACK_MESSAGE = ptBR["approve.fallback"];

/**
 * Spec legada sem comando de testes (409 test_command_missing): a UI pede ao Arquiteto, sem campo
 * para o usuário. Mensagem interna fixa (prompt interno, fica em português); o estado neutro mostra
 * apenas que o Arquiteto trabalha.
 */
export const ARCHITECT_COMPLETE_PLAN_MESSAGE =
  "Defina a stack e o comando de testes (test_command) da spec e atualize-a para aprovação.";
/** Reserva pt-BR de `plan.architectCompleting`. */
export const ARCHITECT_COMPLETING_NOTICE = ptBR["plan.architectCompleting"];

/** Reservas pt-BR das checagens do plano (a tela usa as chaves `plan.review.*`). */
export const PLAN_ACTIONS_WAITING_REASON = ptBR["plan.review.waitingReason"];
export const PLAN_REVIEW_WAITING = ptBR["plan.review.waiting"];

/** Rótulos dos botões de decisão do cartão de permissão. */
export const PERMISSION_OPTION_LABEL: Readonly<Record<PermissionDecision, string>> = localized({
  once: "permission.option.once",
  run: "permission.option.run",
  always: "permission.option.always",
  deny: "permission.option.deny",
});

/**
 * Uma linha por opção, abaixo dos botões, com o efeito real (approvals.rs): `once` vale só para este
 * pedido; `run` libera o comando normalizado exato até o fim do run corrente, só em memória;
 * `always` grava o comando em `allowed_commands` de `.sdd/settings.yaml` do projeto; `deny` devolve
 * a recusa ao agente, que continua sem o comando.
 */
export const PERMISSION_OPTION_HINT: Readonly<Record<PermissionDecision, string>> = localized({
  once: "permission.hint.once",
  run: "permission.hint.run",
  always: "permission.hint.always",
  deny: "permission.hint.deny",
});

/** Mudança de ambiente: só "Aprovar mudança" (vale para este pedido) e "Negar". */
export const PERMISSION_ENV_OPTION_HINT: Readonly<Record<"once" | "deny", string>> = localized({
  once: "permission.envHint.once",
  deny: "permission.envHint.deny",
});

/** Título da aba quando há algo esperando o usuário (permissão, execução pausada ou plano), no idioma ativo. */
export function attentionTitle(count: number): string {
  return t("attention.tabTitle", { count });
}

/** Confirmação discreta do escopo realmente aplicado (`applied` da resposta). */
export const PERMISSION_APPLIED_NOTICE: Readonly<Record<PermissionDecision, string>> = localized({
  once: "permission.applied.once",
  run: "permission.applied.run",
  always: "permission.applied.always",
  deny: "permission.applied.deny",
});

/** Pediu "Sempre", mas o servidor aplicou um escopo menor. */
export const PERMISSION_ALWAYS_FALLBACK_NOTICE: Readonly<Record<"once" | "run", string>> = localized({
  once: "permission.alwaysFallback.once",
  run: "permission.alwaysFallback.run",
});

/**
 * Códigos do servidor para `always_allowed: false` (`approvals::always_denied_code`, spec 115 T03)
 * e a chave do texto exibido para cada um. Código ausente ou desconhecido vira o genérico.
 */
const PERMISSION_ALWAYS_REASON_KEYS: ReadonlyMap<string, MessageKey> = new Map<string, MessageKey>([
  ["arbitrary", "permission.always.arbitrary"],
  ["operators", "permission.always.operators"],
  ["secret", "permission.always.secret"],
  ["docker", "permission.always.docker"],
  ["generic", "permission.always.generic"],
]);

/** Explicação de não haver "Sempre" pelo código do motivo (ausente ou desconhecido: genérico). */
export function permissionAlwaysReason(code: string | null | undefined): string {
  const key = code === null || code === undefined ? undefined : PERMISSION_ALWAYS_REASON_KEYS.get(code);
  return t(key ?? "permission.always.generic");
}

/**
 * Causas fixas de falha do provedor (`ProviderFailure` do servidor): `provider_http_<NNN>` com
 * status 100..599, ou um dos códigos fixos. Qualquer outro texto fica de fora.
 */
const PROVIDER_HTTP_CODE = /^provider_http_([1-5][0-9]{2})$/;
const PROVIDER_FIXED_CODES: ReadonlySet<string> = new Set([
  "provider_timeout",
  "provider_connect_failed",
  "provider_bad_response",
  "provider_invalid_config",
  "provider_plan_limit",
  "provider_plan_not_eligible",
  "provider_plan_needs_login",
  "provider_spend_limit",
  "provider_daily_quota",
  "provider_billing",
]);

/** Dica de limite (spec 089) para `provider_bad_response` com detalhe "provedor no limite…" (reserva pt-BR). */
export const PROVIDER_THROTTLE_HINT = ptBR["providerMsg.throttleHint"].replace("{advice}", ptBR["providerMsg.throttleAdvice"]);

/** Dica de limite no idioma ativo. */
function throttleHint(): string {
  return t("providerMsg.throttleHint", { advice: t("providerMsg.throttleAdvice") });
}

/** Prefixo do detalhe que o servidor usa quando o provedor está no limite (spec 089). */
const PROVIDER_THROTTLE_DETAIL_PREFIX = "provedor no limite";

/** `true` só para códigos do vocabulário de falha do provedor. */
export function isProviderFailureCode(code: string): boolean {
  return PROVIDER_FIXED_CODES.has(code) || PROVIDER_HTTP_CODE.test(code);
}

/** Detalhe da falha do provedor (spec 088): texto curto do vocabulário do servidor. */
const PROVIDER_DETAIL_MAX = 80;

/**
 * Detalhe aceito para exibição: texto não vazio, com até 80 caracteres e sem quebra de linha.
 * Qualquer outra coisa é ignorada (`null`).
 */
export function providerFailureDetail(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const detail = raw.trim();
  if (detail === "" || detail.length > PROVIDER_DETAIL_MAX || /[\r\n]/.test(raw)) return null;
  return detail;
}

/** Prefixo das chaves do rótulo do motivo da falha do provedor (`provider.kind.<kind>`). */
const PROVIDER_KIND_PREFIX = "provider.kind.";
/** Código saneado do provedor (o mesmo formato do servidor, `ProviderCode`). */
const PROVIDER_REASON_CODE = /^[a-z0-9_.-]{1,40}$/;

/**
 * Motivo da falha do provedor no idioma ativo (spec 119), pelos `params` neutros do servidor: o
 * rótulo do `kind` no dicionário, com `: <provider_code>` quando houver. `null` sem `kind`
 * conhecido.
 */
export function providerReasonText(kind: unknown, providerCode: unknown): string | null {
  if (typeof kind !== "string" || !Object.hasOwn(ptBR, `${PROVIDER_KIND_PREFIX}${kind}`)) return null;
  const label = t(`${PROVIDER_KIND_PREFIX}${kind}` as MessageKey);
  return typeof providerCode === "string" && PROVIDER_REASON_CODE.test(providerCode) ? `${label}: ${providerCode}` : label;
}

/**
 * Detalhe já gravado em pt-BR (registros antigos e textos de reserva do servidor, spec 119) no
 * idioma ativo: o rótulo é lido de volta pelo dicionário pt-BR (`provider.kind.*`). Fora do
 * vocabulário, como veio.
 */
export function localizeProviderDetail(detail: string): string {
  return localizeProviderDetailIn(detail, getLanguage());
}

/** Detalhe de limite do provedor (spec 089), no rótulo pt-BR de sempre ou no do idioma ativo. */
function isThrottleDetail(detail: string | null): boolean {
  if (detail === null) return false;
  return detail.startsWith(PROVIDER_THROTTLE_DETAIL_PREFIX) || detail.startsWith(t("provider.kind.throttled"));
}

/** Põe o detalhe entre parênteses antes do ponto final da frase. */
function withDetail(text: string, detail: string | null): string {
  if (detail === null) return text;
  return text.endsWith(".") ? `${text.slice(0, -1)} (${detail}).` : `${text} (${detail})`;
}

/**
 * Mensagem no idioma ativo para uma causa fixa do provedor; `null` para texto fora do vocabulário.
 * `architectWireApi` é o protocolo da conexão do Arquiteto (`null` quando não se sabe): a dica
 * de Responses só entra para HTTP 4xx (exceto 401/403/404/429) em conexão `chat`. `detail` (já
 * validado por `providerFailureDetail`) entra entre parênteses antes do ponto final. Limite do
 * provedor (HTTP 429, ou `provider_bad_response` com detalhe "provedor no limite…") ganha o
 * conselho de espera (spec 089).
 */
export function providerFailureMessage(
  code: string,
  architectWireApi: WireApi | null,
  detail: string | null = null,
): string | null {
  const http = PROVIDER_HTTP_CODE.exec(code);
  if (http !== null) {
    const status = Number(http[1]);
    if (status === 401 || status === 403) return withDetail(t("providerMsg.keyRefused", { status }), detail);
    if (status === 404) return withDetail(t("providerMsg.notFound"), detail);
    if (status === 429) {
      // A frase já diz que houve limite; só acrescenta o conselho de espera (spec 089).
      return `${withDetail(t("providerMsg.throttled"), detail)} ${t("providerMsg.throttleAdvice")}`;
    }
    if (status >= 500) return withDetail(t("providerMsg.serverFailed", { status }), detail);
    if (status >= 400) {
      const base = withDetail(t("providerMsg.refused", { status }), detail);
      return architectWireApi === "chat" ? `${base} ${t("providerMsg.responsesHint")}` : base;
    }
    return withDetail(t("providerMsg.badResponse"), detail);
  }
  switch (code) {
    case "provider_timeout":
      return withDetail(t("providerMsg.timeout"), detail);
    case "provider_connect_failed":
      return withDetail(t("providerMsg.connectFailed"), detail);
    case "provider_bad_response": {
      const base = withDetail(t("providerMsg.badResponse"), detail);
      // Detalhe de limite do provedor (spec 089) ganha a dica de espera.
      return isThrottleDetail(detail) ? `${base} ${throttleHint()}` : base;
    }
    case "provider_invalid_config":
      return withDetail(t("providerMsg.invalidConfig"), detail);
    // Plano ChatGPT (spec 116 T02): limite de uso, conta não elegível ou entrar de novo.
    case "provider_plan_limit":
      return t("providerMsg.planLimit");
    case "provider_plan_not_eligible":
      return t("providerMsg.planNotEligible");
    case "provider_plan_needs_login":
      return t("providerMsg.planNeedsLogin");
    // Limites que pausam (spec 132 T09): o teto de gasto da Anthropic e a cota diária do Gemini.
    case "provider_spend_limit":
      return t("providerMsg.spendLimit");
    case "provider_daily_quota":
      return t("providerMsg.dailyQuota");
    case "provider_billing":
      return t("providerMsg.billing");
    default:
      return null;
  }
}

/** `<código> (<detalhe>)` da mensagem de sistema da execução (spec 088). */
const PROVIDER_TEXT_WITH_DETAIL = /^(\S+) \(([\s\S]*)\)$/;

/**
 * Mensagem no idioma ativo para o texto de sistema de uma falha do provedor: `<código>` ou
 * `<código> (<detalhe>)`. Código fora do vocabulário dá `null`; detalhe inválido é ignorado.
 */
export function providerFailureText(text: string, architectWireApi: WireApi | null): string | null {
  const match = PROVIDER_TEXT_WITH_DETAIL.exec(text);
  if (match === null) return providerFailureMessage(text, architectWireApi);
  const code = match[1] ?? "";
  if (!isProviderFailureCode(code)) return null;
  // Spec 119: o detalhe gravado em pt-BR (texto de reserva antigo) sai no idioma ativo.
  const detail = providerFailureDetail(match[2]);
  return providerFailureMessage(code, architectWireApi, detail === null ? null : localizeProviderDetail(detail));
}

/*
 * Reservas pt-BR dos avisos do envio ao Arquiteto (a tela usa as chaves `chat.*` no idioma ativo):
 * - acompanhamento do turno depois da queda da conexão (spec 080);
 * - mensagem que não chegou ao Arquiteto (reenviável) e 409 de turno em andamento;
 * - 504 do servidor (com ou sem o plano salvo) e o tempo do navegador esgotado.
 */
export const CHAT_TRACKING_NOTICE = ptBR["chat.tracking"];
export const CHAT_LOST_UNSENT_NOTICE = ptBR["chat.lostUnsent"];
export const CHAT_TURN_BUSY_NOTICE = ptBR["chat.turnBusy"];
export const CHAT_TIMEOUT_NOTICE = ptBR["chat.timeout"];
export const CHAT_TIMEOUT_DRAFT_SAVED_NOTICE = ptBR["chat.timeoutDraftSaved"];
export const CHAT_CLIENT_TIMEOUT_NOTICE = ptBR["chat.clientTimeout"];

/** Rótulo por papel de agente, no idioma ativo. Agentes desconhecidos caem em "Agente". */
const AGENT_LABEL_KEYS: Readonly<Record<string, MessageKey>> = {
  architect: "role.architect",
  developer: "role.developer",
  reviewer: "role.reviewer",
  system: "role.system",
  tests: "role.tests",
  user: "role.user",
};

/** Nome exibido do papel (desconhecido: "Agente"). */
export function agentRoleLabel(kind: string): string {
  return t(AGENT_LABEL_KEYS[kind] ?? "role.unknown");
}

/** Rótulo curto por estado de tarefa (colunas do Quadro, na ordem da máquina de estados), no idioma ativo. */
export const TASK_STATUS_LABEL: Readonly<Record<TaskStatus, string>> = localized({
  TODO: "board.status.TODO",
  IN_PROGRESS: "board.status.IN_PROGRESS",
  IN_REVIEW: "board.status.IN_REVIEW",
  CHANGES_REQUESTED: "board.status.CHANGES_REQUESTED",
  APPROVED: "board.status.APPROVED",
  INTEGRATING: "board.status.INTEGRATING",
  DONE: "board.status.DONE",
  BLOCKED: "board.status.BLOCKED",
});

/** Chip do último veredito de revisão (`NONE` não aparece), no idioma ativo. */
export const TASK_VERDICT_LABEL: Readonly<Record<"APPROVED" | "CHANGES_REQUESTED", string>> = localized({
  APPROVED: "board.verdict.APPROVED",
  CHANGES_REQUESTED: "board.verdict.CHANGES_REQUESTED",
});

/** Horário relativo no idioma ativo, sem ticker: calculado no render. */
export function relativeTime(at: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - at) / 1000));
  if (seconds < 45) return t("time.now");
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t("time.minutesAgo", { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t("time.hoursAgo", { count: hours });
  return t("time.daysAgo", { count: Math.floor(hours / 24) });
}

// ---------- Executar para testar (spec 021) ----------

const APP_REASON_KEYS: ReadonlyMap<string, MessageKey> = new Map<string, MessageKey>([
  ["already_running", "app.error.already_running"],
  ["port_in_use", "app.error.port_in_use"],
  ["busy", "app.error.busy"],
  ["spec_not_completed", "app.error.spec_not_completed"],
  ["no_run_command", "app.error.no_run_command"],
  ["run_command_docker", "app.error.run_command_docker"],
  ["docker_port_in_use", "app.error.docker_port_in_use"],
  ["docker_failed", "app.error.docker_failed"],
  ["environment_not_docker", "app.error.environment_not_docker"],
  ["report_in_progress", "app.error.report_in_progress"],
]);

/** Motivo de uma falha de início/parada do app de teste, no idioma ativo (pelo `reason`, senão pelo status). */
export function appErrorMessage(status: number, reason: string | undefined): string {
  const key = reason === undefined ? undefined : APP_REASON_KEYS.get(reason);
  if (key !== undefined) return t(key);
  if (status === 400) return t("app.error.notConfirmed");
  if (status === 409) return t("app.error.cantNow");
  if (status === 422) return t("app.error.no_run_command");
  return t("app.error.generic");
}

const APP_EXIT_REASON_KEYS: ReadonlyMap<string, MessageKey> = new Map<string, MessageKey>([
  ["exited", "app.exit.why.exited"],
  ["stopped", "app.exit.why.stopped"],
  ["run_started", "app.exit.why.run_started"],
  ["project_removed", "app.exit.why.project_removed"],
  ["shutdown", "app.exit.why.shutdown"],
]);

/** "Encerrado (código 1): o programa terminou.", no idioma ativo. */
export function appExitedText(code: number | null, reason: string | null): string {
  if (reason === "stopped") return t("app.exit.stopped");
  const whyKey = reason === null ? undefined : APP_EXIT_REASON_KEYS.get(reason);
  if (whyKey === undefined) return code === null ? t("app.exit.plain") : t("app.exit.code", { code });
  const why = t(whyKey);
  return code === null ? t("app.exit.reason", { reason: why }) : t("app.exit.codeReason", { code, reason: why });
}

/** Motivo fixo do evento `report_failed` (código do servidor), pela chave do dicionário. */
const REPORT_FAILURE_KEYS: ReadonlyMap<string, MessageKey> = new Map<string, MessageKey>([
  ["busy", "report.failure.busy"],
  ["not_configured", "report.failure.not_configured"],
  ["spec_unreadable", "report.failure.spec_unreadable"],
  ["timeout", "report.failure.timeout"],
  ["shutdown", "report.failure.shutdown"],
  ["write_failed", "report.failure.write_failed"],
  ["git_unsupported", "report.failure.git_unsupported"],
  ["internal", "report.failure.internal"],
]);

/** Mensagem humana para o código de `report_failed`; `provider_*` e desconhecidos têm texto fixo. */
export function reportFailureMessage(code: string): string {
  const known = REPORT_FAILURE_KEYS.get(code);
  if (known !== undefined) return t(known);
  if (code.startsWith("provider_")) return t("report.failure.provider");
  return t("report.failure.fallback");
}

/** Reservas pt-BR da escalada (a tela usa as chaves `recovery.*`). */
export const RECOVERY_OTHER_PATH = ptBR["recovery.otherPath"];
export const RECOVERY_ENV_WARNING = ptBR["recovery.envWarning"];
export const RECOVERY_GUIDANCE_LABEL = ptBR["recovery.guidanceLabel"];
export const RECOVERY_GUIDANCE_SEND = ptBR["recovery.guidanceSend"];
export const RECOVERY_GUIDANCE_MAX_BYTES = 2048;
export const RECOVERY_CONFIRM = ptBR["recovery.confirm"];
/** `reason` de 409 do recovery-choice, no idioma ativo. */
export const RECOVERY_REASON_MESSAGES: Readonly<Record<string, string>> = localized({
  report_in_progress: "recovery.reason.report_in_progress",
  budget_exceeded: "recovery.reason.budget_exceeded",
  environment_not_docker: "approve.dockerRequired",
  docker_unavailable: "approve.dockerUnavailable",
});

/** "Buscas na web: N (US$ x)" no resumo de custo, no idioma ativo. */
export function webSearchCostLine(calls: number, usd: string): string {
  return t("costs.webSearches", { count: formatNumber(calls), usd });
}
/** Dica do custo (spec 092 T03): "inclui N imagens (estimado)", no idioma ativo. */
export function imagesCostHint(count: number, estimated: boolean): string {
  const hint = tp("costs.includesImages", count);
  return estimated ? `${hint} ${t("costs.estimated")}` : hint;
}
/** "Imagens geradas: N (US$ x)" no resumo de custo, com "estimado" sem preço por imagem, no idioma ativo. */
export function imagesCostLine(count: number, usd: string, estimated: boolean): string {
  return t(estimated ? "costs.imagesLineEstimated" : "costs.imagesLine", { count: formatNumber(count), usd });
}
