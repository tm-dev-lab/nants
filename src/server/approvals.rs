//! Aprovações HITL de comandos fora da allowlist (BLUEPRINT §6.2).
//!
//! - [`ApprovalRegistry`] guarda, por `request_id` (128 bits aleatórios em hex), o pedido pendente:
//!   projeto dono, canal `oneshot` de resposta, comando já redigido e instante de criação.
//!   Nenhum I/O ocorre sob o lock do registro.
//! - [`ProjectApprover`] implementa [`Approver`] para um projeto: registra o pedido, publica
//!   `permission_requested` (comando redigido e truncado) e aguarda a decisão com timeout.
//!   Timeout, cancelamento (future descartado) ou canal fechado resultam em `Deny`, e o pedido
//!   é sempre removido do registro.
//! - `POST /api/projects/:id/permissions/:req_id` resolve o pedido. Só o projeto dono resolve;
//!   pedido desconhecido, de outro projeto ou já resolvido responde 404 com a mesma mensagem.
//! - `always` acrescenta o comando literal a `allowed_commands` de `.sdd/settings.yaml`, somente
//!   se `allowlist::is_always_allowable` aprovar. A escrita é atômica e segue as regras de
//!   `safe_fs` (sem symlink, arquivo regular, limite de tamanho). Quando não é persistível, o
//!   escopo aplicado é `run` (se houver run) ou `once`. A resposta traz `applied` com o escopo real.
//! - `run` libera o comando normalizado exato para o restante do run corrente (em memória, por
//!   projeto e por run, ver [`RunScopeGuard`]). Nunca é persistido e nunca vale para comando negado.
//!   Pedidos idênticos ainda pendentes no mesmo run recebem a mesma decisão (sem novo prompt).
//! - `test_command` do projeto (carregado no início do run) é permitido em modo exato para
//!   Developer e Reviewer, sem pedido e sem gravação.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions, Permissions};
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{oneshot, watch};

use super::error::ApiError;
use super::events::{validate_project_id, ApiEvent, EventBus, EventType, SecretSet};
use super::projects::scrub_text;
use super::safe_fs::{self, MAX_READ_BYTES, O_NOFOLLOW};
use super::state::{AppState, PAUSE_AFTER_PERMISSION_LOOKUP, PAUSE_BEFORE_PERMISSION};
use super::dialog::{deliver, DialogTarget};
use super::judge::{CommandJudge, JudgeVerdict};
use crate::harness::allowlist::{
    always_denied_reason, denylist_hit, evaluate, grant_key, is_always_allowable, yolo_container_excluded_reason, yolo_host_verdict, Decision, Policy, YoloHostVerdict,
};
use crate::harness::tools::{ApprovalDecision, ApprovalRequest, Approver, EnvChangeRequest, Role};
use crate::sdd::{docker_command, Environment, Settings};

/// Bytes aleatórios de cada `request_id` (128 bits, 32 caracteres hex).
const REQUEST_ID_BYTES: usize = 16;
/// Tamanho textual de `request_id`.
const REQUEST_ID_LEN: usize = REQUEST_ID_BYTES * 2;
/// Tempo padrão até o pedido ser negado automaticamente.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
/// Tamanho máximo do comando publicado em `permission_requested` (2 KiB).
pub const MAX_EVENT_COMMAND_BYTES: usize = 2 * 1024;
const TRUNCATED_SUFFIX: &str = "...[truncado]";
/// Arquivo de políticas do projeto, relativo à raiz.
const SETTINGS_REL: &str = ".sdd/settings.yaml";
/// Fonte de entropia do sistema.
const RANDOM_DEVICE: &str = "/dev/urandom";
/// `kind` publicado para pedidos de mudança de ambiente.
pub const ENV_KIND: &str = "environment";
/// Limite do motivo e do diff publicados.
const MAX_ENV_REASON_BYTES: usize = 1024;
const MAX_ENV_DIFF_BYTES: usize = 24 * 1024;

/// Motivo fixo da negação dos pedidos pendentes quando o usuário para a execução (spec 087 T06).
pub const STOPPED_REASON: &str = "execução parada";

/// Teto de pedidos pendentes por projeto; excedente é negado sem registro nem evento.
pub const MAX_PENDING_PER_PROJECT: usize = 64;

/// Mensagem uniforme para pedido desconhecido, de outro projeto ou já resolvido.
const PENDING_NOT_FOUND: &str = "pedido de permissão não encontrado";
/// Mensagem para projeto inexistente.
const PROJECT_NOT_FOUND: &str = "projeto não encontrado";

/// Serializa a leitura-modificação-escrita de `.sdd/settings.yaml` (todos os projetos).
/// Nunca é segurado em `.await`: só dentro de `persist_always`, em thread bloqueante.
pub(super) static PERSIST_LOCK: Mutex<()> = Mutex::new(());

/// Falha ao registrar um pedido.
#[derive(Debug)]
enum RegisterError {
    /// Teto de pendentes do projeto atingido.
    Full,
    /// Falha ao gerar o identificador aleatório.
    Entropy,
}

/// Motivo fixo quando o comando contém segredo redigido (nunca liberável com "sempre").
pub const ALWAYS_REASON_SECRET: &str =
    "este comando contém um segredo e não pode ser liberado permanentemente";

/// Motivo fixo: em modo docker, "sempre" para comando que invoca docker/podman é inútil.
pub const ALWAYS_REASON_DOCKER: &str =
    "em modo docker, comandos que invocam docker/podman não são liberados permanentemente";

/// Pedido aguardando decisão humana.
struct Pending {
    project_id: String,
    /// Raiz do projeto dono, usada por `always` para gravar `.sdd/settings.yaml`.
    project_root: PathBuf,
    tx: oneshot::Sender<ApprovalDecision>,
    /// Comando já redigido por `SecretSet` (nunca o texto bruto).
    command: String,
    /// Papel do agente que pediu (`architect`, `developer` ou `reviewer`).
    agent: &'static str,
    /// Tarefa em curso, já redigida; `None` quando não há.
    task_id: Option<String>,
    /// Run ao qual o pedido pertence; `None` fora de um run.
    run: Option<u64>,
    /// Chave da concessão `run` (comando normalizado); `None` quando o comando contém segredo.
    grant_key: Option<String>,
    /// `is_always_allowable` sobre o comando completo (antes da redação).
    always_allowed: bool,
    /// Motivo fixo quando `always_allowed` é falso.
    always_denied_reason: Option<&'static str>,
    /// Pedido de mudança de ambiente (kind `environment`): motivo e diff; nunca vira concessão
    /// `run`/`always` (qualquer permissão aplica só `once`).
    environment: Option<EnvPayload>,
    /// Ordem de criação (crescente) para listagem estável.
    seq: u64,
    #[cfg_attr(not(test), allow(dead_code))]
    created: Instant,
}

/// Motivo e diff (já redigidos e limitados) de um pedido de mudança de ambiente.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvPayload {
    reason: String,
    diff: String,
}

/// Dados de um pedido novo, já calculados a partir do comando completo.
struct PendingDraft {
    project_id: String,
    project_root: PathBuf,
    /// Comando já redigido.
    command: String,
    agent: &'static str,
    task_id: Option<String>,
    run: Option<u64>,
    grant_key: Option<String>,
    always_allowed: bool,
    always_denied_reason: Option<&'static str>,
    environment: Option<EnvPayload>,
}

/// Visão de um pedido pendente para o estado do projeto. Nunca contém o texto bruto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingView {
    pub request_id: String,
    pub agent: &'static str,
    /// Comando redigido e truncado a [`MAX_EVENT_COMMAND_BYTES`].
    pub command: String,
    pub task_id: Option<String>,
    /// `true` quando "sempre" é aceito para este comando (persistível em `allowed_commands`).
    pub always_allowed: bool,
    /// Motivo fixo para `always_allowed == false`; `None` quando é permitido.
    pub always_denied_reason: Option<&'static str>,
    /// `"environment"` para mudança de ambiente; ausente nos pedidos de comando.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    /// Motivo informado pelo agente (só `environment`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Diff unificado, texto puro (só `environment`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// Concessões em memória de um run: `run` do humano e `test_command` exato.
#[derive(Default)]
struct RunScope {
    project_id: String,
    /// Comandos normalizados liberados com "nesta execução".
    granted: HashSet<String>,
    /// `test_command` do projeto normalizado (Developer/Reviewer, modo exato).
    test_command: Option<String>,
    /// Aviso "YOLO ignorado por settings inválido" já emitido neste run (spec 062, C-08).
    yolo_warned: bool,
}

/// Escopo de um run. Enquanto existir, as concessões do run valem para os aprovadores ligados a
/// ele. Ao ser descartado (qualquer desfecho do run) o conjunto é removido; nunca é persistido.
pub struct RunScopeGuard {
    registry: ApprovalRegistry,
    id: u64,
}

impl Drop for RunScopeGuard {
    fn drop(&mut self) {
        self.registry.end_run(self.id);
    }
}

/// Registro compartilhado de pedidos pendentes. Clones compartilham o mesmo mapa.
#[derive(Clone, Default)]
pub struct ApprovalRegistry {
    inner: Arc<Mutex<HashMap<String, Pending>>>,
    /// Escopos de run ativos, por identificador de run.
    runs: Arc<Mutex<HashMap<u64, RunScope>>>,
    /// Próximo identificador de run (compartilhado entre clones).
    next_run: Arc<AtomicU64>,
    /// Próximo número de criação (compartilhado entre clones).
    next_seq: Arc<AtomicU64>,
}

impl std::fmt::Debug for ApprovalRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalRegistry").finish_non_exhaustive()
    }
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Pending>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn runs_lock(&self) -> MutexGuard<'_, HashMap<u64, RunScope>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Abre o escopo de um run do projeto. `test_command` (do settings no início do run) entra
    /// como permitido em modo exato para Developer e Reviewer. O escopo some com o último `Arc`.
    pub fn begin_run(&self, project_id: &str, test_command: Option<&str>) -> Arc<RunScopeGuard> {
        let id = self.next_run.fetch_add(1, Ordering::Relaxed);
        let scope = RunScope {
            project_id: project_id.to_string(),
            granted: HashSet::new(),
            test_command: test_command
                .map(grant_key)
                .filter(|command| !command.is_empty()),
            yolo_warned: false,
        };
        self.runs_lock().insert(id, scope);
        Arc::new(RunScopeGuard {
            registry: self.clone(),
            id,
        })
    }

    /// `true` na primeira chamada de cada run (marca o aviso de settings inválido como emitido).
    fn first_yolo_warning(&self, id: u64) -> bool {
        match self.runs_lock().get_mut(&id) {
            Some(scope) if !scope.yolo_warned => {
                scope.yolo_warned = true;
                true
            }
            _ => false,
        }
    }

    fn end_run(&self, id: u64) {
        self.runs_lock().remove(&id);
    }

    /// Decisão já concedida sem perguntar: `test_command` exato (fora do Arquiteto) ou concessão
    /// `run` do mesmo run e projeto. `None` quando é preciso pedir ao humano.
    fn granted_decision(
        &self,
        project_id: &str,
        run: u64,
        role: Role,
        command: &str,
    ) -> Option<ApprovalDecision> {
        let key = grant_key(command);
        let runs = self.runs_lock();
        let scope = runs.get(&run)?;
        if scope.project_id != project_id {
            return None;
        }
        if role != Role::Architect && scope.test_command.as_deref() == Some(key.as_str()) {
            return Some(ApprovalDecision::Once);
        }
        scope.granted.contains(&key).then_some(ApprovalDecision::Run)
    }

    /// `true` se o run ainda está aberto para o projeto.
    fn run_alive(&self, project_id: &str, run: u64) -> bool {
        self.runs_lock()
            .get(&run)
            .is_some_and(|scope| scope.project_id == project_id)
    }

    /// Acrescenta a concessão `run` ao escopo. `false` se o run já terminou.
    fn add_grant(&self, project_id: &str, run: u64, key: &str) -> bool {
        let mut runs = self.runs_lock();
        match runs.get_mut(&run) {
            Some(scope) if scope.project_id == project_id => {
                scope.granted.insert(key.to_string());
                true
            }
            _ => false,
        }
    }

    /// Remove e devolve os pedidos pendentes do mesmo projeto, run e comando (com seu id).
    fn take_siblings(&self, project_id: &str, run: u64, key: &str) -> Vec<(String, Pending)> {
        let mut map = self.lock();
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, p)| {
                p.project_id == project_id
                    && p.run == Some(run)
                    && p.grant_key.as_deref() == Some(key)
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| map.remove(&id).map(|pending| (id, pending)))
            .collect()
    }

    /// Registra um pedido (até [`MAX_PENDING_PER_PROJECT`] por projeto). O id é gerado antes do lock.
    fn register_draft(
        &self,
        draft: PendingDraft,
    ) -> Result<(String, oneshot::Receiver<ApprovalDecision>), RegisterError> {
        let request_id = random_hex_id().map_err(|_| RegisterError::Entropy)?;
        let (tx, rx) = oneshot::channel();
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let pending = Pending {
            project_id: draft.project_id.clone(),
            project_root: draft.project_root,
            tx,
            command: draft.command,
            agent: draft.agent,
            task_id: draft.task_id,
            run: draft.run,
            grant_key: draft.grant_key,
            always_allowed: draft.always_allowed,
            always_denied_reason: draft.always_denied_reason,
            environment: draft.environment,
            seq,
            created: Instant::now(),
        };
        let mut map = self.lock();
        let pending_for_project = map
            .values()
            .filter(|p| p.project_id == draft.project_id)
            .count();
        if pending_for_project >= MAX_PENDING_PER_PROJECT {
            return Err(RegisterError::Full);
        }
        map.insert(request_id.clone(), pending);
        Ok((request_id, rx))
    }

    /// Registro de teste: calcula os campos "sempre" e a chave `run` a partir do próprio comando,
    /// sem run.
    #[cfg(test)]
    fn register(
        &self,
        project_id: &str,
        project_root: &Path,
        command: String,
        agent: &'static str,
        task_id: Option<String>,
    ) -> Result<(String, oneshot::Receiver<ApprovalDecision>), RegisterError> {
        let draft = PendingDraft {
            project_id: project_id.to_string(),
            project_root: project_root.to_path_buf(),
            always_allowed: is_always_allowable(&command),
            always_denied_reason: always_denied_reason(&command),
            grant_key: Some(grant_key(&command)),
            command,
            agent,
            task_id,
            run: None,
            environment: None,
        };
        self.register_draft(draft)
    }

    /// Pedidos pendentes do projeto, em ordem de criação (no máximo [`MAX_PENDING_PER_PROJECT`]).
    /// Só copia campos sob o lock; a truncagem do comando ocorre depois de soltá-lo.
    pub fn list_pending(&self, project_id: &str) -> Vec<PendingView> {
        let mut found: Vec<(u64, PendingView)> = {
            let map = self.lock();
            map.iter()
                .filter(|(_, p)| p.project_id == project_id)
                .map(|(id, p)| {
                    let view = PendingView {
                        request_id: id.clone(),
                        agent: p.agent,
                        command: p.command.clone(),
                        task_id: p.task_id.clone(),
                        always_allowed: p.always_allowed,
                        always_denied_reason: p.always_denied_reason,
                        kind: p.environment.as_ref().map(|_| ENV_KIND),
                        reason: p.environment.as_ref().map(|e| e.reason.clone()),
                        diff: p.environment.as_ref().map(|e| e.diff.clone()),
                    };
                    (p.seq, view)
                })
                .collect()
        };
        found.sort_by_key(|(seq, _)| *seq);
        found
            .into_iter()
            .take(MAX_PENDING_PER_PROJECT)
            .map(|(_, mut view)| {
                view.command = truncate_command(&view.command);
                view
            })
            .collect()
    }

    /// Limpeza de runtime de um projeto removido do Nants. Os pedidos pendentes são resolvidos
    /// como negados (libera o agente que aguarda, com `permission_resolved` "deny", em ordem de
    /// criação) e os escopos `run` do projeto são descartados. Não toca em disco.
    pub fn clear_project(&self, project_id: &str, events: &EventBus) {
        let mut taken: Vec<(u64, String, Pending)> = {
            let mut map = self.lock();
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, p)| p.project_id == project_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| map.remove(&id).map(|p| (p.seq, id, p)))
                .collect()
        };
        taken.sort_by_key(|(seq, _, _)| *seq);
        for (_, request_id, pending) in taken {
            // Quem aguarda recebe a negação; se o agente já desistiu, o envio falha sem efeito.
            let _ = pending.tx.send(ApprovalDecision::Deny);
            publish_resolved(events, project_id, &request_id, ApprovalDecision::Deny);
        }
        self.runs_lock().retain(|_, scope| scope.project_id != project_id);
    }

    /// Remove o pedido (cancelamento ou timeout). `true` se ele ainda existia.
    fn remove(&self, request_id: &str) -> bool {
        let removed = self.lock().remove(request_id);
        removed.is_some()
    }

    /// Remove e devolve o pedido somente se ele pertencer a `project_id`.
    fn take(&self, project_id: &str, request_id: &str) -> Option<Pending> {
        let mut map = self.lock();
        match map.get(request_id) {
            Some(pending) if pending.project_id == project_id => map.remove(request_id),
            _ => None,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }

    #[cfg(test)]
    fn run_scope_count(&self) -> usize {
        self.runs_lock().len()
    }

    #[cfg(test)]
    fn created_at(&self, request_id: &str) -> Option<Instant> {
        self.lock().get(request_id).map(|p| p.created)
    }
}

