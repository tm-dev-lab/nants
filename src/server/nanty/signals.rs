//! Sinais do Nanty (spec 138 T02): o que um observador viu, sem modelo.
//!
//! Cada tipo de sinal tem um observador fixo (a fonte, C-06), uma urgência da tabela fixa
//! ([`SignalKind::default_urgency`], ajustável em `[nanty.urgency]`) e o porquê: a linha fixa
//! `nanty.why.<tipo>` do dicionário, com o observador, os fatos e o horário. O modelo nunca decide
//! a urgência nem o porquê.

use serde_json::{Map, Value};

use crate::config::{NantyConfig, Urgency};

/// Grupo dos sinais sem projeto (disco, pasta temporária, Docker). Não é um id de projeto válido
/// (`validate_project_id` não aceita `*`), então nunca colide com um projeto.
pub const SYSTEM_GROUP: &str = "*";

/// Tipo de sinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SignalKind {
    /// Plano esperando aprovação (`build_attention`).
    PlanWaiting,
    /// Execução parada por crédito ou cota do provedor (`build_attention`, código da parada).
    NoCredit,
    /// Execução parada por outra falha do provedor (`build_attention`).
    ProviderStopped,
    /// Execução pausada com tarefa BLOCKED por outro motivo (`build_attention`).
    RunBlocked,
    /// Tarefa BLOCKED com `next_action_code = budget` (o runner já pausou: só sinal).
    Budget,
    /// Tarefa BLOCKED com `next_action_code = kept_recurring` (laço de revisão).
    ReviewLoop,
    /// Tarefas da spec em andamento acima do patamar de custo (`cost_alert`).
    CostAlert,
    /// Pouco espaço livre nas pastas do Nants (`statvfs` do `doctor`).
    DiskLow,
    /// Pouco espaço livre na pasta temporária (`statvfs` do `doctor`).
    TmpLow,
    /// Recursos Docker com o rótulo `nants.project` de projeto cadastrado e parado (verificação
    /// estrita de atividade): o que a ação "limpar órfãos" limpa (T03).
    DockerOrphans,
    /// Recursos Docker com o rótulo `nants.project` de projeto que esta instalação não conhece: só
    /// aviso informativo. Podem ser de outra instalação do Nants no mesmo Docker; o Nanty nunca os
    /// remove (T03).
    DockerForeign,
    /// Marco: tarefa entregue (integrada).
    TaskDelivered,
    /// Marco: spec concluída (fim da finalização).
    SpecDone,
    /// Marco: app pronto (iniciado).
    AppReady,
}

/// Observador que viu o sinal (vai nos fatos do porquê).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observer {
    Attention,
    Tasks,
    Cost,
    Disk,
    Docker,
    Progress,
}

impl Observer {
    pub fn as_str(self) -> &'static str {
        match self {
            Observer::Attention => "attention",
            Observer::Tasks => "tasks",
            Observer::Cost => "cost",
            Observer::Disk => "disk",
            Observer::Docker => "docker",
            Observer::Progress => "progress",
        }
    }
}

impl SignalKind {
    pub const ALL: [SignalKind; 14] = [
        SignalKind::PlanWaiting,
        SignalKind::NoCredit,
        SignalKind::ProviderStopped,
        SignalKind::RunBlocked,
        SignalKind::Budget,
        SignalKind::ReviewLoop,
        SignalKind::CostAlert,
        SignalKind::DiskLow,
        SignalKind::TmpLow,
        SignalKind::DockerOrphans,
        SignalKind::DockerForeign,
        SignalKind::TaskDelivered,
        SignalKind::SpecDone,
        SignalKind::AppReady,
    ];

