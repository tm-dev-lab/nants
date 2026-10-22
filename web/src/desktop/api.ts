/*
 * Rotas do servidor usadas pelo shell do desktop (spec 093, T01/T02; contratos em api-notes.md):
 * - `GET /api/attention`: o que espera o usuário, de todos os projetos;
 * - `GET /api/readiness` e `POST /api/readiness/refresh`: prontidão em níveis;
 * - `POST /api/projects {name}`: novo projeto na pasta gerenciada (409 `project_dir_exists`).
 *
 * Tudo passa pelo `request` do cliente (único ponto de `fetch`). As respostas são validadas
 * campo a campo; o que não confere é descartado. Mensagens de erro são fixas.
 */
import { createProject, request, type ApiFailure, type ApiResult } from "../api/client";
import type { Project } from "../api/types";
import { isMessageCode, parseCodeParams, type CodeParams } from "../api/codes";
import type { MessageKey } from "../i18n";
import { ptBR } from "../i18n/ptBR";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const str = (value: unknown, max = 512): string | null =>
  typeof value === "string" && value.length > 0 && value.length <= max ? value : null;

// --- Atenção ---

type AttentionAgent = "developer" | "reviewer" | "architect";
type AttentionPermissionKind = "command" | "environment" | "final_fix";

interface AttentionPermission {
  request_id: string;
  agent: AttentionAgent;
  kind: AttentionPermissionKind;
  task_id?: string;
  always_allowed: boolean;
}

interface EscalationOptionRef {
  id: string;
  kind: "guidance" | "config" | "environment" | "revise_task";
}

export type PausedOrStalled =
  | { kind: "paused"; spec_id: string; task_id: string; analysis?: string; escalation_options?: EscalationOptionRef[] }
  | { kind: "stalled"; spec_id: string };

export interface AttentionProject {
  project_id: string;
  name: string;
  pending_permissions: AttentionPermission[];
  pending_choices: boolean;
  plan_awaiting_approval: boolean;
  paused_or_stalled: PausedOrStalled | null;
}

export interface Attention {
  setup_incomplete: boolean;
  projects: AttentionProject[];
}

const AGENTS: readonly string[] = ["developer", "reviewer", "architect"];
const PERMISSION_KINDS: readonly string[] = ["command", "environment", "final_fix"];
const ESCALATION_KINDS: readonly string[] = ["guidance", "config", "environment", "revise_task"];

function parsePermission(value: unknown): AttentionPermission | null {
  if (!isRecord(value)) return null;
  const requestId = str(value.request_id, 128);
  if (requestId === null) return null;
  if (typeof value.agent !== "string" || !AGENTS.includes(value.agent)) return null;
  if (typeof value.kind !== "string" || !PERMISSION_KINDS.includes(value.kind)) return null;
  const out: AttentionPermission = {
    request_id: requestId,
    agent: value.agent as AttentionAgent,
    kind: value.kind as AttentionPermissionKind,
    always_allowed: value.always_allowed === true,
  };
  const taskId = str(value.task_id, 64);
  if (taskId !== null) out.task_id = taskId;
  return out;
}

function parsePaused(value: unknown): PausedOrStalled | null {
  if (!isRecord(value)) return null;
  const specId = str(value.spec_id, 128);
  if (specId === null) return null;
  if (value.kind === "stalled") return { kind: "stalled", spec_id: specId };
  if (value.kind !== "paused") return null;
  const taskId = str(value.task_id, 64);
  if (taskId === null) return null;
  const out: PausedOrStalled = { kind: "paused", spec_id: specId, task_id: taskId };
  const analysis = str(value.analysis, 16_384);
  if (analysis !== null) out.analysis = analysis;
  if (Array.isArray(value.escalation_options)) {
    out.escalation_options = value.escalation_options.flatMap((o): EscalationOptionRef[] => {
      if (!isRecord(o)) return [];
      const id = str(o.id, 64);
      return id !== null && typeof o.kind === "string" && ESCALATION_KINDS.includes(o.kind)
        ? [{ id, kind: o.kind as EscalationOptionRef["kind"] }]
        : [];
    });
  }
  return out;
}

