/*
 * Tipos do contrato REST/SSE do servidor (src/server/*.rs).
 * Espelham os structs reais de resposta/requisição; não adicionar campos sem
 * conferir o servidor. Chaves de API só aparecem como `api_key` em corpos de
 * envio (putConnection/testConnection); nunca voltam nas respostas.
 */

// ---------- Enums ----------

export type Effort = "low" | "medium" | "high";

/** `SpecStatus` (SCREAMING_SNAKE_CASE). */
export type SpecStatus = "DRAFT" | "AWAITING_APPROVAL" | "IN_PROGRESS" | "COMPLETED" | "BLOCKED";

/** `TaskStatus` (SCREAMING_SNAKE_CASE). */
export type TaskStatus =
  | "TODO"
  | "IN_PROGRESS"
  | "IN_REVIEW"
  | "CHANGES_REQUESTED"
  | "APPROVED"
  | "INTEGRATING"
  | "DONE"
  | "BLOCKED";

/** `Verdict` (SCREAMING_SNAKE_CASE). */
type Verdict = "NONE" | "APPROVED" | "CHANGES_REQUESTED";

/** Escopo pedido ou aplicado: `once` (uma vez), `run` (nesta execução), `always` (persistido) ou `deny`. */
export type PermissionDecision = "once" | "run" | "always" | "deny";

/** Protocolo HTTP da conexão (OpenAI-compatível): Chat Completions ou Responses. */
export type WireApi = "chat" | "responses";

// ---------- Configuração global (/api/config) ----------

interface ServerView {
  host: string;
  port: number;
}

/** Conexão como exibida: a chave aparece só mascarada. */
export interface ConnectionView {
  id: string;
  name: string;
  base_url: string;
  api_key_masked: string;
  /** Ausente em servidores antigos: a UI trata como `chat`. */
  wire_api?: WireApi;
}

/**
 * Corpo de `PUT /api/config/connections`. `api_key` vazia mantém a chave salva.
 * `wire_api` ausente mantém o valor salvo (conexão nova = chat); o cliente sempre envia.
 */
export interface ConnectionInput {
  id: string;
  name: string;
  base_url: string;
  api_key?: string;
  wire_api?: WireApi;
}

export interface AgentConfig {
  connection_id: string;
  model: string;
  effort: Effort;
  /** Teto de passos do laço de ferramentas (10–1000); ausente = padrão do papel (spec 081). */
  max_steps?: number;
}

/** Corpo de `PUT /api/config/agents` e campo `agents` de `GET /api/config`. */
export interface Agents {
  architect: AgentConfig;
  developer: AgentConfig;
  reviewer: AgentConfig;
}

export interface ProjectEntry {
  id: string;
  name: string;
  path: string;
  created_at: string;
}

export interface ConfigView {
  server: ServerView;
  connections: ConnectionView[];
  agents: Agents;
  projects: ProjectEntry[];
  /** Preço por modelo (US$ por 1M tokens); ausente em servidores antigos. */
  pricing?: Record<string, ModelPricing>;
  /** Acesso pela rede (spec 097); ausente em servidores antigos. */
  network?: NetworkView;
  /** Versão do Nants (spec 103 T03), vinda do `Cargo.toml`; ausente em servidores antigos. */
  version?: string;
}

/** Faixa de portas de visualização (spec 097), inclusiva. */
export interface LanRange {
  start: number;
  end: number;
}

/**
 * Acesso pela rede (spec 097, T01): porta efetiva do Nants, faixa de portas de visualização (`null`
 * fora do `--lan`) e se o servidor roda no WSL. Nenhum IP é exposto.
 */
export interface NetworkView {
  nants_port: number;
  lan_view_ports: LanRange | null;
  wsl: boolean;
}

/** Preço de um modelo em US$ por 1 milhão de tokens. */
export interface ModelPricing {
  input_per_mtok: number;
  output_per_mtok: number;
  cached_input_per_mtok?: number;
  /** US$ por imagem gerada (spec 092 T03); ausente = estimativa padrão por qualidade. */
  image_per_unit?: number;
  /** Origem; ausente (formato antigo) = manual. */
  source?: "manual" | "estimated";
  /** Data (AAAA-MM-DD) do catálogo da estimativa. */
  estimated_at?: string;
}