    /// Sinais de condição vistos pela varredura de projetos (`build_attention` e `cost_alert`).
    pub const PROJECT_CONDITIONS: [SignalKind; 7] = [
        SignalKind::PlanWaiting,
        SignalKind::NoCredit,
        SignalKind::ProviderStopped,
        SignalKind::RunBlocked,
        SignalKind::Budget,
        SignalKind::ReviewLoop,
        SignalKind::CostAlert,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SignalKind::PlanWaiting => "plan_waiting",
            SignalKind::NoCredit => "no_credit",
            SignalKind::ProviderStopped => "provider_stopped",
            SignalKind::RunBlocked => "run_blocked",
            SignalKind::Budget => "budget",
            SignalKind::ReviewLoop => "review_loop",
            SignalKind::CostAlert => "cost_alert",
            SignalKind::DiskLow => "disk_low",
            SignalKind::TmpLow => "tmp_low",
            SignalKind::DockerOrphans => "docker_orphans",
            SignalKind::DockerForeign => "docker_foreign",
            SignalKind::TaskDelivered => "task_delivered",
            SignalKind::SpecDone => "spec_done",
            SignalKind::AppReady => "app_ready",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    pub fn observer(self) -> Observer {
        match self {
            SignalKind::PlanWaiting | SignalKind::NoCredit | SignalKind::ProviderStopped => Observer::Attention,
            SignalKind::RunBlocked | SignalKind::Budget | SignalKind::ReviewLoop => Observer::Tasks,
            SignalKind::CostAlert => Observer::Cost,
            SignalKind::DiskLow | SignalKind::TmpLow => Observer::Disk,
            SignalKind::DockerOrphans | SignalKind::DockerForeign => Observer::Docker,
            SignalKind::TaskDelivered | SignalKind::SpecDone | SignalKind::AppReady => Observer::Progress,
        }
    }

    /// Tabela fixa de urgência (decisão da spec 138, C-06):
    /// - urgente: crédito ou cota parada, disco baixo;
    /// - normal: plano esperando, execução pausada, e os marcos que pedem a pessoa (spec concluída,
    ///   app pronto);
    /// - só no resumo: tarefa entregue, aviso de custo, órfãos do Docker.
    pub fn default_urgency(self) -> Urgency {
        match self {
            SignalKind::NoCredit | SignalKind::DiskLow | SignalKind::TmpLow => Urgency::Urgent,
            SignalKind::PlanWaiting
            | SignalKind::ProviderStopped
            | SignalKind::RunBlocked
            | SignalKind::Budget
            | SignalKind::ReviewLoop
            | SignalKind::SpecDone
            | SignalKind::AppReady => Urgency::Normal,
            SignalKind::CostAlert | SignalKind::TaskDelivered | SignalKind::DockerOrphans | SignalKind::DockerForeign => {
                Urgency::Digest
            }
        }
    }

    /// Marco de progresso: acontece uma vez (não tem "resolvido").
    pub fn one_shot(self) -> bool {
        matches!(self, SignalKind::TaskDelivered | SignalKind::SpecDone | SignalKind::AppReady)
    }

    /// O porquê tem `{count}` (chaves `.one`/`.other` no dicionário).
    pub fn counted(self) -> bool {
        matches!(self, SignalKind::CostAlert | SignalKind::DockerOrphans | SignalKind::DockerForeign)
    }

    /// Código do porquê: a chave base no dicionário (`nanty.why.<tipo>`).
    pub fn why_code(self) -> String {
        format!("nanty.why.{}", self.as_str())
    }
}

/// Urgência efetiva: a da configuração, se a pessoa mudou; senão a da tabela fixa.
pub fn urgency_of(kind: SignalKind, cfg: &NantyConfig) -> Urgency {
    cfg.urgency.get(kind.as_str()).copied().unwrap_or_else(|| kind.default_urgency())
}

/// Inibição: (quem cala, quem é calado) no mesmo projeto. "Sem crédito" cala "execução bloqueada".
pub const INHIBITIONS: [(SignalKind, SignalKind); 2] = [
    (SignalKind::NoCredit, SignalKind::RunBlocked),
    (SignalKind::NoCredit, SignalKind::ProviderStopped),
];

/// O que um observador viu.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub kind: SignalKind,
    pub project_id: Option<String>,
    /// Fatos estáveis (contagens e códigos): mudou, é novidade.
    pub facts: Map<String, Value>,
    /// Fatos que variam a cada rodada (espaço livre): vão no cartão, mas não contam como novidade.
    pub extra: Map<String, Value>,
}

impl Signal {
    pub fn new(kind: SignalKind, project_id: Option<&str>) -> Self {
        Self { kind, project_id: project_id.map(str::to_string), facts: Map::new(), extra: Map::new() }
    }

    pub fn fact(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.facts.insert(name.to_string(), value.into());
        self
    }

    pub fn extra(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.extra.insert(name.to_string(), value.into());
        self
    }

    /// Grupo do filtro: projeto (ou [`SYSTEM_GROUP`]) e tipo.
    pub fn group(&self) -> String {
        group_key(self.project_id.as_deref(), self.kind)
    }
}

pub fn group_key(project_id: Option<&str>, kind: SignalKind) -> String {
    format!("{}:{}", project_id.unwrap_or(SYSTEM_GROUP), kind.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{lookup, Lang};

    #[test]
    fn urgency_table_is_fixed_and_configurable() {
        let cfg = NantyConfig::default();
        let urgent: Vec<&str> =
            SignalKind::ALL.into_iter().filter(|k| urgency_of(*k, &cfg) == Urgency::Urgent).map(SignalKind::as_str).collect();
        assert_eq!(urgent, ["no_credit", "disk_low", "tmp_low"]);
        let digest: Vec<&str> =
            SignalKind::ALL.into_iter().filter(|k| urgency_of(*k, &cfg) == Urgency::Digest).map(SignalKind::as_str).collect();
        assert_eq!(digest, ["cost_alert", "docker_orphans", "docker_foreign", "task_delivered"]);
        for normal in [SignalKind::PlanWaiting, SignalKind::RunBlocked, SignalKind::Budget, SignalKind::ReviewLoop] {
            assert_eq!(urgency_of(normal, &cfg), Urgency::Normal, "{normal:?}");
        }
        let mut changed = NantyConfig::default();
        changed.urgency.insert("plan_waiting".into(), Urgency::Urgent);
        changed.urgency.insert("nao_existe".into(), Urgency::Digest);
        assert_eq!(urgency_of(SignalKind::PlanWaiting, &changed), Urgency::Urgent);
        assert_eq!(urgency_of(SignalKind::DiskLow, &changed), Urgency::Urgent);
    }

    #[test]
    fn kinds_round_trip_and_every_why_line_exists_in_both_languages() {
        for kind in SignalKind::ALL {
            assert_eq!(SignalKind::parse(kind.as_str()), Some(kind));
            let base = kind.why_code();
            let keys: Vec<String> =
                if kind.counted() { vec![format!("{base}.one"), format!("{base}.other")] } else { vec![base.clone()] };
            for key in keys {
                for lang in Lang::ALL {
                    let line = lookup(lang, &key).unwrap_or_else(|| panic!("{key} sem texto em {lang:?}"));
                    assert!(line.contains("{since}"), "{key}: o porquê traz o horário");
                }
            }
        }
        assert_eq!(SignalKind::parse("outro"), None);
        assert_eq!(Signal::new(SignalKind::DiskLow, None).group(), "*:disk_low");
        assert_eq!(Signal::new(SignalKind::Budget, Some("p1")).group(), "p1:budget");
    }
}