/// Remove o pedido do registro quando o future de aprovação termina por qualquer caminho.
struct PendingGuard {
    registry: ApprovalRegistry,
    request_id: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let _ = self.registry.remove(&self.request_id);
    }
}

/// Aprovador de um projeto: implementa [`Approver`] sobre o [`ApprovalRegistry`].
#[derive(Clone)]
pub struct ProjectApprover {
    project_id: String,
    project_root: PathBuf,
    registry: ApprovalRegistry,
    events: EventBus,
    secrets: Arc<SecretSet>,
    timeout: Duration,
    /// Run ligado a este aprovador (concessões `run` e `test_command`); `None` fora de um run.
    run: Option<Arc<RunScopeGuard>>,
    /// Destino da linha de diálogo das aprovações automáticas (YOLO); sem ele só há o evento.
    dialog: Option<DialogTarget>,
    /// Juiz rápido do YOLO no host (spec 062); sem ele, o que as checagens não decidem vai ao humano.
    judge: Option<Arc<dyn CommandJudge>>,
    /// Vereditos do juiz por comando, válidos durante o run (o aprovador vive um run).
    judge_cache: Arc<Mutex<HashMap<String, JudgeVerdict>>>,
    /// Parada pedida pelo usuário na execução deste aprovador (spec 087 T06): vira `true` e libera
    /// os pedidos pendentes como negados. `None` fora de uma execução (chat, relatório...).
    stop: Option<watch::Receiver<bool>>,
}

/// Pedido genérico de alteração (ambiente ou configuração de testes) para [`ProjectApprover::decide_change`].
struct ChangeRequest<'a> {
    role: Role,
    task_id: Option<&'a str>,
    reason: &'a str,
    command: String,
    auto_detail: String,
    auto_kind: &'static str,
    diff: String,
    /// `false` desliga o atalho YOLO (o pedido segue para o humano).
    auto_allowed: bool,
}

/// `kind` da linha de aprovação automática (YOLO) da correção de configuração de testes.
pub const CONFIG_KIND: &str = "config";
const CONFIG_COMMAND: &str = "Alterar o comando de testes do projeto (test_command/checks)";

/// `kind` da aprovação automática (YOLO) da tarefa de correção da verificação final.
pub const FINAL_FIX_KIND: &str = "final_fix";
/// Texto fixo do pedido da correção final (o `/api/attention` o reconhece por ele).
pub const FINAL_FIX_COMMAND: &str = "Criar tarefa de correção dos critérios globais não atendidos";

impl crate::agents::runner::FinalGate for ProjectApprover {
    fn approve_fix<'a>(&'a self, _spec_id: &'a str, reason: &'a str) -> crate::agents::runner::Fut<'a, bool> {
        Box::pin(self.approve_final_fix(reason))
    }
}

/// Prefixo fixo da mensagem de aprovação automática (evento e linha de diálogo).
pub const YOLO_AUTO_PREFIX: &str = "aprovada automaticamente";
/// Código (spec 102) do aviso de aprovação automática ignorada; a interface o traduz.
pub const CODE_YOLO_IGNORED: &str = "run.yolo_ignored";
/// Linha de sistema (uma por run) quando a aprovação automática foi ignorada por `.sdd/settings.yaml` inválido.
pub const YOLO_IGNORED_TEXT: &str =
    "Aprovação automática ignorada: .sdd/settings.yaml está inválido e não pôde ser lido; os pedidos de permissão vão para você até corrigir o arquivo.";