/** Totais de uso (`GET /api/projects/:id/usage`). `cost_usd` é null sem preço. */
export interface UsageTotals {
  calls: number;
  input: number;
  output: number;
  cached: number;
  reasoning: number;
  cost_usd: number | null;
  unpriced_calls: number;
  /** O custo usa preço estimado (catálogo), não digitado. */
  cost_estimated?: boolean;
  /** Buscas na web somadas (spec 076; omitido quando zero). */
  web_search_calls?: number;
  /** US$ das buscas na web, já incluído em `cost_usd` (omitido quando zero). */
  web_search_usd?: number;
  /** Imagens geradas somadas (spec 092 T03; omitido quando zero). */
  images?: number;
  /** US$ das imagens, já incluído em `cost_usd` (omitido quando zero). */
  images_usd?: number;
  /** Alguma imagem usou a estimativa padrão por qualidade (sem preço por imagem). */
  images_estimated?: boolean;
}

export interface NamedUsage extends UsageTotals {
  id: string;
}

/** Totais de uma tarefa com o tempo de trabalho (086/T06; campos aditivos, opcionais no contrato antigo). */
export interface TaskUsage extends NamedUsage {
  /** Primeiro registro (uso ou ação), ISO UTC. */
  first_at?: string | null;
  /** Último registro (uso ou ação), ISO UTC. */
  last_at?: string | null;
  /** Segundos ativos: intervalos entre registros, sem pausas maiores que 10 min. */
  active_secs?: number;
}

interface SpecUsage extends UsageTotals {
  spec: string | null;
  tasks: TaskUsage[];
  agents: NamedUsage[];
}

/** Orçamento opcional de uma spec. */
export interface SpecBudget {
  max_tokens?: number;
  max_usd?: number;
}

export interface UsageView {
  specs: SpecUsage[];
  total: UsageTotals;
  /** Cada chamada é precificada pelo próprio modelo; o total é a soma. */
  by_model?: NamedUsage[];
  by_agent?: NamedUsage[];
  /** Só na consulta do projeto inteiro (sem filtro de spec); chat sem spec vem como `chat`. */
  by_spec?: NamedUsage[];
  budget: SpecBudget | null;
}

/** Corpo de `POST /api/config/connections/test`. */
export interface TestConnectionInput {
  id?: string;
  base_url?: string;
  api_key?: string;
  model?: string;
}

/** Resultado do teste: `error` é sempre um código fixo do servidor. */
export interface TestConnectionResult {
  ok: boolean;
  latency_ms?: number;
  error?: string;
  status?: number;
  /** Quantidade de modelos listados (só no teste sem `model`). */
  models_count?: number;
}

/** Um modelo listado por `GET /api/config/connections/:id/models`. */
interface ModelEntry {
  id: string;
}

/**
 * Resposta de `GET /api/config/connections/:id/models`. Falha do provedor vem em 200 com
 * `models: []` e `error` (código fixo); `status` só em `http_error`.
 */
export interface ModelsResult {
  connection_id?: string;
  models: ModelEntry[];
  truncated?: boolean;
  fetched_at?: number;
  error?: string;
  status?: number;
}

// ---------- Projetos e SDD ----------

/** Resposta de `POST /api/projects`. */
export interface Project {
  id: string;
  name: string;
  path: string;
  created_at: string;
  is_git_repo: boolean;
  initialized: boolean;
  /** `true` quando o Nants executou `git init` e/ou o commit inicial. */
  git_initialized?: boolean;
}

interface RunView {
  spec_id: string;
  status: string;
}