/** Valida o corpo de `/api/attention`; inválido vira `null`. */
export function parseAttention(value: unknown): Attention | null {
  if (!isRecord(value) || !Array.isArray(value.projects)) return null;
  const projects = value.projects.flatMap((p): AttentionProject[] => {
    if (!isRecord(p)) return [];
    const id = str(p.project_id, 128);
    if (id === null) return [];
    const permissions = Array.isArray(p.pending_permissions)
      ? p.pending_permissions.map(parsePermission).filter((x): x is AttentionPermission => x !== null)
      : [];
    return [
      {
        project_id: id,
        name: str(p.name, 256) ?? id,
        pending_permissions: permissions,
        pending_choices: p.pending_choices === true,
        plan_awaiting_approval: p.plan_awaiting_approval === true,
        paused_or_stalled: parsePaused(p.paused_or_stalled),
      },
    ];
  });
  return { setup_incomplete: value.setup_incomplete === true, projects };
}

/** Itens que exigem ação num projeto (cada permissão conta; escolhas, plano e pausa contam 1). */
export function projectAttentionCount(project: AttentionProject): number {
  return (
    project.pending_permissions.length +
    (project.pending_choices ? 1 : 0) +
    (project.plan_awaiting_approval ? 1 : 0) +
    (project.paused_or_stalled !== null ? 1 : 0)
  );
}

/** Contagem do sino: todos os projetos, mais o setup incompleto (um item). */
export function attentionCount(attention: Attention | null): number {
  if (attention === null) return 0;
  const projects = attention.projects.reduce((sum, p) => sum + projectAttentionCount(p), 0);
  return projects + (attention.setup_incomplete ? 1 : 0);
}

export function getAttention(): Promise<ApiResult<Attention>> {
  return request<unknown>({ method: "GET", path: "/api/attention" }).then((r) => {
    if (!r.ok) return r;
    const parsed = parseAttention(r.data);
    return parsed === null
      ? { ok: false, error: { ...INVALID_RESPONSE } }
      : { ok: true, data: parsed };
  });
}

// --- Prontidão ---

export type SettingsTab = "connections" | "agents" | "machine" | "pricing";
const SETTINGS_TABS: readonly SettingsTab[] = ["connections", "agents", "machine", "pricing"];

/**
 * Abas da janela Configurações (T08): as quatro de destino da prontidão mais Aparência e
 * Projetos (raiz da pasta de projetos), na ordem em que aparecem.
 */
export type SettingsWindowTab = SettingsTab | "appearance" | "projects";
export const SETTINGS_WINDOW_TABS: readonly SettingsWindowTab[] = ["connections", "agents", "pricing", "machine", "appearance", "projects"];

export function isSettingsWindowTab(value: unknown): value is SettingsWindowTab {
  return typeof value === "string" && (SETTINGS_WINDOW_TABS as readonly string[]).includes(value);
}

export interface ReadinessItem {
  id: string;
  /** Rótulo em pt-BR (reserva); a interface mostra o do dicionário pelo `code` (`readinessItemLabel`). */
  label: string;
  /** Comando de correção (só texto; o Nants nunca executa). */
  fix?: string;
  settings_tab: SettingsTab;
  /** Código estável do rótulo (spec 102 T05): `connection`, `git`, `connection_key`, `pricing`... */
  code?: string;
  /** Parâmetros neutros do rótulo (`name` da conexão, `model`). */
  params?: CodeParams;
}

export interface ReadinessLevel {
  ok: boolean;
  missing: ReadinessItem[];
}

export interface Readiness {
  chat: ReadinessLevel;
  create_project: ReadinessLevel;
  execute: ReadinessLevel;
  recommended: ReadinessLevel;
}

export const READINESS_LEVELS = ["chat", "create_project", "execute", "recommended"] as const;