impl ProjectApprover {
    pub fn new(
        project_id: impl Into<String>,
        project_root: PathBuf,
        registry: ApprovalRegistry,
        events: EventBus,
        secrets: Arc<SecretSet>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            project_root,
            registry,
            events,
            secrets,
            timeout: APPROVAL_TIMEOUT,
            run: None,
            dialog: None,
            judge: None,
            judge_cache: Arc::new(Mutex::new(HashMap::new())),
            stop: None,
        }
    }

    /// Liga a parada da execução (spec 087 T06): com ela pedida, cada pedido pendente deste
    /// aprovador é negado com [`STOPPED_REASON`], sem gravar regra nem concessão.
    pub fn with_stop(mut self, stop: watch::Receiver<bool>) -> Self {
        self.stop = Some(stop);
        self
    }

    /// Espera a decisão do pedido `request_id`: resposta humana, timeout (nega) ou parada da
    /// execução. Na parada, o pedido ainda pendente sai do registro e o `permission_resolved`
    /// "deny" é publicado com o motivo fixo; nada é persistido (o caminho do "sempre" é só a rota).
    async fn await_decision(&self, request_id: &str, rx: oneshot::Receiver<ApprovalDecision>) -> ApprovalDecision {
        tokio::select! {
            res = tokio::time::timeout(self.timeout, rx) => match res {
                Ok(Ok(decision)) => decision,
                Ok(Err(_)) | Err(_) => ApprovalDecision::Deny,
            },
            () = wait_stop(self.stop.clone()) => {
                // Se o humano resolveu no mesmo instante, o pedido já saiu e a resolução dele vale
                // para o evento; a sessão é cancelada de qualquer forma.
                if self.registry.take(&self.project_id, request_id).is_some() {
                    publish_stopped(&self.events, &self.project_id, request_id);
                }
                ApprovalDecision::Deny
            }
        }
    }

    /// Liga o juiz do YOLO (modelo do Arquiteto em produção; fake nos testes).
    pub fn with_judge(mut self, judge: Arc<dyn CommandJudge>) -> Self {
        self.judge = Some(judge);
        self
    }

    /// Veredito do juiz para `command` (com cache por comando); `None` sem juiz, erro ou JSON
    /// inválido (⇒ humano).
    async fn judge_verdict(&self, command: &str, why: &str) -> Option<JudgeVerdict> {
        let judge = self.judge.as_ref()?;
        if let Some(hit) = self.judge_cache.lock().unwrap_or_else(PoisonError::into_inner).get(command) {
            return Some(hit.clone());
        }
        let verdict = judge.judge(command, &self.project_root, why).await.ok()?;
        self.judge_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(command.to_string(), verdict.clone());
        Some(verdict)
    }

    /// Grava as aprovações automáticas (YOLO) no diálogo da spec, para reidratar o chat.
    pub fn with_dialog(mut self, dialog: DialogTarget) -> Self {
        self.dialog = Some(dialog);
        self
    }

    /// Configurações do projeto quando o modo YOLO está ligado; `None` desligado, ausente ou ilegível.
    async fn yolo_settings(&self) -> Option<Settings> {
        let root = self.project_root.clone();
        tokio::task::spawn_blocking(move || {
            let text = safe_fs::read_regular(&root, SETTINGS_REL, MAX_READ_BYTES).ok()?;
            Settings::parse(&text).ok().filter(|s| s.yolo)
        })
        .await
        .ok()
        .flatten()
    }

    /// Settings ilegíveis com `yolo: true` escrito: o pedido vai ao humano e o YOLO foi ignorado.
    /// Uma linha de sistema por run explica o motivo (spec 062, C-08).
    async fn warn_invalid_settings(&self) {
        let Some(run) = self.run.as_ref().map(|r| r.id) else {
            return;
        };
        let root = self.project_root.clone();
        let invalid_yolo = tokio::task::spawn_blocking(move || {
            let text = safe_fs::read_regular(&root, SETTINGS_REL, MAX_READ_BYTES).ok()?;
            let wants_yolo = text.lines().any(|l| l.trim().replace(' ', "") == "yolo:true");
            (wants_yolo && Settings::parse(&text).is_err()).then_some(())
        })
        .await
        .ok()
        .flatten()
        .is_some();
        if !invalid_yolo || !self.registry.first_yolo_warning(run) {
            return;
        }
        deliver(
            &self.events,
            self.dialog.as_ref(),
            ApiEvent::new(
                EventType::AgentMessage,
                Some(self.project_id.clone()),
                json!({
                    "agent": "system",
                    "text": YOLO_IGNORED_TEXT,
                    "code": CODE_YOLO_IGNORED,
                    "params": {},
                    "task_id": Value::Null,
                    "attempt": Value::Null,
                    "seq": Value::Null,
                }),
            ),
        );
    }

    /// Configurações atuais do projeto (relidas do disco); `None` ausente ou ilegível.
    async fn current_settings(&self) -> Option<Settings> {
        let root = self.project_root.clone();
        tokio::task::spawn_blocking(move || {
            let text = safe_fs::read_regular(&root, SETTINGS_REL, MAX_READ_BYTES).ok()?;
            Settings::parse(&text).ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// Evento SSE `agent_message` (`yolo_auto: true`, `kind`: `command`|`environment`) e linha de
    /// diálogo "aprovada automaticamente: ..." com o pedido já redigido.
    fn announce_auto(&self, role: Role, task_id: Option<&str>, kind: &str, detail: &str) {
        let detail = truncate_command(&scrub_text(&self.secrets, detail));
        deliver(
            &self.events,
            self.dialog.as_ref(),
            ApiEvent::new(
                EventType::AgentMessage,
                Some(self.project_id.clone()),
                json!({
                    "agent": role_name(role),
                    "text": format!("{YOLO_AUTO_PREFIX}: {detail}"),
                    "task_id": task_id.map(|t| scrub_text(&self.secrets, t)),
                    "attempt": Value::Null,
                    "seq": Value::Null,
                    "yolo_auto": true,
                    "kind": kind,
                }),
            ),
        );
    }

    /// Linha de diálogo (sem `yolo_auto`) com o motivo de o comando ter ido ao humano.
    fn announce_judge_refusal(&self, role: Role, task_id: Option<&str>, command: &str, reason: &str) {
        let text = truncate_command(&scrub_text(
            &self.secrets,
            &format!("A aprovação automática não liberou sozinha ({reason}); aguardando você: {command}"),
        ));
        deliver(
            &self.events,
            self.dialog.as_ref(),
            ApiEvent::new(
                EventType::AgentMessage,
                Some(self.project_id.clone()),
                json!({
                    "agent": role_name(role),
                    "text": text,
                    "task_id": task_id.map(|t| scrub_text(&self.secrets, t)),
                    "attempt": Value::Null,
                    "seq": Value::Null,
                }),
            ),
        );
    }

    /// Define o tempo máximo de espera (padrão: [`APPROVAL_TIMEOUT`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Liga o aprovador a um run: concessões `run` e `test_command` passam a valer para ele.
    pub fn with_run(mut self, run: Arc<RunScopeGuard>) -> Self {
        self.registry = run.registry.clone();
        self.run = Some(run);
        self
    }

    /// Pedido `environment`: motivo e diff redigidos/limitados; sem concessão `run`/`always`.
    async fn decide_environment(&self, req: &EnvChangeRequest) -> ApprovalDecision {
        let paths: Vec<&str> = req.files.iter().map(|f| f.path.as_str()).collect();
        let diff = req.files.iter().map(|f| f.diff.as_str()).collect::<Vec<_>>().join("\n");
        let change = ChangeRequest {
            role: req.role,
            task_id: req.task_id.as_deref(),
            reason: &req.reason,
            command: format!("Alterar o ambiente Docker: {}", paths.join(", ")),
            auto_detail: format!("alterar o ambiente Docker ({}). Motivo: {}", paths.join(", "), req.reason),
            auto_kind: ENV_KIND,
            diff,
            auto_allowed: true,
        };
        self.decide_change(&change).await
    }

    /// Pedido HITL de correção de `test_command`/`checks` (spec 039, REVISE_CONFIG). Reaproveita o
    /// pedido `environment` da UI (motivo + diff antes/depois, só `once`); YOLO aprova sozinho com a
    /// linha "aprovada automaticamente" (`kind: "config"`).
    pub async fn approve_config_change(
        &self,
        task_id: Option<&str>,
        reason: &str,
        diff: &str,
        commands: &[String],
    ) -> ApprovalDecision {
        // YOLO só auto-aprova se CADA segmento (`&&`) de cada comando passa na exclusão do YOLO.
        // Em projeto docker os comandos rodam no container: vale a regra do container (spec 062).
        let in_container = self.yolo_settings().await.is_some_and(|s| s.environment == Environment::Docker);
        let auto_allowed = commands.iter().flat_map(|c| c.split("&&")).all(|seg| {
            let seg = seg.trim();
            if in_container {
                yolo_container_excluded_reason(seg).is_none()
            } else {
                yolo_host_verdict(seg, &self.project_root) == YoloHostVerdict::Auto
            }
        });
        let change = ChangeRequest {
            role: Role::Architect,
            task_id,
            reason,
            command: CONFIG_COMMAND.to_string(),
            auto_detail: format!("{CONFIG_COMMAND}. Motivo: {reason}"),
            auto_kind: CONFIG_KIND,
            diff: diff.to_string(),
            auto_allowed,
        };
        self.decide_change(&change).await
    }

    /// Pedido HITL da tarefa de correção da verificação final (spec 059/T02): reaproveita o
    /// pedido `environment` da UI; YOLO aprova sozinho (`kind: "final_fix"`). `true` = criar.
    pub async fn approve_final_fix(&self, reason: &str) -> bool {
        let change = ChangeRequest {
            role: Role::Architect,
            task_id: None,
            reason,
            command: FINAL_FIX_COMMAND.to_string(),
            auto_detail: format!("{FINAL_FIX_COMMAND}. Motivo: {reason}"),
            auto_kind: FINAL_FIX_KIND,
            diff: String::new(),
            auto_allowed: true,
        };
        self.decide_change(&change).await != ApprovalDecision::Deny
    }

    async fn decide_change(&self, req: &ChangeRequest<'_>) -> ApprovalDecision {
        if req.auto_allowed {
            if self.yolo_settings().await.is_some() {
                self.announce_auto(req.role, req.task_id, req.auto_kind, &req.auto_detail);
                return ApprovalDecision::Once;
            }
            self.warn_invalid_settings().await;
        }
        let run = self.run.as_ref().map(|r| r.id);
        let clip = |t: &str, max: usize| {
            let t = scrub_text(&self.secrets, t);
            if t.len() <= max {
                return t;
            }
            let mut end = max;
            while !t.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}{TRUNCATED_SUFFIX}", &t[..end])
        };
        let command = clip(&req.command, MAX_EVENT_COMMAND_BYTES);
        let payload = EnvPayload { reason: clip(req.reason, MAX_ENV_REASON_BYTES), diff: clip(&req.diff, MAX_ENV_DIFF_BYTES) };
        let task_id = req.task_id.map(|t| scrub_text(&self.secrets, t));
        let agent = role_name(req.role);
        let Ok((request_id, rx)) = self.registry.register_draft(PendingDraft {
            project_id: self.project_id.clone(),
            project_root: self.project_root.clone(),
            command: command.clone(),
            agent,
            task_id: task_id.clone(),
            run,
            grant_key: None,
            always_allowed: false,
            always_denied_reason: None,
            environment: Some(payload.clone()),
        }) else {
            return ApprovalDecision::Deny;
        };
        let _guard = PendingGuard { registry: self.registry.clone(), request_id: request_id.clone() };
        self.events.publish(ApiEvent::new(
            EventType::PermissionRequested,
            Some(self.project_id.clone()),
            json!({
                "request_id": request_id,
                "project_id": self.project_id,
                "task_id": task_id,
                "agent": agent,
                "command": command,
                "kind": ENV_KIND,
                "reason": payload.reason,
                "diff": payload.diff,
                "always_allowed": false,
                "always_denied_reason": null,
            }),
        ));
        self.await_decision(&request_id, rx).await
    }

    async fn decide(&self, req: &ApprovalRequest) -> ApprovalDecision {
        let run = self.run.as_ref().map(|r| r.id);
        if let Some(run) = run {
            if let Some(decision) =
                self.registry
                    .granted_decision(&self.project_id, run, req.role, &req.command)
            {
                return decision;
            }
        }
        // Política ATUAL (o "sempre" persistido durante o run/turno vale já): allowlist aprova em
        // silêncio; a denylist atual vence.
        let current = self.current_settings().await;
        let is_docker = current.as_ref().is_some_and(|s| s.environment == Environment::Docker);
        if let Some(settings) = &current {
            // `Allow` já exclui qualquer padrão da denylist atual.
            if evaluate(&req.command, &Policy::from_settings(settings)) == Decision::Allow {
                return ApprovalDecision::Once;
            }
        }
        if current.is_none() {
            self.warn_invalid_settings().await;
        }
        // YOLO: aprova sozinho, exceto exclusões duras; a denylist do projeto sempre vence.
        if let Some(settings) = current.filter(|s| s.yolo) {
            let denied = denylist_hit(&req.command, &Policy::from_settings(&settings));
            if !denied {
                if req.in_container {
                    if yolo_container_excluded_reason(&req.command).is_none() {
                        self.announce_auto(req.role, req.task_id.as_deref(), "command", &req.command);
                        return ApprovalDecision::Once;
                    }
                } else {
                    match yolo_host_verdict(&req.command, &self.project_root) {
                        YoloHostVerdict::Auto => {
                            self.announce_auto(req.role, req.task_id.as_deref(), "command", &req.command);
                            return ApprovalDecision::Once;
                        }
                        YoloHostVerdict::NeedsJudge(why) => {
                            // Seguro ⇒ aprova com o motivo; inseguro/erro ⇒ humano.
                            match self.judge_verdict(&req.command, &why).await {
                                Some(v) if v.safe => {
                                    let detail = format!("{} (juiz: {})", req.command, v.reason);
                                    self.announce_auto(req.role, req.task_id.as_deref(), "command", &detail);
                                    return ApprovalDecision::Once;
                                }
                                Some(v) => self.announce_judge_refusal(req.role, req.task_id.as_deref(), &req.command, &v.reason),
                                None => self.announce_judge_refusal(req.role, req.task_id.as_deref(), &req.command, &why),
                            }
                        }
                        YoloHostVerdict::Excluded(_) => {}
                    }
                }
            }
        }
        // Campos "sempre" calculados do comando completo, antes da redação.
        // "Sempre" só vale para o comando exatamente como foi pedido: com segredo redigido, o
        // comando guardado/persistido seria outro, então não é liberável permanentemente.
        let command = scrub_text(&self.secrets, &req.command);
        let redacted = command != req.command;
        let docker_block = is_docker && docker_command::invokes_container_tool(&req.command);
        let always_allowed = !redacted && !docker_block && is_always_allowable(&req.command);
        let denied_reason = if redacted {
            Some(ALWAYS_REASON_SECRET)
        } else if docker_block {
            Some(ALWAYS_REASON_DOCKER)
        } else {
            always_denied_reason(&req.command)
        };
        // Sem segredo, a chave estrutural vira concessão `run`; com segredo, nunca é guardada.
        let key = (!redacted).then(|| grant_key(&req.command));
        let task_id = req
            .task_id
            .as_deref()
            .map(|t| scrub_text(&self.secrets, t));
        let agent = role_name(req.role);
        let Ok((request_id, rx)) = self.registry.register_draft(PendingDraft {
            project_id: self.project_id.clone(),
            project_root: self.project_root.clone(),
            command: command.clone(),
            agent,
            task_id: task_id.clone(),
            run,
            grant_key: key,
            always_allowed,
            always_denied_reason: denied_reason,
            environment: None,
        }) else {
            // Teto por projeto atingido ou falha de entropia: negação imediata, sem evento.
            return ApprovalDecision::Deny;
        };
        let _guard = PendingGuard {
            registry: self.registry.clone(),
            request_id: request_id.clone(),
        };
        self.events.publish(ApiEvent::new(
            EventType::PermissionRequested,
            Some(self.project_id.clone()),
            json!({
                "request_id": request_id,
                "project_id": self.project_id,
                "task_id": task_id,
                "agent": agent,
                "command": truncate_command(&command),
                "always_allowed": always_allowed,
                "always_denied_reason": denied_reason,
            }),
        ));
        self.await_decision(&request_id, rx).await
    }
}

impl Approver for ProjectApprover {
    fn approve<'a>(
        &'a self,
        req: &'a ApprovalRequest,
    ) -> Pin<Box<dyn Future<Output = ApprovalDecision> + Send + 'a>> {
        Box::pin(async move { self.decide(req).await })
    }

    fn approve_environment<'a>(
        &'a self,
        req: &'a EnvChangeRequest,
    ) -> Pin<Box<dyn Future<Output = ApprovalDecision> + Send + 'a>> {
        Box::pin(async move { self.decide_environment(req).await })
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Architect => "architect",
        Role::Developer => "developer",
        Role::Reviewer => "reviewer",
    }
}

/// Limita o comando publicado a [`MAX_EVENT_COMMAND_BYTES`] em fronteira de caractere.
fn truncate_command(text: &str) -> String {
    if text.len() <= MAX_EVENT_COMMAND_BYTES {
        return text.to_string();
    }
    let mut end = MAX_EVENT_COMMAND_BYTES - TRUNCATED_SUFFIX.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED_SUFFIX}", &text[..end])
}

/// 128 bits aleatórios de `/dev/urandom` em hex minúsculo (32 caracteres).
/// Usado por `request_id` e pelo `turn_id` dos turnos de chat.
pub fn random_hex_id() -> io::Result<String> {
    random_hex(REQUEST_ID_BYTES)
}

fn random_hex(len: usize) -> io::Result<String> {
    let mut bytes = vec![0u8; len];
    File::open(RANDOM_DEVICE)?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `request_id` válido: exatamente 32 caracteres hex minúsculos.
pub fn validate_request_id(id: &str) -> Result<(), ApiError> {
    let valid = id.len() == REQUEST_ID_LEN
        && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if valid {
        Ok(())
    } else {
        Err(ApiError::unprocessable(
            "request_id deve ter 32 caracteres hexadecimais minúsculos",
        ))
    }
}

fn parse_decision(body: &[u8]) -> Result<ApprovalDecision, ApiError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "corpo JSON malformado"))?;
    match value.get("decision").and_then(Value::as_str) {
        Some("once") => Ok(ApprovalDecision::Once),
        Some("run") => Ok(ApprovalDecision::Run),
        Some("always") => Ok(ApprovalDecision::Always),
        Some("deny") => Ok(ApprovalDecision::Deny),
        _ => Err(ApiError::unprocessable(
            "decision deve ser \"once\", \"run\", \"always\" ou \"deny\"",
        )),
    }
}

/// Nome do escopo aplicado, como exposto em `applied` e em `permission_resolved`.
fn scope_name(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Once => "once",
        ApprovalDecision::Run => "run",
        ApprovalDecision::Always => "always",
        ApprovalDecision::Deny => "deny",
    }
}

/// Publica a resolução de um pedido com o escopo realmente aplicado.
fn publish_resolved(events: &EventBus, project_id: &str, request_id: &str, applied: ApprovalDecision) {
    events.publish(ApiEvent::new(
        EventType::PermissionResolved,
        Some(project_id.to_string()),
        json!({
            "request_id": request_id,
            "project_id": project_id,
            "applied": scope_name(applied),
        }),
    ));
}

/// Resolução por parada da execução (spec 087 T06): "deny" com o motivo fixo [`STOPPED_REASON`].
fn publish_stopped(events: &EventBus, project_id: &str, request_id: &str) {
    events.publish(ApiEvent::new(
        EventType::PermissionResolved,
        Some(project_id.to_string()),
        json!({
            "request_id": request_id,
            "project_id": project_id,
            "applied": scope_name(ApprovalDecision::Deny),
            "reason": STOPPED_REASON,
        }),
    ));
}