export interface TaskView {
  id: string;
  title: string;
  status: TaskStatus | null;
  attempts: number;
  max_attempts: number;
  depends_on: string[];
  last_review_verdict: Verdict | null;
  /** Motivo do bloqueio (redigido e limitado pelo servidor); ausente quando não há. */
  next_action?: string;
  error?: string;
  /** Opções de recuperação vigentes (só em tarefa BLOCKED); ver spec 073. */
  recovery_options?: RecoveryOption[];
  /** Número da análise das opções; devolvido sem alteração ao escolher. */
  recovery_options_analysis?: number;
}

export interface SpecView {
  id: string;
  title: string;
  status: SpecStatus | null;
  tasks: TaskView[];
  error?: string;
  /** Ambiente docker proposto no plano (spec em rascunho); ausente = host. */
  environment?: EnvironmentView;
  /** Itens da seção "Premissas" do spec.md, quando o servidor os expõe. */
  premises?: string[];
  /** Orientações por papel (Desenvolvedor/Revisor) escritas pelo Arquiteto no spec.md. */
  role_guidance?: { role: string; text: string }[];
  /** Perfis de especialista do projeto (.sdd/skills) e sua situação em relação a esta spec. */
  skills?: SkillProfileView[];
  /** Avisos de critério que cita caminho fora do allowed_scope (não bloqueiam). */
  scope_warnings?: string[];
  /** Avisos de cobertura dos critérios globais pelo `covers` das tarefas (não bloqueiam). */
  coverage_warnings?: string[];
  /** Ações gravadas em tools.jsonl por id de tarefa (`FINAL` = verificação final); ausente = nenhuma. */
  tool_counts?: Record<string, number>;
  /** Revisão do plano pelo Revisor (spec 086 T08), só da versão atual do plano; ausente = sem revisão. */
  plan_review?: PlanReviewView;
  /** O que mudou no plano desde a versão anterior (spec 086 T09), só da versão atual; ausente = sem mudanças. */
  plan_changes?: string[];
}

/** Apontamento objetivo da revisão do plano. `task` null = problema da spec. */
export interface PlanIssue {
  task: string | null;
  kind: "contradiction" | "untestable" | "scope" | "uncovered_criterion" | "missing_dependency" | string;
  text: string;
}

export interface PlanReviewView {
  /** `pending`: versão atual ainda sem revisão (até 3 min); `stale`: sem resultado depois disso (spec 092 T02). */
  status: "pending" | "running" | "stale" | "ok" | "unavailable" | string;
  at?: string;
  issues: PlanIssue[];
}

interface SkillProfileView {
  role: "developer" | "reviewer" | string;
  stack: string[];
  /** Motivo da última reescrita, quando houver. */
  reason?: string;
  status: "criado" | "atualizado" | "reaproveitado" | string;
  updated_at: string;
  text: string;
}

interface EnvironmentFileView {
  path: string;
  content: string;
}

/** Ambiente proposto pelo Arquiteto: aprovar o plano aprova estes arquivos. */
export interface EnvironmentView {
  mode: "docker";
  services: string[];
  base_image?: string;
  files: EnvironmentFileView[];
}

/** Resposta de `GET /api/projects/:id/state`. Sem caminhos absolutos. */
export interface ProjectState {
  id: string;
  name: string;
  is_git_repo: boolean;
  initialized: boolean;
  base_branch: string;
  /** Comando de testes de `settings.yaml` (vazio quando ainda não configurado). */
  test_command: string;
  /** `true` quando `test_command` é redigido ou inválido na leitura (não editável de volta). */
  test_command_redacted: boolean;
  /** Tentativas do Desenvolvedor por tarefa nova (1–10; padrão 5). */
  max_attempts: number;
  /** Ciclos de teste por tentativa antes de bloquear (1–10; padrão 5). */
  max_test_cycles: number;
  running: boolean;
  run: RunView | null;
  specs: SpecView[];
  /** Pedidos de permissão ainda pendentes (por projeto, por criação, teto 64). */
  pending_permissions: PendingPermissionView[];
  /** `true` enquanto o Arquiteto escreve o relatório da spec integrada (aditivo; ausente = `false`). */
  report_pending?: boolean;
  /** Modo YOLO ligado (aditivo; ausente = `false`). */
  yolo?: boolean;
  /** Arquiteto pode consultar a internet (spec 076; aditivo; ausente = `false`). */
  architect_internet?: boolean;
  /** O provedor do Arquiteto tem busca na web (spec 076; aditivo; ausente = `false`). */
  web_search_available?: boolean;
  /** Turno do Arquiteto em andamento (aditivo; ausente ou `null` = sem turno). */
  architect_turn?: ArchitectTurnView | null;
}