function parseItem(value: unknown): ReadinessItem | null {
  if (!isRecord(value)) return null;
  const id = str(value.id, 160);
  const label = str(value.label, 256);
  if (id === null || label === null) return null;
  const tab = SETTINGS_TABS.find((t) => t === value.settings_tab) ?? "machine";
  const item: ReadinessItem = { id, label, settings_tab: tab };
  const fix = str(value.fix, 1024);
  if (fix !== null) item.fix = fix;
  if (isMessageCode(value.code)) {
    item.code = value.code;
    const params = parseCodeParams(value.params);
    if (params !== null) item.params = params;
  }
  return item;
}

function parseLevel(value: unknown): ReadinessLevel | null {
  if (!isRecord(value) || typeof value.ok !== "boolean") return null;
  const missing = Array.isArray(value.missing)
    ? value.missing.map(parseItem).filter((x): x is ReadinessItem => x !== null)
    : [];
  return { ok: value.ok, missing };
}

/** Valida o corpo de `/api/readiness`; inválido vira `null`. */
export function parseReadiness(value: unknown): Readiness | null {
  if (!isRecord(value)) return null;
  const chat = parseLevel(value.chat);
  const create = parseLevel(value.create_project);
  const execute = parseLevel(value.execute);
  if (chat === null || create === null || execute === null) return null;
  return { chat, create_project: create, execute, recommended: parseLevel(value.recommended) ?? { ok: true, missing: [] } };
}

/** Setup incompleto: algum nível obrigatório pendente (o recomendado não bloqueia). */
export function setupIncomplete(readiness: Readiness | null): boolean {
  return readiness !== null && (!readiness.chat.ok || !readiness.create_project.ok || !readiness.execute.ok);
}

/** Pendências obrigatórias sem repetição (a mesma pendência aparece em vários níveis). */
export function requiredMissing(readiness: Readiness): ReadinessItem[] {
  const seen = new Set<string>();
  const out: ReadinessItem[] = [];
  for (const level of [readiness.chat, readiness.create_project, readiness.execute]) {
    for (const item of level.missing) {
      if (seen.has(item.id)) continue;
      seen.add(item.id);
      out.push(item);
    }
  }
  return out;
}

function readinessResult(r: ApiResult<unknown>): ApiResult<Readiness> {
  if (!r.ok) return r;
  const parsed = parseReadiness(r.data);
  return parsed === null
    ? { ok: false, error: { ...INVALID_RESPONSE } }
    : { ok: true, data: parsed };
}

export function getReadiness(): Promise<ApiResult<Readiness>> {
  return request<unknown>({ method: "GET", path: "/api/readiness" }).then(readinessResult);
}

/** Verifica a máquina de novo (teto de 8 s no servidor). */
export function refreshReadiness(): Promise<ApiResult<Readiness>> {
  return request<unknown>({ method: "POST", path: "/api/readiness/refresh", timeoutMs: 15_000 }).then(readinessResult);
}

// --- Novo projeto por nome ---

/** Reservas pt-BR das falhas fixas de criação (a interface mostra a chave no idioma ativo). */
export const NAME_INVALID_MESSAGE = ptBR["newProject.nameInvalid"];
export const ROOT_INVALID_MESSAGE = ptBR["newProject.rootInvalid"];
export const GIT_MISSING_MESSAGE = ptBR["newProject.gitMissing"];

export type CreateProjectOutcome =
  | { kind: "created"; project: Project }
  | { kind: "exists"; path: string }
  /**
   * `message` em pt-BR (reserva); `key` é a chave da falha fixa (nome, raiz, Git) para exibir no
   * idioma ativo. Sem `key`, a interface usa o texto da `failure` (`apiFailureText`).
   */
  | { kind: "error"; message: string; key?: MessageKey; failure: ApiFailure };

const EXISTS_REASON = "project_dir_exists";

/** Chave fixa de uma falha de criação (nome, raiz, Git), ou `undefined` (segue o catálogo do cliente). */
function createProblem(failure: ApiFailure): MessageKey | undefined {
  if (failure.status === 412 && failure.missing?.includes("git") === true) return "newProject.gitMissing";
  if (failure.status === 400) return "newProject.nameInvalid";
  if (failure.status === 422) return "newProject.rootInvalid";
  return undefined;
}