/// Espera a parada pedida pelo usuário (`true` no canal). Sem canal, ou com o emissor descartado
/// antes da parada, nunca termina.
pub(super) async fn wait_stop(stop: Option<watch::Receiver<bool>>) {
    if let Some(mut rx) = stop {
        if rx.wait_for(|stopped| *stopped).await.is_ok() {
            return;
        }
    }
    std::future::pending::<()>().await
}

fn pending_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, PENDING_NOT_FOUND)
}

/// Persiste `command` em `allowed_commands` do projeto. `false` se não for seguro ou falhar.
pub(super) fn persist_always(root: &Path, command: &str) -> bool {
    if !is_always_allowable(command) {
        return false;
    }
    let pattern = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let Ok(text) = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES) else {
        return false;
    };
    let Ok(mut settings) = Settings::parse(&text) else {
        return false;
    };
    if settings.environment == Environment::Docker && docker_command::invokes_container_tool(command) {
        return false;
    }
    if settings
        .allowed_commands
        .iter()
        .any(|p| p.split_whitespace().collect::<Vec<_>>().join(" ") == pattern)
    {
        return true;
    }
    settings.allowed_commands.push(pattern);
    let Ok(rendered) = settings.render() else {
        return false;
    };
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).is_ok()
}

/// Substitui `rel` por `contents` via arquivo temporário criado com `create_new` no mesmo
/// diretório e `rename`. Mantém o modo do arquivo existente. Nunca segue symlink.
pub(super) fn replace_atomic(root: &Path, rel: &str, contents: &[u8]) -> io::Result<()> {
    safe_fs::check_chain(root, rel).map_err(|_| io::Error::other("caminho inseguro"))?;
    let target = root.join(rel);
    let meta = fs::symlink_metadata(&target)?;
    if !meta.file_type().is_file() {
        return Err(io::Error::other("destino não é arquivo regular"));
    }
    let mode = meta.permissions().mode() & 0o777;
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::other("destino sem diretório"))?;
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::other("nome de destino inválido"))?;
    let tmp = dir.join(format!(".{file_name}.{}.tmp", random_hex(8)?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW)
        .open(&tmp)?;
    let result = (|| {
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(&tmp, Permissions::from_mode(mode))?;
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Confirma que o projeto está configurado. Só o lock de leitura é segurado (sem I/O).
/// `POST /api/projects/:id/permissions/:req_id` com corpo `{"decision":"once"|"always"|"deny"}`.
pub async fn resolve_permission(
    State(state): State<AppState>,
    AxPath((id, req_id)): AxPath<(String, String)>,
    body: Bytes,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    validate_request_id(&req_id)?;
    state.pause(PAUSE_BEFORE_PERMISSION).await;
    // Configuração sob leitura até o fim da resolução (take, persistência e evento). A remoção
    // pede o write lock, então não se intercala: depois dela o projeto não existe e nada é gravado
    // ou publicado. Ordem de locks: config → approvals (a mesma de `remove_project`).
    let config = state.config().read().await;
    if !config.projects.iter().any(|p| p.id == id) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, PROJECT_NOT_FOUND));
    }
    state.pause(PAUSE_AFTER_PERMISSION_LOOKUP).await;
    let decision = parse_decision(&body)?;
    let registry = state.approvals().clone();
    let pending = registry
        .take(&id, &req_id)
        .ok_or_else(pending_not_found)?;
    if pending.tx.is_closed() {
        return Err(pending_not_found());
    }
    // `run` só é possível com run aberto e comando sem segredo; senão o escopo real é `once`.
    let grant = match (pending.run, pending.grant_key.as_deref()) {
        (Some(run), Some(key)) if registry.run_alive(&id, run) => Some((run, key.to_string())),
        _ => None,
    };
    let mut persisted = None;
    // Mudança de ambiente: qualquer permissão vale só para este pedido (nunca `run`/`always`).
    let decision = match decision {
        ApprovalDecision::Deny => ApprovalDecision::Deny,
        _ if pending.environment.is_some() => ApprovalDecision::Once,
        other => other,
    };
    let applied = match decision {
        ApprovalDecision::Always => {
            let root = pending.project_root.clone();
            let command = pending.command.clone();
            let saved = tokio::task::spawn_blocking(move || persist_always(&root, &command))
                .await
                .unwrap_or(false);
            persisted = Some(saved);
            if saved {
                ApprovalDecision::Always
            } else if grant.is_some() {
                ApprovalDecision::Run
            } else {
                ApprovalDecision::Once
            }
        }
        ApprovalDecision::Run if grant.is_some() => ApprovalDecision::Run,
        ApprovalDecision::Run => ApprovalDecision::Once,
        other => other,
    };
    let request_id = req_id.clone();
    pending
        .tx
        .send(applied)
        .map_err(|_| pending_not_found())?;
    publish_resolved(state.events(), &id, &request_id, applied);
    if let (ApprovalDecision::Run, Some((run, key))) = (applied, grant) {
        // Após o envio: se o pedido já sumiu, nada é concedido.
        if registry.add_grant(&id, run, &key) {
            // Pedidos idênticos ainda pendentes no mesmo run recebem a mesma decisão.
            for (sibling_id, sibling) in registry.take_siblings(&id, run, &key) {
                if sibling.tx.send(ApprovalDecision::Run).is_ok() {
                    publish_resolved(state.events(), &id, &sibling_id, ApprovalDecision::Run);
                }
            }
        }
    }
    let applied_name = scope_name(applied);
    let view = match persisted {
        Some(saved) => json!({"ok": true, "persisted": saved, "applied": applied_name}),
        None => json!({"ok": true, "applied": applied_name}),
    };
    Ok(Json(view).into_response())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::env;
    use std::process;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;
    use crate::config::{Config, Project};
    use crate::harness::allowlist::{Policy, ALWAYS_REASON_ARBITRARY, ALWAYS_REASON_CHARS};
    use crate::harness::tools::{ApprovalRequest, Role, ToolError, ToolHost, ToolName};
    use crate::server::router;

    const HOST: &str = "127.0.0.1:3000";
    const SECRET: &str = "sk-SEGREDO-APR-1234";
    const DENY_TEXT: &str = "Command execution denied by user policy";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-approvals-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn settings_text(allowed: &[&str]) -> String {
        Settings {
            base_branch: "main".into(),
            merge_strategy: "squash".into(),
            test_command: "cargo test".into(),
            allowed_commands: allowed.iter().map(|s| s.to_string()).collect(),
            denied_commands: Vec::new(),
            checks: Vec::new(),
            formatters: Vec::new(),
            failure_artifacts: Vec::new(),
            screenshots: Vec::new(),
            environment: Default::default(),
            environment_hash: None,
            approved_environment_hashes: Vec::new(),
            max_attempts: crate::sdd::DEFAULT_LIMIT,
            max_test_cycles: crate::sdd::DEFAULT_LIMIT,
            yolo: false,
            review_diff_budget: None,
            skills: true,
            ask_architect_limit: None,
            architect_internet: false,
        }
        .render()
        .expect("render")
    }

    /// Projeto `proj` com `.sdd/settings.yaml` válido, e um segundo projeto `outro` vazio.
    struct Harness {
        state: AppState,
        root: PathBuf,
        registry: ApprovalRegistry,
        secrets: Arc<SecretSet>,
    }

    fn harness(tag: &str) -> Harness {
        let dir = temp_dir(tag);
        let root = dir.join("proj");
        fs::create_dir_all(root.join(".sdd")).expect("sdd");
        fs::write(root.join(SETTINGS_REL), settings_text(&[])).expect("settings");
        let other = dir.join("outro");
        fs::create_dir_all(&other).expect("outro");
        let mut cfg = Config::default();
        cfg.projects.push(Project {
            id: "proj".into(),
            name: "Projeto".into(),
            path: root.display().to_string(),
            created_at: "2026-10-14T00:00:00Z".into(),
        });
        cfg.projects.push(Project {
            id: "outro".into(),
            name: "Outro".into(),
            path: other.display().to_string(),
            created_at: "2026-10-14T00:00:00Z".into(),
        });
        let state = AppState::new(dir.join("config.toml"), cfg, 3000);
        let registry = state.approvals().clone();
        let secrets = Arc::new(SecretSet::new(vec![SECRET.to_string()]));
        Harness {
            state,
            root,
            registry,
            secrets,
        }
    }

    impl Harness {
        fn approver(&self, timeout: Option<Duration>) -> ProjectApprover {
            let approver = ProjectApprover::new(
                "proj",
                self.root.clone(),
                self.registry.clone(),
                self.state.events().clone(),
                self.secrets.clone(),
            );
            match timeout {
                Some(t) => approver.with_timeout(t),
                None => approver,
            }
        }
    }

    fn request(method: Method, uri: &str, body: &str, content_type: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, HOST);
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        builder.body(Body::from(body.to_string())).expect("requisição")
    }

    fn decide_req(project: &str, req_id: &str, decision: &str) -> Request<Body> {
        request(
            Method::POST,
            &format!("/api/projects/{project}/permissions/{req_id}"),
            &json!({ "decision": decision }).to_string(),
            Some("application/json"),
        )
    }

    async fn send(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
        let resp: Response<Body> = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("corpo");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    fn command_request(command: &str) -> ApprovalRequest {
        ApprovalRequest {
            in_container: false,
            role: Role::Developer,
            task_id: Some("T04".into()),
            command: command.into(),
        }
    }

    /// Espera o próximo `permission_requested` e devolve seu payload.
    async fn next_permission(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let ev = rx.recv().await.expect("evento");
                if ev.event_type() == "permission_requested" {
                    return ev.payload;
                }
            }
        })
        .await
        .expect("evento em tempo hábil")
    }

    /// Dispara uma aprovação em task, espera o evento e devolve (request_id, handle).
    async fn start_approval(
        h: &Harness,
        command: &str,
    ) -> (String, tokio::task::JoinHandle<ApprovalDecision>) {
        let mut rx = h.state.events().subscribe();
        let approver = h.approver(None);
        let req = command_request(command);
        let handle = tokio::spawn(async move { approver.approve(&req).await });
        let payload = next_permission(&mut rx).await;
        let id = payload["request_id"].as_str().expect("id").to_string();
        (id, handle)
    }

    async fn join(handle: tokio::task::JoinHandle<ApprovalDecision>) -> ApprovalDecision {
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("tempo")
            .expect("join")
    }

    #[tokio::test]
    async fn environment_request_publishes_kind_reason_diff_and_never_grants_always() {
        use crate::harness::tools::{EnvChangeRequest, EnvFileChange};
        let h = harness("env-kind");
        let before = fs::read(h.root.join(SETTINGS_REL)).expect("ler");
        let mut rx = h.state.events().subscribe();
        let approver = h.approver(None);
        let req = EnvChangeRequest {
            role: Role::Developer,
            task_id: None,
            reason: "preciso de curl".into(),
            files: vec![EnvFileChange { path: ".nants/Dockerfile".into(), diff: "-a\n+b\n".into() }],
        };
        let handle = tokio::spawn(async move { approver.approve_environment(&req).await });
        let payload = next_permission(&mut rx).await;
        assert_eq!(payload["kind"], "environment");
        assert_eq!(payload["reason"], "preciso de curl");
        assert_eq!(payload["diff"], "-a\n+b\n");
        assert_eq!(payload["always_allowed"], false);
        let id = payload["request_id"].as_str().expect("id").to_string();
        assert_eq!(h.registry.list_pending("proj")[0].kind, Some("environment"));
        let (status, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["applied"], "once");
        assert_eq!(join(handle).await, ApprovalDecision::Once);
        assert_eq!(fs::read(h.root.join(SETTINGS_REL)).expect("ler"), before, "settings intactos");
    }

    #[tokio::test]
    async fn environment_request_denied_by_human() {
        use crate::harness::tools::EnvChangeRequest;
        let h = harness("env-deny");
        let mut rx = h.state.events().subscribe();
        let approver = h.approver(None);
        let req = EnvChangeRequest { role: Role::Developer, task_id: None, reason: "r".into(), files: vec![] };
        let handle = tokio::spawn(async move { approver.approve_environment(&req).await });
        let id = next_permission(&mut rx).await["request_id"].as_str().expect("id").to_string();
        send(&h.state, decide_req("proj", &id, "deny")).await;
        assert_eq!(join(handle).await, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn once_resolves_waiting_approval_with_once() {
        let h = harness("once");
        let (id, handle) = start_approval(&h, "echo hi").await;
        let (status, body) = send(&h.state, decide_req("proj", &id, "once")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"ok": true, "applied": "once"}));
        assert_eq!(join(handle).await, ApprovalDecision::Once);
        assert_eq!(h.registry.len(), 0);
    }

    #[tokio::test]
    async fn deny_resolves_waiting_approval_with_deny() {
        let h = harness("deny");
        let (id, handle) = start_approval(&h, "echo hi").await;
        let (status, _) = send(&h.state, decide_req("proj", &id, "deny")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(join(handle).await, ApprovalDecision::Deny);
        assert_eq!(h.registry.len(), 0);
    }

    #[tokio::test]
    async fn always_persists_literal_command_and_is_idempotent() {
        let h = harness("always");
        let (id, handle) = start_approval(&h, "  echo   hi ").await;
        let (status, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"ok": true, "persisted": true, "applied": "always"}));
        assert_eq!(join(handle).await, ApprovalDecision::Always);

        let saved = Settings::parse(&fs::read_to_string(h.root.join(SETTINGS_REL)).expect("ler"))
            .expect("parse");
        assert_eq!(saved.allowed_commands, vec!["echo hi".to_string()]);

        // Já está na allowlist atual: nova consulta é aprovada em silêncio, sem duplicar.
        let again = h.approver(None).approve(&command_request("echo hi")).await;
        assert_eq!(again, ApprovalDecision::Once);
        let saved = Settings::parse(&fs::read_to_string(h.root.join(SETTINGS_REL)).expect("ler"))
            .expect("parse");
        assert_eq!(saved.allowed_commands, vec!["echo hi".to_string()], "não duplica");
    }

    #[tokio::test]
    async fn always_during_run_applies_to_next_requests_of_any_agent() {
        let h = harness("always-immediate");
        let run = h.registry.begin_run("proj", None);
        let (id, handle) = start_request(
            &h,
            h.approver(None).with_run(run.clone()),
            Role::Developer,
            "npm run build",
        )
        .await;
        send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(join(handle).await, ApprovalDecision::Always);
        for role in [Role::Developer, Role::Reviewer, Role::Architect] {
            let req = ApprovalRequest { role, task_id: None, command: "npm run build".into(), in_container: false };
            let d = tokio::time::timeout(
                Duration::from_secs(2),
                h.approver(None).with_run(run.clone()).approve(&req),
            )
            .await
            .expect("sem novo pedido");
            assert_eq!(d, ApprovalDecision::Once);
        }
        assert_eq!(h.registry.len(), 0);
        // Comando diferente continua pedindo.
        let other = ApprovalRequest { role: Role::Developer, task_id: None, command: "npm run lint".into(), in_container: false };
        let pending = tokio::time::timeout(
            Duration::from_millis(300),
            h.approver(None).with_run(run).approve(&other),
        )
        .await;
        assert!(pending.is_err(), "comando diferente deve aguardar o humano");
    }

    #[tokio::test]
    async fn always_with_wrapper_is_once_and_settings_untouched() {
        let h = harness("always-wrapper");
        let before = fs::read(h.root.join(SETTINGS_REL)).expect("ler");
        let (id, handle) = start_approval(&h, "sh -c x").await;
        let (status, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"ok": true, "persisted": false, "applied": "once"}));
        assert_eq!(join(handle).await, ApprovalDecision::Once);
        assert_eq!(fs::read(h.root.join(SETTINGS_REL)).expect("ler"), before);
    }

    #[tokio::test]
    async fn always_with_symlinked_settings_is_not_persisted() {
        let h = harness("always-symlink");
        let outside = h.root.parent().expect("pai").join("fora.yaml");
        let original = settings_text(&[]);
        fs::write(&outside, &original).expect("fora");
        fs::remove_file(h.root.join(SETTINGS_REL)).expect("rm");
        std::os::unix::fs::symlink(&outside, h.root.join(SETTINGS_REL)).expect("symlink");

        let (id, handle) = start_approval(&h, "echo hi").await;
        let (_, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(body, json!({"ok": true, "persisted": false, "applied": "once"}));
        assert_eq!(join(handle).await, ApprovalDecision::Once);
        assert_eq!(fs::read_to_string(&outside).expect("ler"), original);
    }

    #[tokio::test]
    async fn always_keeps_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let h = harness("always-mode");
        let path = h.root.join(SETTINGS_REL);
        fs::set_permissions(&path, Permissions::from_mode(0o640)).expect("chmod");
        let (id, handle) = start_approval(&h, "echo hi").await;
        let (_, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(body["persisted"], json!(true));
        join(handle).await;
        let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn persist_always_refuses_container_tool_in_docker_project_only() {
        let h = harness("docker-always");
        let cmd = "docker compose -f .nants/compose.yaml run --rm workspace dotnet test";
        assert!(persist_always(&h.root, cmd), "projeto host aceita");
        let docker = fs::read_to_string(h.root.join(SETTINGS_REL))
            .expect("ler")
            .replace("environment: host", "environment: docker");
        let docker = if docker.contains("environment: docker") { docker } else { format!("{docker}environment: docker\n") };
        fs::write(h.root.join(SETTINGS_REL), docker).expect("gravar");
        assert!(!persist_always(&h.root, "docker compose run --rm workspace dotnet build"));
        assert!(persist_always(&h.root, "echo alfa"));
    }

    #[tokio::test]
    async fn concurrent_always_writes_keep_every_entry() {
        for iteration in 0..20 {
            let h = harness(&format!("race-{iteration}"));
            let root = h.root.clone();
            let a = std::thread::spawn({
                let root = root.clone();
                move || persist_always(&root, "echo alfa")
            });
            let b = std::thread::spawn({
                let root = root.clone();
                move || persist_always(&root, "echo beta")
            });
            assert!(a.join().expect("thread a"));
            assert!(b.join().expect("thread b"));
            let saved = Settings::parse(&fs::read_to_string(root.join(SETTINGS_REL)).expect("ler"))
                .expect("parse");
            let mut got = saved.allowed_commands.clone();
            got.sort();
            assert_eq!(got, vec!["echo alfa".to_string(), "echo beta".to_string()], "iteração {iteration}");
        }
    }

    #[tokio::test]
    async fn pending_cap_per_project_denies_immediately_without_event() {
        let h = harness("cap");
        let mut keep = Vec::new();
        for _ in 0..MAX_PENDING_PER_PROJECT {
            keep.push(
                h.registry
                    .register("proj", &h.root, "echo".into(), "developer", None)
                    .expect("registro"),
            );
        }
        assert_eq!(h.registry.len(), MAX_PENDING_PER_PROJECT);

        let mut rx = h.state.events().subscribe();
        let approver = h.approver(None);
        let req = command_request("echo hi");
        let decision = tokio::time::timeout(Duration::from_millis(500), approver.approve(&req))
            .await
            .expect("negação imediata");
        assert_eq!(decision, ApprovalDecision::Deny);
        assert_eq!(h.registry.len(), MAX_PENDING_PER_PROJECT);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::broadcast::error::TryRecvError::Empty)));

        // Outro projeto não é afetado pelo teto.
        let other = ProjectApprover::new(
            "outro",
            h.root.clone(),
            h.registry.clone(),
            h.state.events().clone(),
            h.secrets.clone(),
        )
        .with_timeout(Duration::from_millis(20));
        assert_eq!(other.approve(&req).await, ApprovalDecision::Deny);
        assert!(next_permission(&mut rx).await["project_id"] == "outro");

        // Liberar uma vaga volta a aceitar pedidos.
        let (freed, _rx) = keep.pop().expect("pendente");
        assert!(h.registry.remove(&freed));
        let (_, handle) = start_approval(&h, "echo hi").await;
        assert_eq!(h.registry.len(), MAX_PENDING_PER_PROJECT);
        handle.abort();
    }

    #[tokio::test]
    async fn timeout_denies_and_removes_request() {
        let h = harness("timeout");
        let mut rx = h.state.events().subscribe();
        let approver = h.approver(Some(Duration::from_millis(50)));
        let req = command_request("echo hi");
        let decision = approver.approve(&req).await;
        assert_eq!(decision, ApprovalDecision::Deny);
        assert_eq!(h.registry.len(), 0);
        let payload = next_permission(&mut rx).await;
        let id = payload["request_id"].as_str().expect("id").to_string();
        let (status, _) = send(&h.state, decide_req("proj", &id, "once")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cancellation_removes_request() {
        let h = harness("cancel");
        let (id, handle) = start_approval(&h, "echo hi").await;
        assert_eq!(h.registry.len(), 1);
        assert!(h.registry.created_at(&id).is_some());
        handle.abort();
        let _ = handle.await;
        assert_eq!(h.registry.len(), 0);
        let (status, _) = send(&h.state, decide_req("proj", &id, "once")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resolving_twice_returns_404_the_second_time() {
        let h = harness("twice");
        let (id, handle) = start_approval(&h, "echo hi").await;
        let (first, _) = send(&h.state, decide_req("proj", &id, "deny")).await;
        assert_eq!(first, StatusCode::OK);
        assert_eq!(join(handle).await, ApprovalDecision::Deny);
        let (second, body) = send(&h.state, decide_req("proj", &id, "deny")).await;
        assert_eq!(second, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], PENDING_NOT_FOUND);
    }

    #[tokio::test]
    async fn request_of_another_project_is_404_and_does_not_resolve() {
        let h = harness("wrong-project");
        let (id, handle) = start_approval(&h, "echo hi").await;
        let (status, body) = send(&h.state, decide_req("outro", &id, "once")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], PENDING_NOT_FOUND);
        assert_eq!(h.registry.len(), 1, "pedido alheio não é removido");

        let (status, body) = send(&h.state, decide_req("proj", &id, "once")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(join(handle).await, ApprovalDecision::Once);
    }

    #[tokio::test]
    async fn unknown_request_and_unknown_project_are_404() {
        let h = harness("unknown");
        let unknown = "0".repeat(32);
        let (status, body) = send(&h.state, decide_req("proj", &unknown, "once")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], PENDING_NOT_FOUND);
        let (status, body) = send(&h.state, decide_req("ghost", &unknown, "once")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], PROJECT_NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_ids_are_422() {
        let h = harness("ids");
        let good = "a".repeat(32);
        let (status, _) = send(&h.state, decide_req("bad.id", &good, "once")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let bad_ids = ["XYZ".to_string(), "A".repeat(32), "g".repeat(32), "a".repeat(31)];
        for bad in &bad_ids {
            let (status, body) = send(&h.state, decide_req("proj", bad, "once")).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {body}");
        }
    }

    #[tokio::test]
    async fn invalid_decision_is_422() {
        let h = harness("decision");
        let id = "b".repeat(32);
        for body in [r#"{"decision":"maybe"}"#, r#"{}"#, r#"{"decision":1}"#] {
            let req = request(
                Method::POST,
                &format!("/api/projects/proj/permissions/{id}"),
                body,
                Some("application/json"),
            );
            let (status, _) = send(&h.state, req).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        }
        let req = request(
            Method::POST,
            &format!("/api/projects/proj/permissions/{id}"),
            "{not json",
            Some("application/json"),
        );
        let (status, _) = send(&h.state, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resolve_requires_json_content_type() {
        let h = harness("ctype");
        let id = "c".repeat(32);
        let req = request(
            Method::POST,
            &format!("/api/projects/proj/permissions/{id}"),
            r#"{"decision":"once"}"#,
            None,
        );
        let (status, _) = send(&h.state, req).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn request_ids_are_random_hex_and_distinct() {
        let h = harness("ids-random");
        let mut seen = HashSet::new();
        for _ in 0..64 {
            let (id, _rx) = h
                .registry
                .register("proj", &h.root, "echo".into(), "developer", None)
                .expect("registro");
            assert_eq!(id.len(), REQUEST_ID_LEN);
            assert!(validate_request_id(&id).is_ok(), "{id}");
            assert!(seen.insert(id), "id repetido");
        }
        assert_eq!(h.registry.len(), 64);
    }

    #[tokio::test]
    async fn permission_event_scrubs_and_truncates_command() {
        let h = harness("event");
        let mut rx = h.state.events().subscribe();
        let approver = h.approver(Some(Duration::from_millis(20)));
        let long = "a".repeat(3000);
        let command = format!("echo {SECRET} {long}");
        let req = ApprovalRequest {
            in_container: false,
            role: Role::Architect,
            task_id: Some(format!("T-{SECRET}")),
            command,
        };
        let _ = approver.approve(&req).await;
        let payload = next_permission(&mut rx).await;
        let text = payload["command"].as_str().expect("comando");
        assert!(!text.contains(SECRET), "segredo vazou: {text}");
        assert!(text.contains("echo ***"), "{text}");
        assert!(text.len() <= MAX_EVENT_COMMAND_BYTES, "{}", text.len());
        assert!(text.ends_with(TRUNCATED_SUFFIX), "{text}");
        assert_eq!(payload["agent"], "architect");
        assert_eq!(payload["project_id"], "proj");
        assert!(!payload["task_id"].as_str().unwrap_or("").contains(SECRET));
        assert_eq!(payload["request_id"].as_str().map(str::len), Some(REQUEST_ID_LEN));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let text = "é".repeat(MAX_EVENT_COMMAND_BYTES);
        let out = truncate_command(&text);
        assert!(out.len() <= MAX_EVENT_COMMAND_BYTES);
        assert!(out.ends_with(TRUNCATED_SUFFIX));
        assert_eq!(truncate_command("curto"), "curto");
    }

    #[tokio::test]
    async fn denied_approval_returns_deny_message_to_tool() {
        let h = harness("tool-deny");
        let mut rx = h.state.events().subscribe();
        let host = ToolHost::new(
            Role::Developer,
            h.root.clone(),
            Vec::new(),
            Policy::new(Vec::new(), Vec::new()),
            Arc::new(h.approver(None)),
        );
        let task = tokio::spawn(async move {
            host.call(ToolName::RunCommand, json!({"command": "echo hi"}))
                .await
        });
        let payload = next_permission(&mut rx).await;
        let id = payload["request_id"].as_str().expect("id").to_string();
        let (status, _) = send(&h.state, decide_req("proj", &id, "deny")).await;
        assert_eq!(status, StatusCode::OK);
        match tokio::time::timeout(Duration::from_secs(5), task).await {
            Ok(Ok(Err(ToolError::Denied(msg)))) => assert!(msg.starts_with(DENY_TEXT), "{msg}"),
            other => panic!("esperado Denied, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn once_through_tool_executes_command() {
        let h = harness("tool-once");
        let mut rx = h.state.events().subscribe();
        let host = ToolHost::new(
            Role::Developer,
            h.root.clone(),
            Vec::new(),
            Policy::new(Vec::new(), Vec::new()),
            Arc::new(h.approver(None)),
        );
        let task = tokio::spawn(async move {
            host.call(ToolName::RunCommand, json!({"command": "echo aprovado-ok"}))
                .await
        });
        let payload = next_permission(&mut rx).await;
        let id = payload["request_id"].as_str().expect("id").to_string();
        let (status, _) = send(&h.state, decide_req("proj", &id, "once")).await;
        assert_eq!(status, StatusCode::OK);
        let out = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("tempo")
            .expect("join")
            .expect("executa");
        assert!(out.contains("aprovado-ok"), "{out}");
        assert_eq!(h.registry.len(), 0);
    }

    #[tokio::test]
    async fn concurrent_requests_resolve_independently() {
        let h = harness("concurrent");
        let (id_a, handle_a) = start_approval(&h, "echo a").await;
        let (id_b, handle_b) = start_approval(&h, "echo b").await;
        assert_ne!(id_a, id_b);
        let (_, _) = send(&h.state, decide_req("proj", &id_b, "deny")).await;
        let (_, _) = send(&h.state, decide_req("proj", &id_a, "once")).await;
        assert_eq!(join(handle_b).await, ApprovalDecision::Deny);
        assert_eq!(join(handle_a).await, ApprovalDecision::Once);
    }

    #[test]
    fn parse_decision_accepts_only_known_values() {
        assert_eq!(parse_decision(br#"{"decision":"once"}"#).ok(), Some(ApprovalDecision::Once));
        assert_eq!(parse_decision(br#"{"decision":"always"}"#).ok(), Some(ApprovalDecision::Always));
        assert_eq!(parse_decision(br#"{"decision":"deny"}"#).ok(), Some(ApprovalDecision::Deny));
        assert!(parse_decision(br#"{"decision":"ONCE"}"#).is_err());
    }

    #[test]
    fn random_hex_has_requested_length_and_charset() {
        let id = random_hex_id().expect("id");
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn random_hex_id_is_distinct_between_calls() {
        let a = random_hex_id().expect("id");
        let b = random_hex_id().expect("id");
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn list_pending_is_empty_without_requests() {
        let h = harness("list-empty");
        assert!(h.registry.list_pending("proj").is_empty());
    }

    #[tokio::test]
    async fn list_pending_orders_by_creation_and_only_lists_the_project() {
        let h = harness("list-order");
        let (a, _ra) = h
            .registry
            .register("proj", &h.root, "echo a".into(), "developer", Some("T01".into()))
            .expect("a");
        let (_o, _ro) = h
            .registry
            .register("outro", &h.root, "echo outro".into(), "reviewer", None)
            .expect("outro");
        let (b, _rb) = h
            .registry
            .register("proj", &h.root, "echo b".into(), "architect", None)
            .expect("b");
        let list = h.registry.list_pending("proj");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].request_id, a);
        assert_eq!(list[0].command, "echo a");
        assert_eq!(list[0].agent, "developer");
        assert_eq!(list[0].task_id.as_deref(), Some("T01"));
        assert_eq!(list[1].request_id, b);
        assert_eq!(list[1].agent, "architect");
        assert_eq!(list[1].task_id, None);
        let other = h.registry.list_pending("outro");
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].command, "echo outro");
    }

    #[tokio::test]
    async fn list_pending_truncates_long_commands() {
        let h = harness("list-trunc");
        let (_id, _rx) = h
            .registry
            .register("proj", &h.root, "a".repeat(3000), "developer", None)
            .expect("registro");
        let list = h.registry.list_pending("proj");
        assert!(list[0].command.len() <= MAX_EVENT_COMMAND_BYTES);
        assert!(list[0].command.ends_with(TRUNCATED_SUFFIX));
    }

    #[tokio::test]
    async fn list_pending_is_capped_per_project() {
        let h = harness("list-cap");
        let mut keep = Vec::new();
        for _ in 0..MAX_PENDING_PER_PROJECT {
            keep.push(
                h.registry
                    .register("proj", &h.root, "echo".into(), "developer", None)
                    .expect("registro"),
            );
        }
        assert_eq!(h.registry.list_pending("proj").len(), MAX_PENDING_PER_PROJECT);
        assert!(h.registry.list_pending("proj").len() <= MAX_PENDING_PER_PROJECT);
    }

    #[tokio::test]
    async fn list_pending_shows_role_task_and_scrubbed_command_while_waiting() {
        let h = harness("list-meta");
        let (id, handle) = start_approval(&h, &format!("echo {SECRET}")).await;
        let list = h.registry.list_pending("proj");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].request_id, id);
        assert_eq!(list[0].agent, "developer");
        assert_eq!(list[0].task_id.as_deref(), Some("T04"));
        assert!(!list[0].command.contains(SECRET), "{}", list[0].command);
        assert_eq!(list[0].command, "echo ***");
        let (status, _) = send(&h.state, decide_req("proj", &id, "once")).await;
        assert_eq!(status, StatusCode::OK);
        join(handle).await;
    }

    #[tokio::test]
    async fn list_pending_drops_resolved_and_timed_out_requests() {
        let h = harness("list-gone");
        let (id, handle) = start_approval(&h, "echo hi").await;
        assert_eq!(h.registry.list_pending("proj").len(), 1);
        let (status, _) = send(&h.state, decide_req("proj", &id, "once")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(join(handle).await, ApprovalDecision::Once);
        assert!(h.registry.list_pending("proj").is_empty());

        let approver = h.approver(Some(Duration::from_millis(20)));
        assert_eq!(approver.approve(&command_request("echo t")).await, ApprovalDecision::Deny);
        assert!(h.registry.list_pending("proj").is_empty());
    }

    /// Pedido de `role` para `command` (sem task) com `approver`; devolve (request_id, handle).
    async fn start_request(
        h: &Harness,
        approver: ProjectApprover,
        role: Role,
        command: &str,
    ) -> (String, tokio::task::JoinHandle<ApprovalDecision>) {
        let mut rx = h.state.events().subscribe();
        let req = req_of(role, command);
        let handle = tokio::spawn(async move { approver.approve(&req).await });
        let payload = next_permission(&mut rx).await;
        let id = payload["request_id"].as_str().expect("id").to_string();
        (id, handle)
    }

    fn req_of(role: Role, command: &str) -> ApprovalRequest {
        ApprovalRequest {
            in_container: false,
            role,
            task_id: Some("T04".into()),
            command: command.into(),
        }
    }

    /// Decisão que precisa ser imediata (sem pedido ao humano).
    async fn approve_now(approver: &ProjectApprover, role: Role, command: &str) -> ApprovalDecision {
        tokio::time::timeout(Duration::from_secs(2), approver.approve(&req_of(role, command)))
            .await
            .expect("sem pedido: decisão imediata")
    }

    /// Conta `permission_requested` publicados e ainda não lidos.
    fn drain_requests(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>) -> usize {
        let mut count = 0;
        while let Ok(ev) = rx.try_recv() {
            if ev.event_type() == "permission_requested" {
                count += 1;
            }
        }
        count
    }

    /// Espera o próximo evento `kind` e devolve seu payload.
    async fn next_event_of(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>, kind: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let ev = rx.recv().await.expect("evento");
                if ev.event_type() == kind {
                    return ev.payload;
                }
            }
        })
        .await
        .expect("evento em tempo hábil")
    }

    /// Lista `allowed_commands` gravada em `.sdd/settings.yaml`.
    fn read_allowed(h: &Harness) -> Vec<String> {
        Settings::parse(&fs::read_to_string(h.root.join(SETTINGS_REL)).expect("ler"))
            .expect("parse")
            .allowed_commands
    }

    #[tokio::test]
    async fn run_decision_is_applied_as_run_and_not_asked_again_in_the_same_run() {
        let h = harness("run-scope");
        let run = h.registry.begin_run("proj", None);
        let approver = h.approver(None).with_run(run.clone());
        let mut rx = h.state.events().subscribe();
        let (id, handle) = start_request(&h, approver.clone(), Role::Developer, "node --test").await;
        let (status, body) = send(&h.state, decide_req("proj", &id, "run")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"ok": true, "applied": "run"}));
        assert_eq!(join(handle).await, ApprovalDecision::Run);
        for _ in 0..3 {
            assert_eq!(approve_now(&approver, Role::Developer, "node --test").await, ApprovalDecision::Run);
        }
        assert_eq!(drain_requests(&mut rx), 1, "só o primeiro pedido foi perguntado");
        assert!(h.registry.list_pending("proj").is_empty());
        assert!(read_allowed(&h).is_empty(), "run não grava em allowed_commands");
    }

    #[tokio::test]
    async fn run_grant_does_not_leak_to_other_project_or_other_run() {
        let h = harness("run-isolation");
        let run = h.registry.begin_run("proj", None);
        let (id, handle) = start_request(
            &h,
            h.approver(None).with_run(run.clone()),
            Role::Developer,
            "node --test",
        )
        .await;
        let (status, _) = send(&h.state, decide_req("proj", &id, "run")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(join(handle).await, ApprovalDecision::Run);

        let short = Duration::from_millis(50);
        let mut rx = h.state.events().subscribe();
        let other_run = h.registry.begin_run("proj", None);
        let next_run = h.approver(Some(short)).with_run(other_run);
        assert_eq!(next_run.approve(&req_of(Role::Developer, "node --test")).await, ApprovalDecision::Deny);
        assert_eq!(drain_requests(&mut rx), 1, "outro run volta a perguntar");

        let other_project = ProjectApprover::new(
            "outro",
            h.root.clone(),
            h.registry.clone(),
            h.state.events().clone(),
            h.secrets.clone(),
        )
        .with_timeout(short)
        .with_run(run.clone());
        assert_eq!(other_project.approve(&req_of(Role::Developer, "node --test")).await, ApprovalDecision::Deny);
        assert_eq!(drain_requests(&mut rx), 1, "outro projeto volta a perguntar");
    }

    #[tokio::test]
    async fn always_on_interpreter_is_applied_as_run_and_not_persisted() {
        let h = harness("always-node");
        let run = h.registry.begin_run("proj", None);
        let approver = h.approver(None).with_run(run.clone());
        let before = fs::read(h.root.join(SETTINGS_REL)).expect("ler");
        let (id, handle) = start_request(&h, approver.clone(), Role::Developer, "node --test").await;
        let (status, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"ok": true, "persisted": false, "applied": "run"}));
        assert_eq!(join(handle).await, ApprovalDecision::Run);
        assert_eq!(fs::read(h.root.join(SETTINGS_REL)).expect("ler"), before, "settings inalterado");
        assert_eq!(approve_now(&approver, Role::Developer, "node --test").await, ApprovalDecision::Run);
    }

    #[tokio::test]
    async fn always_on_interpreter_without_run_is_applied_as_once() {
        let h = harness("always-node-norun");
        let (id, handle) = start_approval(&h, "node --test").await;
        let (_, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(body, json!({"ok": true, "persisted": false, "applied": "once"}));
        assert_eq!(join(handle).await, ApprovalDecision::Once);
    }

    #[tokio::test]
    async fn always_on_plain_command_persists_and_reports_always() {
        let h = harness("always-git");
        let run = h.registry.begin_run("proj", None);
        let (id, handle) = start_request(
            &h,
            h.approver(None).with_run(run),
            Role::Developer,
            "git status --short",
        )
        .await;
        let (status, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"ok": true, "persisted": true, "applied": "always"}));
        assert_eq!(join(handle).await, ApprovalDecision::Always);
        assert_eq!(read_allowed(&h), vec!["git status --short".to_string()]);
    }

    #[tokio::test]
    async fn run_decision_without_run_scope_is_applied_as_once() {
        let h = harness("run-noscope");
        let (id, handle) = start_approval(&h, "echo hi").await;
        let (status, body) = send(&h.state, decide_req("proj", &id, "run")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"ok": true, "applied": "once"}));
        assert_eq!(join(handle).await, ApprovalDecision::Once);
    }

    #[tokio::test]
    async fn run_scope_never_overrides_denied_commands() {
        let h = harness("run-denied");
        let run = h.registry.begin_run("proj", None);
        let mut rx = h.state.events().subscribe();
        let host = ToolHost::new(
            Role::Developer,
            h.root.clone(),
            Vec::new(),
            Policy::new(Vec::new(), vec!["rm *".into()]),
            Arc::new(h.approver(Some(Duration::from_millis(50))).with_run(run)),
        );
        let result = host.call(ToolName::RunCommand, json!({"command": "rm arquivo"})).await;
        assert!(matches!(result, Err(ToolError::Denied(_))), "{result:?}");
        assert_eq!(drain_requests(&mut rx), 0, "negado não gera pedido nem liberação");
    }

    #[tokio::test]
    async fn test_command_exact_runs_without_prompt_and_variations_ask() {
        let h = harness("test-command");
        let run = h.registry.begin_run("proj", Some("  node   --test "));
        let approver = h.approver(Some(Duration::from_millis(50))).with_run(run);
        let mut rx = h.state.events().subscribe();
        assert_eq!(approve_now(&approver, Role::Developer, "node --test").await, ApprovalDecision::Once);
        assert_eq!(approve_now(&approver, Role::Reviewer, "node --test").await, ApprovalDecision::Once);
        assert_eq!(drain_requests(&mut rx), 0, "test_command exato não pede");

        let variation = approver.approve(&req_of(Role::Developer, "node --test test/x.test.js")).await;
        assert_eq!(variation, ApprovalDecision::Deny);
        assert_eq!(drain_requests(&mut rx), 1, "variação com argumento pede");

        let architect = approver.approve(&req_of(Role::Architect, "node --test")).await;
        assert_eq!(architect, ApprovalDecision::Deny);
        assert_eq!(drain_requests(&mut rx), 1, "test_command não vale para o Arquiteto");
        assert!(read_allowed(&h).is_empty(), "test_command não grava em allowed_commands");
    }

    #[tokio::test]
    async fn interpreter_without_test_command_still_asks() {
        let h = harness("no-test-command");
        let run = h.registry.begin_run("proj", None);
        let approver = h.approver(Some(Duration::from_millis(50))).with_run(run);
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver.approve(&req_of(Role::Developer, "node --test")).await, ApprovalDecision::Deny);
        assert_eq!(drain_requests(&mut rx), 1);
    }

    #[tokio::test]
    async fn pending_view_and_request_event_report_always_allowed_and_reason() {
        let h = harness("always-view");
        let mut keep = Vec::new();
        for command in ["node --test", "git status", "a && b"] {
            keep.push(
                h.registry
                    .register("proj", &h.root, command.into(), "developer", None)
                    .expect("registro"),
            );
        }
        let list = h.registry.list_pending("proj");
        assert_eq!(list.len(), 3);
        assert!(!list[0].always_allowed);
        assert_eq!(list[0].always_denied_reason, Some(ALWAYS_REASON_ARBITRARY));
        assert!(list[1].always_allowed);
        assert_eq!(list[1].always_denied_reason, None);
        assert!(!list[2].always_allowed);
        assert_eq!(list[2].always_denied_reason, Some(ALWAYS_REASON_CHARS));

        let approver = h.approver(Some(Duration::from_millis(20)));
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver.approve(&req_of(Role::Developer, "node --test")).await, ApprovalDecision::Deny);
        let payload = next_permission(&mut rx).await;
        assert_eq!(payload["always_allowed"], json!(false));
        assert_eq!(payload["always_denied_reason"], json!(ALWAYS_REASON_ARBITRARY));
    }

    #[tokio::test]
    async fn ending_the_run_clears_its_grants() {
        let h = harness("run-end");
        let run = h.registry.begin_run("proj", None);
        assert_eq!(h.registry.run_scope_count(), 1);
        let approver = h.approver(None).with_run(run.clone());
        let (id, handle) = start_request(&h, approver.clone(), Role::Developer, "node --test").await;
        let (status, _) = send(&h.state, decide_req("proj", &id, "run")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(join(handle).await, ApprovalDecision::Run);
        drop(approver);
        drop(run);
        assert_eq!(h.registry.run_scope_count(), 0, "fim do run limpa o conjunto");

        let next = h.registry.begin_run("proj", None);
        let next_approver = h.approver(Some(Duration::from_millis(50))).with_run(next);
        assert_eq!(
            next_approver.approve(&req_of(Role::Developer, "node --test")).await,
            ApprovalDecision::Deny,
            "novo run pergunta de novo"
        );
    }

    #[tokio::test]
    async fn identical_pending_requests_are_resolved_by_one_run_decision() {
        let h = harness("run-siblings");
        let run = h.registry.begin_run("proj", None);
        let approver = h.approver(None).with_run(run);
        let mut rx = h.state.events().subscribe();
        let (id_a, handle_a) = start_request(&h, approver.clone(), Role::Developer, "node --test").await;
        let (id_b, handle_b) = start_request(&h, approver.clone(), Role::Developer, "node --test").await;
        assert_ne!(id_a, id_b);
        let (status, body) = send(&h.state, decide_req("proj", &id_a, "run")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"ok": true, "applied": "run"}));
        assert_eq!(join(handle_a).await, ApprovalDecision::Run);
        // O irmão já pendente recebe a mesma decisão: sem segundo pedido para o humano.
        assert_eq!(join(handle_b).await, ApprovalDecision::Run);
        assert!(h.registry.list_pending("proj").is_empty());
        assert_eq!(approve_now(&approver, Role::Developer, "node --test").await, ApprovalDecision::Run);
        assert_eq!(drain_requests(&mut rx), 2);
    }

    #[tokio::test]
    async fn resolution_publishes_permission_resolved_with_applied_scope() {
        let h = harness("resolved-event");
        let (id, handle) = start_approval(&h, "echo hi").await;
        let mut rx = h.state.events().subscribe();
        let (status, _) = send(&h.state, decide_req("proj", &id, "run")).await;
        assert_eq!(status, StatusCode::OK);
        join(handle).await;
        let payload = next_event_of(&mut rx, "permission_resolved").await;
        assert_eq!(payload["request_id"], json!(id));
        assert_eq!(payload["applied"], json!("once"));
    }

    #[test]
    fn parse_decision_accepts_run() {
        assert_eq!(parse_decision(br#"{"decision":"run"}"#).ok(), Some(ApprovalDecision::Run));
    }

    /// Concede `run` ao pedido `command` feito com `approver` e devolve a decisão recebida.
    async fn grant_run_for(h: &Harness, approver: ProjectApprover, command: &str) -> ApprovalDecision {
        let (id, handle) = start_request(h, approver, Role::Developer, command).await;
        let (status, _) = send(&h.state, decide_req("proj", &id, "run")).await;
        assert_eq!(status, StatusCode::OK);
        join(handle).await
    }

    #[tokio::test]
    async fn run_grant_does_not_cover_different_spacing_inside_quotes() {
        let h = harness("grant-quotes");
        let run = h.registry.begin_run("proj", None);
        let short = Duration::from_millis(50);
        assert_eq!(grant_run_for(&h, h.approver(None).with_run(run.clone()), "echo \"a  b\"").await, ApprovalDecision::Run);
        assert_eq!(approve_now(&h.approver(None).with_run(run.clone()), Role::Developer, "echo \"a  b\"").await, ApprovalDecision::Run, "repetição exata não pede");
        let mut rx = h.state.events().subscribe();
        let other = h.approver(Some(short)).with_run(run.clone());
        assert_eq!(other.approve(&req_of(Role::Developer, "echo \"a b\"")).await, ApprovalDecision::Deny, "1 espaço dentro de aspas pede");
        assert_eq!(drain_requests(&mut rx), 1);
        let single = h.approver(Some(short)).with_run(run.clone());
        assert_eq!(single.approve(&req_of(Role::Developer, "echo 'a  b'")).await, ApprovalDecision::Deny, "aspas simples são outra chave");
        assert_eq!(drain_requests(&mut rx), 1);
    }

    #[tokio::test]
    async fn run_grant_collapses_spaces_outside_quotes_only() {
        let h = harness("grant-outside");
        let run = h.registry.begin_run("proj", None);
        assert_eq!(grant_run_for(&h, h.approver(None).with_run(run.clone()), "echo   a   b").await, ApprovalDecision::Run);
        assert_eq!(approve_now(&h.approver(None).with_run(run.clone()), Role::Developer, "echo a b").await, ApprovalDecision::Run);
    }

    #[tokio::test]
    async fn run_grant_with_unclosed_quote_matches_only_the_same_text() {
        let h = harness("grant-unclosed");
        let run = h.registry.begin_run("proj", None);
        assert_eq!(grant_run_for(&h, h.approver(None).with_run(run.clone()), "echo \"a  b").await, ApprovalDecision::Run);
        assert_eq!(approve_now(&h.approver(None).with_run(run.clone()), Role::Developer, "echo \"a  b").await, ApprovalDecision::Run);
        let approver = h.approver(Some(Duration::from_millis(50))).with_run(run);
        assert_eq!(approver.approve(&req_of(Role::Developer, "echo \"a b")).await, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn test_command_with_quoted_double_spaces_matches_exact_text_only() {
        let h = harness("test-command-quotes");
        let run = h.registry.begin_run("proj", Some("echo \"a  b\""));
        let approver = h.approver(Some(Duration::from_millis(50))).with_run(run);
        assert_eq!(approve_now(&approver, Role::Developer, "echo \"a  b\"").await, ApprovalDecision::Once);
        assert_eq!(approver.approve(&req_of(Role::Developer, "echo \"a b\"")).await, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn secret_command_is_never_always_allowed_nor_granted_for_run() {
        let h = harness("secret-always");
        let run = h.registry.begin_run("proj", None);
        let approver = h.approver(Some(Duration::from_millis(50))).with_run(run.clone());
        let mut rx = h.state.events().subscribe();
        let command = format!("echo {SECRET}");
        assert_eq!(approver.approve(&req_of(Role::Developer, &command)).await, ApprovalDecision::Deny);
        let payload = next_permission(&mut rx).await;
        assert_eq!(payload["always_allowed"], json!(false));
        assert_eq!(payload["always_denied_reason"], json!(ALWAYS_REASON_SECRET));
        assert_eq!(h.registry.list_pending("proj").len(), 0);

        let (id, handle) = start_request(&h, h.approver(None).with_run(run.clone()), Role::Developer, &command).await;
        let view = h.registry.list_pending("proj");
        assert_eq!(view.len(), 1);
        assert!(!view[0].always_allowed);
        assert_eq!(view[0].always_denied_reason, Some(ALWAYS_REASON_SECRET));
        let (status, body) = send(&h.state, decide_req("proj", &id, "always")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"ok": true, "persisted": false, "applied": "once"}));
        assert_eq!(join(handle).await, ApprovalDecision::Once);
        assert!(read_allowed(&h).is_empty());
    }

    #[tokio::test]
    async fn plain_command_without_secret_keeps_always_allowed() {
        let h = harness("plain-always");
        let approver = h.approver(Some(Duration::from_millis(50)));
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver.approve(&req_of(Role::Developer, "git status")).await, ApprovalDecision::Deny);
        let payload = next_permission(&mut rx).await;
        assert_eq!(payload["always_allowed"], json!(true));
        assert_eq!(payload["always_denied_reason"], Value::Null);
    }

    #[tokio::test]
    async fn docker_project_container_command_is_not_always_allowed() {
        let h = harness("docker-view");
        let text = fs::read_to_string(h.root.join(SETTINGS_REL)).expect("ler");
        fs::write(h.root.join(SETTINGS_REL), format!("{text}\nenvironment: docker\n")).expect("gravar");
        let approver = h.approver(Some(Duration::from_millis(50)));
        let mut rx = h.state.events().subscribe();
        let cmd = "docker compose run --rm workspace dotnet test";
        assert_eq!(approver.approve(&req_of(Role::Developer, cmd)).await, ApprovalDecision::Deny);
        let payload = next_permission(&mut rx).await;
        assert_eq!(payload["always_allowed"], json!(false));
        assert_eq!(payload["always_denied_reason"], json!(ALWAYS_REASON_DOCKER));
    }

    // ----- Modo YOLO (spec 037/T01) -----

    fn set_yolo(h: &Harness, on: bool, denied: &[&str]) {
        let mut text = settings_text(&[]);
        if on {
            text.push_str("yolo: true\n");
        }
        if !denied.is_empty() {
            text = text.replace(
                "denied_commands: []",
                &format!(
                    "denied_commands: [{}]",
                    denied.iter().map(|d| format!("\"{d}\"")).collect::<Vec<_>>().join(",")
                ),
            );
        }
        fs::write(h.root.join(SETTINGS_REL), text).expect("settings");
    }

    fn drain_agent_messages(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if ev.event_type() == "agent_message" {
                out.push(ev.payload);
            }
        }
        out
    }

    #[tokio::test]
    async fn yolo_auto_approves_commands_with_event_and_redaction() {
        let h = harness("yolo-ok");
        set_yolo(&h, true, &[]);
        let mut rx = h.state.events().subscribe();
        let approver = h.approver(None);
        let cmd = format!("npm install left-pad --token {SECRET}");
        assert_eq!(approve_now(&approver, Role::Developer, &cmd).await, ApprovalDecision::Once);
        assert_eq!(drain_requests_and_messages(&mut rx).0, 0, "sem pedido ao humano");
        assert_eq!(h.registry.len(), 0);
        let mut rx = h.state.events().subscribe();
        assert_eq!(approve_now(&approver, Role::Developer, "pip install requests").await, ApprovalDecision::Once);
        let msgs = drain_agent_messages(&mut rx);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["yolo_auto"], true);
        assert_eq!(msgs[0]["agent"], "developer");
        assert!(msgs[0]["text"].as_str().unwrap_or("").starts_with("aprovada automaticamente: pip install"));
        // Redação do segredo no texto publicado.
        let mut rx = h.state.events().subscribe();
        approve_now(&approver, Role::Developer, &cmd).await;
        let text = drain_agent_messages(&mut rx).remove(0)["text"].to_string();
        assert!(!text.contains(SECRET), "{text}");
    }

    fn drain_requests_and_messages(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>) -> (usize, usize) {
        let (mut r, mut m) = (0, 0);
        while let Ok(ev) = rx.try_recv() {
            match ev.event_type() {
                "permission_requested" => r += 1,
                "agent_message" => m += 1,
                _ => {}
            }
        }
        (r, m)
    }

    #[tokio::test]
    async fn yolo_hard_exclusions_still_go_to_the_human() {
        let h = harness("yolo-excl");
        set_yolo(&h, true, &[]);
        let approver = h.approver(Some(Duration::from_millis(80)));
        for cmd in [
            "sudo apt install x",
            "env sudo ls",
            "git push origin main",
            "git -C . push --force",
            "rm -rf /",
            "rm -rf ../outro",
            "rm -rf ~/x",
            "rm -rf $HOME/x",
            "rm -rf .git",
            "find /tmp -delete",
        ] {
            let mut rx = h.state.events().subscribe();
            let decision = approver.approve(&req_of(Role::Developer, cmd)).await;
            assert_eq!(decision, ApprovalDecision::Deny, "{cmd}: sem humano o timeout nega");
            let (requests, messages) = drain_requests_and_messages(&mut rx);
            // Pode haver 1 linha explicando por que o YOLO não decidiu (juiz ausente/recusou).
            assert!(requests == 1 && messages <= 1, "{cmd}: segue o fluxo atual (pedido ao humano)");
        }
        // Remoção dentro da worktree continua automática.
        assert_eq!(approve_now(&h.approver(None), Role::Developer, "rm -rf target/debug").await, ApprovalDecision::Once);
    }

    #[tokio::test]
    async fn yolo_config_change_in_docker_uses_container_rule() {
        let h = harness("yolo-cfg-docker");
        set_yolo(&h, true, &[]);
        let text = fs::read_to_string(h.root.join(SETTINGS_REL)).expect("ler");
        let text = if text.contains("environment:") { text.replace("environment: host", "environment: docker") } else { format!("{text}environment: docker\n") };
        fs::write(h.root.join(SETTINGS_REL), text).expect("settings");
        let approver = h.approver(Some(Duration::from_millis(80)));
        let cmds = vec!["mix test".to_string()];
        assert_eq!(approver.approve_config_change(None, "r", "d", &cmds).await, ApprovalDecision::Once);
        let bad = vec!["sudo mix test".to_string()];
        assert_eq!(approver.approve_config_change(None, "r", "d", &bad).await, ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn yolo_container_commands_use_hard_exclusions_only() {
        let h = harness("yolo-ctr");
        set_yolo(&h, true, &[]);
        let approver = h.approver(Some(Duration::from_millis(80)));
        let ctr = |c: &str| ApprovalRequest { in_container: true, ..req_of(Role::Developer, c) };
        for cmd in ["mix deps.update cowlib", "nl -ba lib/x.ex"] {
            let mut rx = h.state.events().subscribe();
            assert_eq!(approver.approve(&ctr(cmd)).await, ApprovalDecision::Once, "{cmd}");
            assert_eq!(drain_requests_and_messages(&mut rx), (0, 1), "{cmd}");
        }
        for cmd in ["sudo mix deps.get", "docker ps"] {
            let mut rx = h.state.events().subscribe();
            assert_eq!(approver.approve(&ctr(cmd)).await, ApprovalDecision::Deny, "{cmd}");
            assert_eq!(drain_requests_and_messages(&mut rx), (1, 0), "{cmd}");
        }
        // No host não há lista de programas (spec 062): `mix test` é automático; a denylist do projeto
        // vence no container.
        assert_eq!(approver.approve(&req_of(Role::Developer, "mix test")).await, ApprovalDecision::Once);
        set_yolo(&h, true, &["mix deps*"]);
        assert_eq!(approver.approve(&ctr("mix deps.get")).await, ApprovalDecision::Deny);
    }

    /// C-08: settings inválido com `yolo: true` manda o pedido ao humano e avisa UMA vez por run.
    #[tokio::test]
    async fn invalid_settings_with_yolo_warns_once_per_run() {
        let h = harness("yolo-invalid");
        let text = settings_text(&[]);
        fs::write(h.root.join(SETTINGS_REL), format!("{text}yolo: true\nmax_test_cycles: abc\n")).expect("settings");
        assert!(Settings::parse(&fs::read_to_string(h.root.join(SETTINGS_REL)).expect("ler")).is_err());
        let run = h.registry.begin_run("proj", None);
        let approver = h.approver(Some(Duration::from_millis(60))).with_run(run);
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver.approve(&req_of(Role::Developer, "npm install x")).await, ApprovalDecision::Deny);
        assert_eq!(approver.approve(&req_of(Role::Developer, "npm install y")).await, ApprovalDecision::Deny);
        let warnings: Vec<Value> = drain_agent_messages(&mut rx)
            .into_iter()
            .filter(|m| m["text"] == YOLO_IGNORED_TEXT)
            .collect();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0]["agent"], "system");
        assert_eq!(warnings[0]["code"], CODE_YOLO_IGNORED, "spec 102: aviso com code");
        // Outro run avisa de novo.
        let run2 = h.registry.begin_run("proj", None);
        let approver2 = h.approver(Some(Duration::from_millis(60))).with_run(run2);
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver2.approve(&req_of(Role::Developer, "npm install z")).await, ApprovalDecision::Deny);
        assert_eq!(drain_agent_messages(&mut rx).iter().filter(|m| m["text"] == YOLO_IGNORED_TEXT).count(), 1);
    }

    #[tokio::test]
    async fn valid_settings_without_yolo_never_warn() {
        let h = harness("yolo-valid-off");
        set_yolo(&h, false, &[]);
        let run = h.registry.begin_run("proj", None);
        let approver = h.approver(Some(Duration::from_millis(60))).with_run(run);
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver.approve(&req_of(Role::Developer, "npm install x")).await, ApprovalDecision::Deny);
        assert!(drain_agent_messages(&mut rx).iter().all(|m| m["text"] != YOLO_IGNORED_TEXT));
    }

    #[tokio::test]
    async fn yolo_project_denylist_wins_and_off_means_unchanged() {
        let h = harness("yolo-deny");
        set_yolo(&h, true, &["npm install*"]);
        let approver = h.approver(Some(Duration::from_millis(80)));
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver.approve(&req_of(Role::Developer, "npm install x")).await, ApprovalDecision::Deny);
        assert_eq!(drain_requests_and_messages(&mut rx), (1, 0));
        // Sem yolo: pedido normal ao humano.
        set_yolo(&h, false, &[]);
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver.approve(&req_of(Role::Developer, "npm install x")).await, ApprovalDecision::Deny);
        assert_eq!(drain_requests_and_messages(&mut rx), (1, 0));
    }

    #[tokio::test]
    async fn yolo_auto_approves_environment_change() {
        use crate::harness::tools::{EnvChangeRequest, EnvFileChange};
        let h = harness("yolo-env");
        set_yolo(&h, true, &[]);
        let mut rx = h.state.events().subscribe();
        let req = EnvChangeRequest {
            role: Role::Developer,
            task_id: Some("T01".into()),
            reason: "preciso do node".into(),
            files: vec![EnvFileChange { path: ".nants/Dockerfile".into(), diff: "+RUN x".into() }],
        };
        let decision = tokio::time::timeout(Duration::from_secs(2), h.approver(None).approve_environment(&req))
            .await
            .expect("imediato");
        assert_eq!(decision, ApprovalDecision::Once);
        let msgs = drain_agent_messages(&mut rx);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["kind"], "environment");
        assert!(msgs[0]["text"].as_str().unwrap_or("").starts_with(YOLO_AUTO_PREFIX));
    }

    #[tokio::test]
    async fn yolo_shell_write_to_settings_goes_to_the_human() {
        let h = harness("yolo-sh");
        set_yolo(&h, true, &[]);
        let approver = h.approver(Some(Duration::from_millis(80)));
        let cmd = format!("sh -c \"echo yolo: true >> {}/.sdd/settings.yaml\"", h.root.display());
        let mut rx = h.state.events().subscribe();
        assert_eq!(approver.approve(&req_of(Role::Developer, &cmd)).await, ApprovalDecision::Deny);
        // Sem juiz: um pedido ao humano e uma linha explicando por que o YOLO não decidiu.
        assert_eq!(drain_requests_and_messages(&mut rx), (1, 1));
        assert_eq!(fs::read_to_string(h.root.join(SETTINGS_REL)).expect("ler").matches("yolo: true").count(), 1);
    }

    // ----- Juiz do YOLO no host (spec 062/T02) -----

    struct FakeJudge {
        reply: Result<JudgeVerdict, String>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeJudge {
        fn new(reply: Result<JudgeVerdict, String>) -> Arc<Self> {
            Arc::new(Self { reply, calls: Mutex::new(Vec::new()) })
        }
        fn calls(&self) -> usize {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).len()
        }
    }

    impl CommandJudge for FakeJudge {
        fn judge<'a>(&'a self, command: &'a str, _root: &'a Path, _why: &'a str) -> super::super::judge::JudgeFut<'a> {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(command.to_string());
            let reply = self.reply.clone();
            Box::pin(async move { reply })
        }
    }

    fn verdict(safe: bool, reason: &str) -> Result<JudgeVerdict, String> {
        Ok(JudgeVerdict { safe, reason: reason.into() })
    }

    #[tokio::test]
    async fn yolo_judge_safe_approves_with_reason_and_caches_per_command() {
        let h = harness("yolo-judge-safe");
        set_yolo(&h, true, &[]);
        let judge = FakeJudge::new(verdict(true, "só imprime"));
        let approver = h.approver(Some(Duration::from_millis(80))).with_judge(judge.clone());
        let mut rx = h.state.events().subscribe();
        assert_eq!(approve_now(&approver, Role::Developer, "echo $(date)").await, ApprovalDecision::Once);
        let msgs = drain_agent_messages(&mut rx);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["yolo_auto"], true);
        let text = msgs[0]["text"].as_str().unwrap_or("");
        assert!(text.starts_with("aprovada automaticamente: echo $(date)") && text.contains("só imprime"), "{text}");
        assert_eq!(approve_now(&approver, Role::Developer, "echo $(date)").await, ApprovalDecision::Once);
        assert_eq!(judge.calls(), 1, "cache por comando");
        // Comando decidido pelas checagens não consulta o juiz.
        assert_eq!(approve_now(&approver, Role::Developer, "mix test").await, ApprovalDecision::Once);
        assert_eq!(judge.calls(), 1);
    }

    #[tokio::test]
    async fn yolo_judge_unsafe_error_or_missing_goes_to_the_human_with_reason() {
        let h = harness("yolo-judge-unsafe");
        set_yolo(&h, true, &[]);
        for (reply, expected) in [(verdict(false, "apaga fora da raiz"), "apaga fora da raiz"), (Err("falha".to_string()), "expansão dinâmica")] {
            let judge = FakeJudge::new(reply);
            let approver = h.approver(Some(Duration::from_millis(80))).with_judge(judge.clone());
            let mut rx = h.state.events().subscribe();
            let decision = approver.approve(&req_of(Role::Developer, "rm -rf $(pwd)/..")).await;
            assert_eq!(decision, ApprovalDecision::Deny, "sem humano o timeout nega");
            assert_eq!(judge.calls(), 1);
            let mut requests = 0;
            let mut said = String::new();
            while let Ok(ev) = rx.try_recv() {
                match ev.event_type() {
                    "permission_requested" => requests += 1,
                    "agent_message" => said = ev.payload["text"].as_str().unwrap_or("").to_string(),
                    _ => {}
                }
            }
            assert_eq!(requests, 1);
            assert!(said.contains(expected) && !said.contains(YOLO_AUTO_PREFIX), "{said}");
        }
    }

    #[tokio::test]
    async fn yolo_judge_never_overrides_exclusions_denylist_or_yolo_off() {
        let h = harness("yolo-judge-limits");
        set_yolo(&h, true, &["echo*"]);
        let judge = FakeJudge::new(verdict(true, "ok"));
        let approver = h.approver(Some(Duration::from_millis(80))).with_judge(judge.clone());
        // Denylist do projeto vence; exclusão de princípio e caminho fora da raiz não chegam ao juiz.
        for cmd in ["echo $(id)", "sudo ls $(id)", "cat ../fora $(id)", "git push"] {
            assert_eq!(approver.approve(&req_of(Role::Developer, cmd)).await, ApprovalDecision::Deny, "{cmd}");
        }
        assert_eq!(judge.calls(), 0);
        set_yolo(&h, false, &[]);
        assert_eq!(approver.approve(&req_of(Role::Developer, "cat $(id)")).await, ApprovalDecision::Deny);
        assert_eq!(judge.calls(), 0, "YOLO desligado não usa o juiz");
    }

    #[tokio::test]
    async fn yolo_judge_unsafe_on_root_removal_goes_to_the_human() {
        let h = harness("yolo-judge-root");
        set_yolo(&h, true, &[]);
        let judge = FakeJudge::new(verdict(false, "apaga a raiz"));
        let approver = h.approver(Some(Duration::from_millis(80))).with_judge(judge.clone());
        assert_eq!(approver.approve(&req_of(Role::Developer, "rm -rf .")).await, ApprovalDecision::Deny);
        assert_eq!(judge.calls(), 1);
        let big = format!("echo {}", "a".repeat(5000));
        assert_eq!(approver.approve(&req_of(Role::Developer, &big)).await, ApprovalDecision::Deny);
        assert_eq!(judge.calls(), 1, "comando enorme não vai ao juiz");
    }

    #[tokio::test]
    async fn yolo_host_paths_resolved_on_disk_go_to_the_human() {
        let h = harness("yolo-host-paths");
        set_yolo(&h, true, &[]);
        let approver = h.approver(Some(Duration::from_millis(80)));
        for cmd in ["cat ../fora", "rm -rf /tmp/../home", "cat .git/config", "git push"] {
            assert_eq!(approver.approve(&req_of(Role::Developer, cmd)).await, ApprovalDecision::Deny, "{cmd}");
        }
        for cmd in ["mix test", "clojure -M:test", "bundle exec rake", "nl -ba src/x.clj"] {
            assert_eq!(approve_now(&approver, Role::Developer, cmd).await, ApprovalDecision::Once, "{cmd}");
        }
    }

    #[tokio::test]
    async fn yolo_dialog_line_is_persisted_redacted() {
        let h = harness("yolo-dialog");
        set_yolo(&h, true, &[]);
        fs::create_dir_all(h.root.join(".sdd/specs/001-exemplo")).expect("spec dir");
        fs::write(h.root.join(".sdd/specs/001-exemplo/spec.md"), "---\nid: \"001-demo\"\n---\n").expect("spec");
        let approver = h.approver(None).with_dialog(DialogTarget::new(&h.root, "001-exemplo"));
        let cmd = format!("npm install pacote --registry-token {SECRET}");
        assert_eq!(approve_now(&approver, Role::Developer, &cmd).await, ApprovalDecision::Once);
        crate::server::dialog::flush_project(&h.root, Duration::from_secs(5));
        let raw = fs::read_to_string(h.root.join(".sdd/specs/001-exemplo/dialog.jsonl")).expect("dialog.jsonl");
        let line: Value = serde_json::from_str(raw.lines().next().expect("linha")).expect("json");
        let text = line["text"].as_str().unwrap_or("");
        assert!(text.starts_with("aprovada automaticamente: npm install pacote"), "{text}");
        assert!(!raw.contains(SECRET), "{raw}");
        assert_eq!(line["agent"], "developer");
    }
}