/** Item de `pending_permissions` em `GET /api/projects/:id/state`. */
export interface PendingPermissionView {
  request_id: string;
  agent: string;
  command: string;
  task_id: string | null;
  /** Ausente = não permitido (servidor antigo): a UI não oferece "Sempre permitir". */
  always_allowed?: boolean;
  /** Motivo fixo quando `always_allowed` é falso; `null`/ausente quando permitido. */
  always_denied_reason?: string | null;
  /** `"environment"`: mudança do ambiente Docker (motivo e diff abaixo); ausente = comando. */
  kind?: string | null;
  reason?: string | null;
  /** Diff unificado em texto puro (só `environment`). */
  diff?: string | null;
}

/** Mensagem persistida do diálogo dos agentes (`GET .../specs/:spec_id/dialog`), texto já redigido. */
export interface DialogMessage {
  /** Número de sequência em microssegundos (inteiro seguro do JS), crescente no arquivo. */
  seq: number;
  /** Segundos desde a época Unix. */
  ts: number;
  agent: string;
  task_id: string | null;
  attempt: number | null;
  text: string;
  /** Código estável da mensagem do sistema (spec 102); a interface a mostra pelo `code`. */
  code?: string;
  /** Parâmetros do `code` (textos e números). */
  params?: Readonly<Record<string, string | number>>;
}

/** Resposta de `GET .../specs/:spec_id/dialog`: página ascendente após `after`. */
export interface DialogView {
  messages: DialogMessage[];
  /** Maior seq do arquivo no momento da leitura (0 sem mensagens). */
  last_seq: number;
  /** `true` quando há mensagens mais antigas que a janela lida (rotação do arquivo). */
  truncated_before: boolean;
}

/** Uma ação da trilha (`tools.jsonl`): `seq` é a posição da linha no arquivo. */
export interface ToolActionItem {
  seq: number;
  /** Segundos desde a época Unix. */
  ts: number;
  agent: string;
  task_id: string | null;
  /** "tool" (ferramenta) ou "command" (comando). */
  kind: string;
  text: string;
  stage?: string;
  /** `code`/`params` do rótulo e `code` da etapa (spec 102); linhas antigas não têm. */
  code?: string;
  params?: Readonly<Record<string, string | number>>;
  stage_code?: string;
}

/** Resposta de `GET .../specs/:spec_id/tools`: página cronológica, paginada para trás. */
export interface ToolActionsView {
  items: ToolActionItem[];
  /** Total de ações do filtro (todas as páginas). */
  total: number;
  /** `true` quando há ações anteriores à primeira da página. */
  has_more: boolean;
}

/** Resposta de `POST /api/projects/:id/chat`. `turn_id` também vai no `agent_message`. */
export interface ChatReply {
  reply: string;
  draft_updated: boolean;
  turn_id: string;
}

/** Turno do histórico do chat (`GET /api/projects/:id/chat`), texto já redigido pelo servidor. */
export interface ChatHistoryTurn {
  role: "user" | "architect";
  text: string;
  /** Segundos desde a época Unix. */
  ts: number;
  /** Identidade do turno (hex, até 64 caracteres); ausente em linhas antigas. */
  turn_id?: string;
  /** Imagens anexadas pelo usuário (spec 082 T05); ausente ou vazio sem imagens. */
  attachments?: string[];
}

/** Resposta de `POST /api/projects/:id/chat/attachments` (201). */
export interface ChatAttachmentUpload {
  id: string;
  name: string;
}

/** Resposta de `GET /api/projects/:id/chat`: até 40 turnos, do mais antigo para o mais novo. */
export interface ChatHistoryView {
  turns: ChatHistoryTurn[];
}