/** Falha de criação com a mensagem pt-BR de reserva e a chave fixa, quando houver. */
function createError(failure: ApiFailure, key: MessageKey | undefined): CreateProjectOutcome {
  return key === undefined ? { kind: "error", message: failure.message, failure } : { kind: "error", message: ptBR[key], key, failure };
}

const INVALID_RESPONSE: ApiFailure = {
  kind: "invalid_response",
  status: 200,
  message: ptBR["api.invalidResponse"],
  messageKey: "api.invalidResponse",
};

/** `POST /api/projects {name}`: cria `<raiz>/<slug>` (o nome exibido é o digitado). */
export async function createProjectByName(name: string): Promise<CreateProjectOutcome> {
  let existsPath: string | null = null;
  const result = await request<Project>({
    method: "POST",
    path: "/api/projects",
    body: JSON.stringify({ name }),
    onHttpError: (status, body) => {
      if (status !== 409 || !isRecord(body) || body.error !== EXISTS_REASON) return null;
      const path = str(body.path, 4096);
      if (path === null) return null;
      existsPath = path;
      return { kind: "http", status, message: "a pasta já existe", reason: EXISTS_REASON };
    },
  });
  if (result.ok) {
    return isRecord(result.data) && typeof result.data.id === "string"
      ? { kind: "created", project: result.data }
      : createError(INVALID_RESPONSE, undefined);
  }
  if (existsPath !== null) return { kind: "exists", path: existsPath };
  return createError(result.error, createProblem(result.error));
}

/** `POST /api/projects {name, path}` (pasta existente), com as mesmas mensagens fixas. */
export async function createProjectAtPath(name: string, path: string): Promise<CreateProjectOutcome> {
  const result = await createProject({ name, path });
  if (result.ok) return { kind: "created", project: result.data };
  // Aqui o 400/422 fala do caminho (mensagem fixa do servidor), não do nome.
  const failure = result.error;
  return createError(failure, failure.status === 412 && failure.missing?.includes("git") === true ? "newProject.gitMissing" : undefined);
}

// --- Pasta de projetos (T02) ---

interface ProjectsRoot {
  /** Como configurada (`null` = padrão `~/NantsProjects`). */
  projects_root: string | null;
  /** Caminho efetivo, já expandido. */
  projects_root_effective: string | null;
}

/** Mensagens fixas do 422 de `PUT /api/config/projects-root` (as únicas repassadas). */
const PROJECTS_ROOT_MESSAGES: readonly string[] = [
  "pasta de projetos inválida: use um caminho absoluto (aceita ~), sem '..'",
  "pasta de projetos inválida: diretório de sistema não é permitido",
  "pasta de projetos inválida: não pode ficar dentro de um projeto registrado",
];
const PROJECTS_ROOT_INVALID = ptBR["api.projectsRoot.invalid"];

/** Lê os dois campos da raiz numa resposta (`/api/config` ou o PUT); ausentes viram `null`. */
export function parseProjectsRoot(value: unknown): ProjectsRoot {
  const v = isRecord(value) ? value : {};
  return { projects_root: str(v.projects_root, 4096), projects_root_effective: str(v.projects_root_effective, 4096) };
}

/** `PUT /api/config/projects-root {projects_root}` (`null` volta ao padrão). */
export function putProjectsRoot(root: string | null): Promise<ApiResult<ProjectsRoot>> {
  return request<unknown>({
    method: "PUT",
    path: "/api/config/projects-root",
    body: JSON.stringify({ projects_root: root }),
    onHttpError: (status, body) => {
      if (status !== 422) return null;
      const error = isRecord(body) && typeof body.error === "string" ? body.error : "";
      const message = PROJECTS_ROOT_MESSAGES.includes(error) ? `${error.charAt(0).toUpperCase()}${error.slice(1)}.` : PROJECTS_ROOT_INVALID;
      return { kind: "http", status, message };
    },
  }).then((r) => (r.ok ? { ok: true, data: parseProjectsRoot(r.data) } : r));
}