/** Resposta de `POST /api/projects/:id/specs/:spec_id/approve` (202). */
export interface ApproveResult {
  ok: boolean;
  spec_id: string;
}

/** Resposta de `POST /api/projects/:id/permissions/:req_id`: `applied` é o escopo realmente aplicado. */
export interface ResolveResult {
  ok: boolean;
  applied: PermissionDecision;
}

/** Resposta de `GET /api/fs/browse`. */
interface BrowseEntry {
  name: string;
  path: string;
  is_git_repo: boolean;
}

export interface BrowseView {
  path: string;
  parent: string | null;
  entries: BrowseEntry[];
  truncated: boolean;
}

// ---------- Eventos (/api/events) ----------

/** Envelope comum a todos os eventos com nome (BLUEPRINT §8). */
interface ApiEventEnvelope<T extends string, P> {
  event_type: T;
  project_id: string | null;
  payload: P;
}

/** Natureza do bloqueio (aditivo; ausente = servidor antigo). */
export type BlockKind = "quality" | "environmental";

/** `recovery_started`: o Arquiteto começou a analisar a tarefa bloqueada. */
interface RecoveryStartedPayload {
  spec_id: string;
  task_id: string;
  cycle: number;
}

type RecoveryDecision = "RETRY" | "ESCALATE";

type RecoveryOptionKind = "config" | "environment" | "guidance" | "revise_task";

/** Opção de recuperação gerada pelo Arquiteto e validada pelo servidor (spec 073). */
export interface RecoveryOption {
  id: string;
  label: string;
  kind: RecoveryOptionKind;
  test_command?: string;
  checks?: string[];
  text?: string;
  objective?: string;
  acceptance?: string[];
  allowed_scope?: string[];
  /** Config: "Antes:\n…\n\nDepois:\n…". */
  diff?: string;
  /** Environment: arquivos do ambiente docker propostos (o servidor também preenche `diff`). */
  files?: EnvironmentFileView[];
}

/** Corpo do POST recovery-choice: uma opção (com o número da análise) ou texto livre. */
export type RecoveryChoiceBody = { option_id: string; analysis: number } | { guidance: string };

export interface RecoveryChoiceResult {
  ok: boolean;
  spec_id: string;
  task_id: string;
  applied?: string;
}

/** `recovery_decided`: decisão do Arquiteto para o ciclo. */
interface RecoveryDecidedPayload extends RecoveryStartedPayload {
  decision: RecoveryDecision;
  options?: RecoveryOption[];
  options_analysis?: number;
}

type ProjectStatusPayload =
  | { status: "executing"; spec_id: string }
  | { status: "integrated"; spec_id: string }
  | { status: "blocked"; task_id: string; reason: string; kind?: BlockKind }
  | { status: "idle"; reason: string; needs_manual_recovery?: boolean };

interface AgentMessagePayload {
  agent: string;
  text: string;
  /** Identidade do turno do Arquiteto (hex de 32 caracteres); ausente em mensagens antigas. */
  turn_id?: string;
  /** Tarefa do diálogo; `null` ou ausente quando a mensagem não é de uma tarefa. */
  task_id?: string | null;
  /** Tentativa da tarefa, quando informada. */
  attempt?: number | null;
  /** Seq em `dialog.jsonl`; `null` quando a linha ainda não foi gravada. */
  seq?: number | null;
  /** Spec do destino (servidor); `null` ou ausente quando a mensagem não tem destino (servidor antigo). */
  spec_id?: string | null;
  /** Código estável da mensagem do sistema (spec 102), com `params`; ausente nas demais. */
  code?: string;
  params?: Readonly<Record<string, string | number>>;
}

interface TaskUpdatedPayload {
  spec_id: string;
  task_id: string;
  status: TaskStatus;
  integrated?: boolean;
}

interface PermissionRequestedPayload {
  request_id: string;
  project_id: string;
  /** `null` quando o comando não está associado a uma tarefa (fora da allowlist). */
  task_id: string | null;
  agent: string;
  command: string;
  /** Ausente = não permitido: não oferecer "Sempre permitir". */
  always_allowed?: boolean;
  always_denied_reason?: string | null;
  kind?: string | null;
  reason?: string | null;
  diff?: string | null;
}

/** Pedido resolvido (qualquer origem): remove o cartão; `applied` é o escopo aplicado. */
interface PermissionResolvedPayload {
  request_id: string;
  project_id: string;
  applied: PermissionDecision;
}

interface SddDraftUpdatedPayload {
  project_id: string;
}

/** Payload de `project_removed`: o projeto saiu do Nants (arquivos no disco intactos). */
interface ProjectRemovedPayload {
  project_id: string;
}

/** `report_ready`: o relatório da spec foi gravado (o cliente recarrega `GET .../report`). */
interface ReportReadyPayload {
  spec_id: string;
}

/** `finalization_done`: a vaga pós-integração (relatório + ARCHITECTURE.md) foi liberada. */
interface FinalizationDonePayload {
  spec_id: string;
}

/** `report_failed`: código fixo do servidor (ex.: `git_unsupported`, `provider_http_400`). */
interface ReportFailedPayload {
  spec_id: string;
  code: string;
}

/** Resposta de `GET /api/projects/:id/specs/:spec_id/report` (texto já limitado pelo servidor). */
export interface SpecReport {
  markdown: string;
  /** Data de gravação (UTC, ISO 8601). */
  generated_at: string;
  source: string;
}

/** `app_started`: o app de teste da spec subiu (`url` pode vir vazia/ausente em CLI). */
interface AppStartedPayload {
  spec_id: string;
  url: string | null;
}

/** `app_exited`: o app de teste terminou (`code` nulo quando morto por sinal). */
interface AppExitedPayload {
  code: number | null;
  reason: string;
}

/** `chat_turn_started`: o Arquiteto começou um turno (`user_text` já redigido; ausente em turno interno). */
interface ChatTurnStartedPayload {
  project_id: string;
  turn_id: string;
  user_text?: string;
  started_at: number;
}

/** `chat_turn_ended`: o turno terminou (sucesso, erro, timeout ou abort). */
interface ChatTurnEndedPayload {
  project_id: string;
  turn_id: string;
  ok: boolean;
}

/** `architect_turn` do estado do projeto (turno do Arquiteto em andamento). */
interface ArchitectTurnView {
  turn_id: string;
  user_text: string;
  started_at: number;
}

/** `agent_activity`: evento efêmero (nunca persistido). `reasoning`/`text` são snapshots do acumulado. */
type AgentActivityKind = "start" | "reasoning" | "text" | "tool" | "command" | "heartbeat" | "command_end" | "end";

export interface AgentActivityPayload {
  agent: string;
  task_id?: string | null;
  spec_id?: string | null;
  activity_id: string;
  kind: AgentActivityKind;
  text?: string;
  /** Só em `command`: início do comando (ms desde a época Unix). */
  command_started_at?: number;
  /** Rótulo da etapa (ex.: "Atualizando ARCHITECTURE.md"), reserva em pt-BR. */
  stage?: string;
  /** `code`/`params` da etapa e do texto (rótulo de ferramenta, aviso do limitador), spec 102. */
  stage_code?: unknown;
  stage_params?: unknown;
  code?: unknown;
  params?: unknown;
  /** Só no agente `tests` (spec 071): testes ou checagem, e a tentativa da tarefa. */
  run_kind?: "tests" | "check";
  attempt?: number;
}

/** Item de `GET /api/projects/:id/activity` (instantâneo da atividade ao vivo). */
export interface ActivitySnapshot {
  agent: string;
  task_id: string | null;
  spec_id: string | null;
  activity_id: string;
  reasoning: string;
  text: string;
  tool: string;
  command?: string | null;
  command_started_at?: number | null;
  stage?: string | null;
  /** `code`/`params` da etapa, do último rótulo e do último texto (spec 102). */
  stage_code?: string | null;
  tool_code?: string | null;
  tool_params?: Readonly<Record<string, string | number>> | null;
  text_code?: string | null;
  text_params?: Readonly<Record<string, string | number>> | null;
  age_ms: number;
}

export type ApiEvent =
  | ApiEventEnvelope<"project_status", ProjectStatusPayload>
  | ApiEventEnvelope<"agent_activity", AgentActivityPayload>
  | ApiEventEnvelope<"agent_message", AgentMessagePayload>
  | ApiEventEnvelope<"chat_turn_started", ChatTurnStartedPayload>
  | ApiEventEnvelope<"chat_turn_ended", ChatTurnEndedPayload>
  | ApiEventEnvelope<"task_updated", TaskUpdatedPayload>
  | ApiEventEnvelope<"permission_requested", PermissionRequestedPayload>
  | ApiEventEnvelope<"permission_resolved", PermissionResolvedPayload>
  | ApiEventEnvelope<"sdd_draft_updated", SddDraftUpdatedPayload>
  | ApiEventEnvelope<"project_removed", ProjectRemovedPayload>
  | ApiEventEnvelope<"report_ready", ReportReadyPayload>
  | ApiEventEnvelope<"report_failed", ReportFailedPayload>
  | ApiEventEnvelope<"finalization_done", FinalizationDonePayload>
  | ApiEventEnvelope<"recovery_started", RecoveryStartedPayload>
  | ApiEventEnvelope<"recovery_decided", RecoveryDecidedPayload>
  | ApiEventEnvelope<"app_started", AppStartedPayload>
  | ApiEventEnvelope<"app_exited", AppExitedPayload>;

export type ApiEventType = ApiEvent["event_type"];

/** Evento SSE `lagged`: assinante perdeu `skipped` eventos. Não tem envelope. */
export interface LaggedEvent {
  skipped: number;
}

// ---------- Erros ----------

// ---------- Resultado da spec (GET /api/projects/:id/specs/:spec_id/result) ----------

/** Tarefa no resultado. `error` (texto fixo) quando a tarefa não pôde ser lida. */
export interface ResultTask {
  id: string;
  title: string;
  status: TaskStatus | null;
  error?: string;
}

/** Commit de integração na base (`feat(<spec>): Txx - ...`). */
export interface ResultCommit {
  hash: string;
  subject: string;
  task_id: string;
  /** ISO 8601 (UTC). */
  date: string;
}

/** `A` criado, `M` alterado, `D` removido. */
type FileChange = "A" | "M" | "D";

export interface ResultFile {
  path: string;
  change: FileChange;
}

/** Trecho inicial do README (texto puro, limitado pelo servidor). */
export interface ResultReadme {
  text: string;
  truncated: boolean;
}

/** Resposta de `GET /api/projects/:id/specs/:spec_id/result`. */
export interface SpecResult {
  spec_id: string;
  title: string;
  status: SpecStatus | null;
  tasks: ResultTask[];
  commits: ResultCommit[];
  files: ResultFile[];
  files_truncated: boolean;
  readme: ResultReadme | null;
  project_path: string;
  test_command: string | null;
  /** Como executar o sistema (spec 021); ausente/nulo em specs antigas ou quando o servidor não informa. */
  run_command?: string | null;
  /** URL declarada (só loopback com porta; revalidada no cliente antes de virar link). */
  run_url?: string | null;
  /** `docker` quando o projeto roda em Compose (spec 030); ausente = host. */
  environment?: "host" | "docker";
}

/** Item do diagnóstico de pré-requisitos (GET /api/system/prereqs). */
export interface PrereqItem {
  id: string;
  label: string;
  ok: boolean;
  version?: string;
  detail?: string;
  fix?: string;
  /**
   * Detalhe na forma neutra de idioma (spec 102 T05): `prereq.detail.<detail_code>` com os
   * `detail_params`; numa lista, os itens em `detail_parts` (`{code, params}`). Validados na hora de
   * exibir (`prereqDetail`); o `detail` em pt-BR fica de reserva.
   */
  detail_code?: unknown;
  detail_params?: unknown;
  detail_parts?: unknown;
}
