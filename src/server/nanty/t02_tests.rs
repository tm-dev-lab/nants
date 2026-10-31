//! Testes dos observadores e do filtro do Nanty (spec 138 T02): cada observador com estado
//! simulado, a tabela de urgência, o filtro (grupo, espera, intervalo, repetição, inibição,
//! silêncio, horário) e o resolvido. O relógio é sempre o injetável da T01.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{json, Value};

use super::clock::{at, fixed_clock, LocalTime};
use super::filter::{apply, delivery_for, next_due_ms, RESOLVED};
use super::journal::JournalKind;
use super::observe::{
    docker_signals, milestone, observe_machine, project_signals, space_signal, BlockedTask, NantyProbe, ProjectFacts,
};
use super::signals::{Signal, SignalKind};
use super::store::{Delivery, NantyState, Silence, Store};
use super::*;
use crate::config::{Config, NantyConfig, Project, Urgency, GB_BYTES};
use crate::harness::compose::fake::FakeRunner;
use crate::server::events::ApiEvent;

/// 2026-10-30 12:00 UTC (o relógio fixo da T01).
const NOON_MS: u64 = 1_793_361_600_000;
const SEC: u64 = 1000;
const MIN: u64 = 60 * SEC;
const HOUR: u64 = 60 * MIN;

/// Hora local `12:00 + ms` do dia 30 (sem passar da meia-noite).
fn noon_plus(ms: u64) -> LocalTime {
    let mins = ms / MIN;
    at((2026, 10, 30), 12 + u8::try_from(mins / 60).expect("hora"), u8::try_from(mins % 60).expect("min"), NOON_MS + ms)
}

/// 23:00 local do dia 30 (dentro do silêncio padrão, 22:00 a 07:00).
fn night_plus(ms: u64) -> LocalTime {
    at((2026, 10, 30), 23, u8::try_from(ms / MIN).expect("min"), NOON_MS + 11 * HOUR + ms)
}

fn plan(project: &str) -> Signal {
    Signal::new(SignalKind::PlanWaiting, Some(project))
}

fn blocked(project: &str, attempts: u32) -> Signal {
    Signal::new(SignalKind::RunBlocked, Some(project)).fact("task_id", "T02").fact("attempts", attempts)
}

fn no_credit(project: &str) -> Signal {
    Signal::new(SignalKind::NoCredit, Some(project)).fact("stop", "run.billing").fact("role", "developer")
}

fn cards_of<'a>(st: &'a NantyState, group: &str) -> Vec<&'a super::store::Card> {
    st.cards.iter().filter(|c| c.group == group).collect()
}

fn codes(out: &super::filter::Outcome, kind: JournalKind) -> Vec<String> {
    out.journal.iter().filter(|e| e.kind == kind).map(|e| e.code.clone()).collect()
}

const PROJECT_KINDS: [SignalKind; 7] = SignalKind::PROJECT_CONDITIONS;

#[test]
fn group_wait_then_notify_and_a_round_without_news_changes_nothing() {
    let cfg = NantyConfig::default();
    let mut st = NantyState::default();
    let first = apply(&mut st, &[plan("p1")], &PROJECT_KINDS, &cfg, noon_plus(0));
    assert!(first.changed, "condição nova é novidade");
    assert_eq!(codes(&first, JournalKind::Signal), ["plan_waiting"]);
    assert!(st.cards.is_empty(), "espera do grupo antes do primeiro aviso");
    assert_eq!(next_due_ms(&st, &cfg, NOON_MS), Some(30 * SEC));

    let waiting = apply(&mut st, &[plan("p1")], &PROJECT_KINDS, &cfg, noon_plus(10 * SEC));
    assert!(!waiting.changed && waiting.journal.is_empty(), "sem novidade: nada muda, nada grava");

    let sent = apply(&mut st, &[plan("p1")], &PROJECT_KINDS, &cfg, noon_plus(30 * SEC));
    assert!(sent.changed);
    assert_eq!(codes(&sent, JournalKind::Filter), ["notify"]);
    let card = cards_of(&st, "p1:plan_waiting")[0].clone();
    assert_eq!(card.code, "nanty.why.plan_waiting");
    assert_eq!((card.urgency, card.delivery), (Some(Urgency::Normal), Some(Delivery::Push)));
    assert_eq!(card.params["observer"], "attention");
    assert_eq!(card.params["since"], "12:00", "o horário em que o observador viu");
    assert_eq!(st.last_sent["p1:plan_waiting"], NOON_MS + 30 * SEC);
    assert_eq!(next_due_ms(&st, &cfg, NOON_MS), None);

    for n in 1..5 {
        let quiet = apply(&mut st, &[plan("p1")], &PROJECT_KINDS, &cfg, noon_plus(30 * SEC + n * MIN));
        assert!(!quiet.changed && quiet.journal.is_empty(), "rodada {n} sem novidade");
    }
}

#[test]
fn new_facts_wait_the_group_interval_and_the_same_facts_wait_the_repeat_interval() {
    let mut cfg = NantyConfig::default();
    cfg.filter.group_wait_secs = 0;
    let mut st = NantyState::default();
    apply(&mut st, &[blocked("p1", 1)], &PROJECT_KINDS, &cfg, noon_plus(0));
    let id = cards_of(&st, "p1:run_blocked")[0].id.clone();

    let changed = apply(&mut st, &[blocked("p1", 2)], &PROJECT_KINDS, &cfg, noon_plus(MIN));
    assert!(changed.changed, "fatos novos ficam guardados");
    assert!(codes(&changed, JournalKind::Filter).is_empty(), "dentro do intervalo do grupo");
    assert_eq!(cards_of(&st, "p1:run_blocked")[0].params["attempts"], 1);

    let update = apply(&mut st, &[blocked("p1", 2)], &PROJECT_KINDS, &cfg, noon_plus(5 * MIN));
    assert_eq!(codes(&update, JournalKind::Filter), ["update"]);
    let card = cards_of(&st, "p1:run_blocked")[0].clone();
    assert_eq!((card.id.as_str(), &card.params["attempts"]), (id.as_str(), &json!(2)), "o mesmo cartão, editado");

    let early = apply(&mut st, &[blocked("p1", 2)], &PROJECT_KINDS, &cfg, noon_plus(5 * MIN + 3 * HOUR));
    assert!(!early.changed);
    let repeat = apply(&mut st, &[blocked("p1", 2)], &PROJECT_KINDS, &cfg, noon_plus(5 * MIN + 4 * HOUR));
    assert_eq!(codes(&repeat, JournalKind::Filter), ["repeat"]);
    let renewed = cards_of(&st, "p1:run_blocked")[0].clone();
    assert_eq!(renewed.id, id);
    assert_eq!(renewed.expires_at_ms, NOON_MS + 5 * MIN + 4 * HOUR + 8 * HOUR, "a repetição renova a validade");
    assert_eq!(renewed.params["age_min"], 5 + 4 * 60, "a idade conta desde a primeira vez");
}

#[test]
fn no_credit_inhibits_run_blocked_of_the_same_project_only() {
    let mut cfg = NantyConfig::default();
    cfg.filter.group_wait_secs = 0;
    let mut st = NantyState::default();
    let out = apply(&mut st, &[no_credit("p1"), blocked("p1", 1), blocked("p2", 1)], &PROJECT_KINDS, &cfg, noon_plus(0));
    assert!(codes(&out, JournalKind::Filter).contains(&"inhibited".to_string()));
    assert!(cards_of(&st, "p1:run_blocked").is_empty(), "calado pelo sem crédito");
    assert_eq!(cards_of(&st, "p2:run_blocked").len(), 1, "outro projeto segue");
    let credit = cards_of(&st, "p1:no_credit")[0].clone();
    assert_eq!((credit.urgency, credit.delivery), (Some(Urgency::Urgent), Some(Delivery::Push)));
    assert_eq!(st.alerts["p1:run_blocked"].suppressed.as_deref(), Some("inhibited"));
    // Sem novidade: a inibição não grava de novo.
    assert!(!apply(&mut st, &[no_credit("p1"), blocked("p1", 1), blocked("p2", 1)], &PROJECT_KINDS, &cfg, noon_plus(MIN)).changed);

    // O crédito voltou: o cartão do sem crédito fecha e o bloqueio, já fora da espera, avisa.
    let back = apply(&mut st, &[blocked("p1", 1), blocked("p2", 1)], &PROJECT_KINDS, &cfg, noon_plus(2 * MIN));
    assert_eq!(codes(&back, JournalKind::Filter), [RESOLVED, "notify"]);
    assert!(cards_of(&st, "p1:no_credit").is_empty());
    assert_eq!(cards_of(&st, "p1:run_blocked").len(), 1);
}

#[test]
fn silences_hold_until_they_expire() {
    let mut cfg = NantyConfig::default();
    cfg.filter.group_wait_secs = 0;
    let mut st = NantyState::default();
    st.silences.push(Silence { project_id: Some("p1".into()), group: None, until_ms: NOON_MS + 10 * MIN });
    st.silences.push(Silence { project_id: None, group: Some("p2:plan_waiting".into()), until_ms: NOON_MS + 10 * MIN });
    let out = apply(&mut st, &[plan("p1"), plan("p2"), plan("p3")], &PROJECT_KINDS, &cfg, noon_plus(0));
    assert_eq!(codes(&out, JournalKind::Filter), ["silenced", "silenced", "notify"]);
    assert_eq!(st.cards.iter().map(|c| c.group.as_str()).collect::<Vec<_>>(), ["p3:plan_waiting"]);
    assert!(!apply(&mut st, &[plan("p1"), plan("p2"), plan("p3")], &PROJECT_KINDS, &cfg, noon_plus(5 * MIN)).changed);

    let after = apply(&mut st, &[plan("p1"), plan("p2"), plan("p3")], &PROJECT_KINDS, &cfg, noon_plus(10 * MIN));
    assert_eq!(codes(&after, JournalKind::Filter), ["notify", "notify"], "o silêncio venceu");
    assert!(st.silences.is_empty(), "o silêncio vencido sai do estado");
    assert_eq!(st.cards.len(), 3);

    // Silêncio de tudo (sem projeto e sem grupo) cala também o sistema e os marcos.
    let mut all = NantyState::default();
    all.silences.push(Silence { project_id: None, group: None, until_ms: NOON_MS + HOUR });
    let disk = Signal::new(SignalKind::DiskLow, None).fact("min_gb", 5).fact("count", 1);
    let spec = Signal::new(SignalKind::SpecDone, Some("p1"));
    apply(&mut all, &[disk, spec], &[SignalKind::DiskLow], &cfg, noon_plus(0));
    assert!(all.cards.is_empty() && all.digest.is_empty());
}

#[test]
fn quiet_hours_send_urgent_without_sound_and_the_rest_to_the_digest() {
    let mut cfg = NantyConfig::default();
    cfg.filter.group_wait_secs = 0;
    assert_eq!(delivery_for(Urgency::Urgent, true), Delivery::Silent);
    assert_eq!(delivery_for(Urgency::Normal, true), Delivery::Digest);
    assert_eq!(delivery_for(Urgency::Normal, false), Delivery::Push);
    assert_eq!(delivery_for(Urgency::Digest, false), Delivery::Digest);

    let mut st = NantyState::default();
    let disk = Signal::new(SignalKind::DiskLow, None).fact("min_gb", 5).fact("count", 1).extra("free_gb", 2);
    let cost = Signal::new(SignalKind::CostAlert, Some("p1")).fact("count", 2);
    let signals = [disk, plan("p1"), cost];
    let covered = [&PROJECT_KINDS[..], &[SignalKind::DiskLow]].concat();
    apply(&mut st, &signals, &covered, &cfg, night_plus(0));
    let disk_card = cards_of(&st, "*:disk_low")[0].clone();
    assert_eq!(disk_card.delivery, Some(Delivery::Silent), "urgente vai sem som");
    assert_eq!(disk_card.params["free_gb"], 2);
    assert_eq!(cards_of(&st, "p1:plan_waiting")[0].delivery, Some(Delivery::Digest), "o resto vai para o resumo");
    assert!(cards_of(&st, "p1:cost_alert").is_empty(), "só no resumo: sem cartão");
    let digest: Vec<&str> = st.digest.iter().map(|d| d.group.as_str()).collect();
    assert_eq!(digest, ["p1:cost_alert", "p1:plan_waiting"], "na ordem dos grupos");
    assert_eq!(st.digest[0].code, "nanty.why.cost_alert");

    // A repetição no silêncio renova o cartão, mas não enfileira o mesmo aviso de novo.
    let repeat = apply(&mut st, &signals, &covered, &cfg, night_plus(0).with_ms(NOON_MS + 11 * HOUR + 4 * HOUR));
    assert_eq!(codes(&repeat, JournalKind::Filter), ["repeat", "repeat"]);
    assert_eq!(st.digest.len(), 2);
}

trait WithMs {
    fn with_ms(self, epoch_ms: u64) -> Self;
}

impl WithMs for LocalTime {
    fn with_ms(mut self, epoch_ms: u64) -> Self {
        self.epoch_ms = epoch_ms;
        self
    }
}

#[test]
fn a_condition_that_goes_away_closes_its_card_as_resolved() {
    let mut cfg = NantyConfig::default();
    cfg.filter.group_wait_secs = 0;
    let mut st = NantyState::default();
    let disk = Signal::new(SignalKind::DiskLow, None).fact("min_gb", 5).fact("count", 1);
    apply(&mut st, &[plan("p1"), disk.clone()], &[&PROJECT_KINDS[..], &[SignalKind::DiskLow]].concat(), &cfg, noon_plus(0));
    let card = cards_of(&st, "p1:plan_waiting")[0].clone();

    // O disco não foi medido nesta rodada (fonte falhou): não conta como resolvido.
    let out = apply(&mut st, &[], &PROJECT_KINDS, &cfg, noon_plus(MIN));
    assert_eq!(codes(&out, JournalKind::Filter), [RESOLVED]);
    assert!(cards_of(&st, "p1:plan_waiting").is_empty(), "o cartão fecha");
    assert_eq!(cards_of(&st, "*:disk_low").len(), 1, "fora da cobertura da rodada");
    let done = st.done.last().expect("feito").clone();
    assert_eq!((done.result.as_str(), done.card_id.as_deref()), (RESOLVED, Some(card.id.as_str())));
    assert_eq!(done.code, "nanty.why.plan_waiting");
    assert!(!st.alerts.contains_key("p1:plan_waiting") && !st.last_sent.contains_key("p1:plan_waiting"));

    // Ainda na espera do grupo: some sem cartão, sem "feito".
    let mut fresh = NantyState::default();
    apply(&mut fresh, &[plan("p2")], &PROJECT_KINDS, &NantyConfig::default(), noon_plus(0));
    assert!(apply(&mut fresh, &[], &PROJECT_KINDS, &NantyConfig::default(), noon_plus(SEC)).changed);
    assert!(fresh.alerts.is_empty() && fresh.done.is_empty());
}

#[test]
fn digest_only_signals_never_open_cards_nor_repeat() {
    let mut cfg = NantyConfig::default();
    cfg.filter.group_wait_secs = 0;
    let mut st = NantyState::default();
    let orphans = Signal::new(SignalKind::DockerOrphans, None).fact("count", 3);
    apply(&mut st, &[orphans.clone()], &[SignalKind::DockerOrphans], &cfg, noon_plus(0));
    assert!(st.cards.is_empty());
    assert_eq!(st.digest.len(), 1);
    assert!(!apply(&mut st, &[orphans], &[SignalKind::DockerOrphans], &cfg, noon_plus(5 * HOUR)).changed, "o resumo não repete");

    // A urgência é da configuração: a pessoa pode subir os órfãos para normal.
    cfg.urgency.insert("docker_orphans".into(), Urgency::Normal);
    let mut st = NantyState::default();
    apply(&mut st, &[Signal::new(SignalKind::DockerOrphans, None).fact("count", 1)], &[], &cfg, noon_plus(0));
    assert_eq!(cards_of(&st, "*:docker_orphans")[0].urgency, Some(Urgency::Normal));
}

#[test]
fn milestones_are_delivered_once_and_never_resolve() {
    let cfg = NantyConfig::default();
    // Os marcos nascem dos eventos de verdade (com o `spec_id` do payload).
    let ev = |kind: EventType, payload: Value| milestone(&ApiEvent::new(kind, Some("p1".into()), payload), &cfg).expect("marco");
    let delivered = |spec: &str, task: &str| {
        ev(EventType::TaskUpdated, json!({"spec_id": spec, "task_id": task, "status": "DONE", "integrated": true}))
    };
    let spec_done = |spec: &str| ev(EventType::FinalizationDone, json!({"spec_id": spec}));
    let mut st = NantyState::default();
    let out = apply(&mut st, &[delivered("001-a", "T01")], &[], &cfg, noon_plus(0));
    assert_eq!(codes(&out, JournalKind::Filter), ["notify"], "o marco não espera");
    assert_eq!(st.digest.len(), 1, "tarefa entregue: só no resumo");
    assert_eq!(st.digest[0].params["spec_id"], "001-a");
    let again = apply(&mut st, &[delivered("001-a", "T01")], &[], &cfg, noon_plus(SEC));
    assert_eq!(st.digest.len(), 1, "o mesmo evento repetido não duplica");
    assert!(!again.changed && again.journal.is_empty(), "nem vai ao registro");
    apply(&mut st, &[delivered("001-a", "T02")], &[], &cfg, noon_plus(2 * SEC));
    assert_eq!(st.digest.len(), 2, "outra tarefa é outro marco");
    apply(&mut st, &[delivered("002-b", "T01")], &[], &cfg, noon_plus(3 * SEC));
    assert_eq!(st.digest.len(), 3, "a T01 de outra spec é outro marco");

    apply(&mut st, &[spec_done("001-a")], &[], &cfg, noon_plus(4 * SEC));
    let card = cards_of(&st, "p1:spec_done")[0].clone();
    assert_eq!((card.urgency, card.delivery), (Some(Urgency::Normal), Some(Delivery::Push)));
    assert_eq!(card.expires_at_ms, NOON_MS + 4 * SEC + 4 * HOUR);
    assert_eq!(card.params["spec_id"], "001-a");
    let repeated = apply(&mut st, &[spec_done("001-a")], &[], &cfg, noon_plus(5 * SEC));
    assert!(!repeated.changed, "a mesma conclusão repetida é descartada");
    let next = apply(&mut st, &[spec_done("002-b")], &[], &cfg, noon_plus(6 * SEC));
    assert_eq!(codes(&next, JournalKind::Filter), ["notify"], "a spec seguinte do mesmo projeto avisa");
    let card = cards_of(&st, "p1:spec_done")[0].clone();
    assert_eq!(card.params["spec_id"], "002-b");
    let app = apply(&mut st, &[ev(EventType::AppStarted, json!({"spec_id": "002-b"}))], &[], &cfg, noon_plus(7 * SEC));
    assert_eq!(codes(&app, JournalKind::Filter), ["notify"]);
    assert!(st.alerts.is_empty(), "marco não é condição");
    apply(&mut st, &[], &SignalKind::ALL, &cfg, noon_plus(MIN));
    assert_eq!(cards_of(&st, "p1:spec_done").len(), 1, "marco não se resolve");

    // Sem fato nenhum (evento sem `spec_id`), o marco nunca é dado como repetido.
    let bare = Signal::new(SignalKind::SpecDone, Some("p1"));
    let first = apply(&mut st, &[bare.clone()], &[], &cfg, noon_plus(2 * MIN));
    let second = apply(&mut st, &[bare], &[], &cfg, noon_plus(3 * MIN));
    assert_eq!((codes(&first, JournalKind::Filter), codes(&second, JournalKind::Filter)), (vec!["notify".to_string()], vec!["notify".to_string()]));
}

#[test]
fn proactive_card_carries_only_counts_and_codes() {
    let mut cfg = NantyConfig::default();
    cfg.filter.group_wait_secs = 0;
    let facts = ProjectFacts {
        project_id: "p1".into(),
        plan_waiting: true,
        blocked: Some(BlockedTask { task_id: "T03".into(), code: Some("budget".into()), attempts: 2 }),
        stop_code: Some("run.billing".into()),
        role: Some("developer"),
        cost_alerts: 1,
        ..ProjectFacts::default()
    };
    let mut st = NantyState::default();
    apply(&mut st, &project_signals(&facts), &PROJECT_KINDS, &cfg, noon_plus(0));
    assert_eq!(st.cards.len(), 3, "plano, sem crédito e orçamento (o custo vai ao resumo)");
    let allowed = ["observer", "since", "age_min", "count", "task_id", "attempts", "reason", "stop", "role", "min_gb", "free_gb"];
    for card in &st.cards {
        for (key, value) in &card.params {
            assert!(allowed.contains(&key.as_str()), "{key} fora do cartão proativo");
            let code_like = value.as_str().is_none_or(|s| s.len() <= 32 && !s.contains(' '));
            assert!(value.is_number() || code_like, "{key}: {value}");
        }
    }
}

#[test]
fn project_observer_maps_attention_tasks_and_cost() {
    let base = ProjectFacts { project_id: "p1".into(), ..ProjectFacts::default() };
    assert!(project_signals(&base).is_empty());
    let kinds = |f: &ProjectFacts| project_signals(f).into_iter().map(|s| s.kind).collect::<Vec<_>>();
    let task = |code: Option<&str>| BlockedTask { task_id: "T01".into(), code: code.map(Into::into), attempts: 1 };
    assert_eq!(kinds(&ProjectFacts { plan_waiting: true, ..base.clone() }), [SignalKind::PlanWaiting]);
    assert_eq!(kinds(&ProjectFacts { blocked: Some(task(Some("budget"))), ..base.clone() }), [SignalKind::Budget]);
    assert_eq!(kinds(&ProjectFacts { blocked: Some(task(Some("kept_recurring"))), ..base.clone() }), [SignalKind::ReviewLoop]);
    assert_eq!(kinds(&ProjectFacts { blocked: Some(task(Some("tests_failed"))), ..base.clone() }), [SignalKind::RunBlocked]);
    assert_eq!(kinds(&ProjectFacts { blocked: Some(task(None)), ..base.clone() }), [SignalKind::RunBlocked]);
    for code in ["run.billing", "run.spend_limit", "run.daily_quota", "run.plan_limit"] {
        assert_eq!(kinds(&ProjectFacts { stop_code: Some(code.into()), ..base.clone() }), [SignalKind::NoCredit], "{code}");
    }
    for code in ["run.provider_failed", "run.plan_needs_login"] {
        assert_eq!(kinds(&ProjectFacts { stop_code: Some(code.into()), ..base.clone() }), [SignalKind::ProviderStopped], "{code}");
    }
    let cost = project_signals(&ProjectFacts { cost_alerts: 2, ..base.clone() });
    assert_eq!((cost[0].kind, &cost[0].facts["count"]), (SignalKind::CostAlert, &json!(2)));
}

#[test]
fn disk_observer_uses_free_space_and_docker_observer_only_detects() {
    let min = 5 * GB_BYTES;
    assert!(space_signal(SignalKind::DiskLow, &[], min, 5).is_none(), "nada medido: não cobre");
    assert_eq!(space_signal(SignalKind::DiskLow, &[min, 50 * GB_BYTES], min, 5), Some(None), "no limite está bem");
    let low = space_signal(SignalKind::TmpLow, &[min - 1, 50 * GB_BYTES], min, 5).flatten().expect("baixo");
    assert_eq!((low.kind, &low.facts["count"], &low.extra["free_gb"]), (SignalKind::TmpLow, &json!(1), &json!(4)));
    assert!(!low.facts.contains_key("free_gb"), "o espaço livre muda a cada rodada: não é novidade");

    // Órfão = projeto cadastrado e parado; o de projeto desconhecido vira só o aviso informativo.
    let known = vec!["p1".to_string(), "p2".to_string()];
    let kinds = |s: &[Signal]| s.iter().map(|x| (x.kind, x.facts["count"].clone())).collect::<Vec<_>>();
    assert!(docker_signals(&["p1".into()], &known, &["p1".into()]).is_empty(), "o ativo não é órfão");
    let found = docker_signals(&["p1".into(), "p2".into(), "ghost".into(), "ghost".into()], &known, &["p2".into()]);
    assert_eq!(kinds(&found), [(SignalKind::DockerOrphans, json!(1)), (SignalKind::DockerForeign, json!(2))]);

    let runner = FakeRunner::default();
    runner.reply("ps", 0, "abc\tghost\ndef\tp1\n");
    runner.reply("network", 0, "n1\tghost\n");
    let found = crate::server::envs::labeled_resources(&runner).expect("lista");
    assert_eq!(found.iter().map(|r| (r.kind, r.project.as_str())).collect::<Vec<_>>(), [
        ("container", "ghost"),
        ("container", "p1"),
        ("network", "ghost")
    ]);
    assert_eq!(runner.subcommands(), ["ps", "network"], "só lista: nada é parado nem apagado");
    let failing = FakeRunner::default();
    failing.reply("ps", 1, "Cannot connect to the Docker daemon");
    assert!(crate::server::envs::labeled_resources(&failing).is_err());
}

/// Sonda falsa: espaço livre por caminho (a pasta temporária à parte) e a lista do Docker.
#[derive(Default)]
struct FakeProbe {
    disk: Option<u64>,
    tmp: Option<u64>,
    docker: Mutex<Option<Vec<String>>>,
}

impl NantyProbe for FakeProbe {
    fn free_bytes(&self, path: &Path) -> Option<u64> {
        if path == std::env::temp_dir() {
            self.tmp
        } else {
            self.disk
        }
    }

    fn docker_labeled(&self) -> Option<Vec<String>> {
        self.docker.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[test]
fn machine_observer_covers_only_what_it_measured() {
    let mut cfg = Config::default();
    cfg.projects.push(Project { id: "p1".into(), name: "p1".into(), path: "/nada".into(), created_at: String::new() });
    let probe = FakeProbe {
        disk: Some(100 * GB_BYTES),
        tmp: Some(GB_BYTES),
        docker: Mutex::new(Some(vec!["p1".into(), "ghost".into()])),
    };
    let scan = observe_machine(&probe, &cfg, true, &[]);
    assert_eq!(
        scan.covered,
        [SignalKind::DiskLow, SignalKind::TmpLow, SignalKind::DockerOrphans, SignalKind::DockerForeign]
    );
    assert_eq!(
        scan.signals.iter().map(|s| s.kind).collect::<Vec<_>>(),
        [SignalKind::TmpLow, SignalKind::DockerOrphans, SignalKind::DockerForeign]
    );
    let busy = observe_machine(&probe, &cfg, true, &["p1".into()]);
    assert!(!busy.signals.iter().any(|s| s.kind == SignalKind::DockerOrphans), "projeto ativo não tem órfão");
    let quiet = observe_machine(&probe, &cfg, false, &[]);
    assert!(!quiet.covered.contains(&SignalKind::DockerOrphans), "o Docker só entra a cada algumas rodadas");
    let blind = observe_machine(&FakeProbe::default(), &cfg, true, &[]);
    assert!(blind.covered.is_empty() && blind.signals.is_empty(), "sem medida, nada é dado por resolvido");
}

#[test]
fn progress_milestones_come_from_events_of_observed_projects() {
    let mut cfg = NantyConfig::default();
    let ev = |kind: EventType, project: &str, payload: Value| ApiEvent::new(kind, Some(project.into()), payload);
    let integrated = ev(EventType::TaskUpdated, "p1", json!({"spec_id": "s", "task_id": "T04", "status": "DONE", "integrated": true}));
    let found = milestone(&integrated, &cfg).expect("entregue");
    assert_eq!((found.kind, &found.facts["task_id"]), (SignalKind::TaskDelivered, &json!("T04")));
    assert_eq!(found.facts["spec_id"], "s");
    assert!(milestone(&ev(EventType::TaskUpdated, "p1", json!({"status": "DONE"})), &cfg).is_none(), "só a integração");
    assert_eq!(milestone(&ev(EventType::FinalizationDone, "p1", json!({"spec_id": "s"})), &cfg).map(|s| s.kind), Some(SignalKind::SpecDone));
    assert_eq!(milestone(&ev(EventType::AppStarted, "p1", json!({})), &cfg).map(|s| s.kind), Some(SignalKind::AppReady));
    assert!(milestone(&ev(EventType::AgentActivity, "p1", json!({})), &cfg).is_none());
    cfg.excluded_projects.push("p1".into());
    assert!(milestone(&integrated, &cfg).is_none(), "projeto excluído não é observado");
}

#[test]
fn filter_values_are_written_only_when_changed() {
    let mut cfg = Config::default();
    let text = toml::to_string(&cfg).expect("ser");
    assert!(!text.contains("nanty"), "{text}");
    cfg.nanty.filter.repeat_interval_secs = 7200;
    cfg.nanty.urgency.insert("plan_waiting".into(), Urgency::Urgent);
    let text = toml::to_string(&cfg).expect("ser");
    let section = text.split("[nanty").skip(1).collect::<Vec<_>>().join("[nanty");
    assert!(section.contains("repeat_interval_secs = 7200") && section.contains("plan_waiting = \"urgent\""), "{text}");
    for absent in ["group_wait_secs", "group_interval_secs", "scan_interval_secs", "disk_min_free_gb", "tmp_min_free_gb"] {
        assert!(!section.contains(absent), "{absent}: {text}");
    }
    let back: Config = toml::from_str(&text).expect("de");
    assert_eq!(back.nanty, cfg.nanty);
    cfg.nanty.filter.scan_interval_secs = 0;
    assert_eq!(cfg.nanty.filter.scan_interval(), Duration::from_secs(10), "varredura nunca abaixo do mínimo");
}

// ---- Laço completo, com projeto simulado no disco e a sonda falsa ----

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-t02-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

fn write_project(root: &Path, spec_status: &str, task: (&str, Option<&str>)) {
    let spec = root.join(".sdd/specs/001-demo");
    std::fs::create_dir_all(spec.join("tasks")).expect("mkdir");
    std::fs::write(root.join(".sdd/settings.yaml"), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\n")
        .expect("settings");
    std::fs::write(
        spec.join("spec.md"),
        format!("---\nid: \"001-demo\"\ntitle: \"Demo\"\nstatus: \"{spec_status}\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-30T10:00:00Z\"\n---\n\n## Contexto\nx\n"),
    )
    .expect("spec");
    let code = task.1.map(|c| format!("next_action_code: \"{c}\"\n")).unwrap_or_default();
    std::fs::write(
        spec.join("tasks/T01.md"),
        format!(
            "---\nid: \"T01\"\nspec_id: \"001-demo\"\ntitle: \"Tarefa\"\nstatus: \"{}\"\ndepends_on: []\nbranch: \"\"\nattempts: 2\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\n{code}allowed_scope:\n  - \"src/**\"\n---\n\n## Objetivo\nx\n",
            task.0
        ),
    )
    .expect("task");
}

async fn wait_for(mut ok: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condição não chegou");
}

#[tokio::test]
async fn the_loop_observes_notifies_resolves_and_stays_quiet_without_news() {
    let dir = temp("loop");
    let root = dir.join("p1");
    write_project(&root, "AWAITING_APPROVAL", ("TODO", None));
    let mut cfg = Config::default();
    cfg.projects.push(Project {
        id: "p1".into(),
        name: "Projeto".into(),
        path: root.to_str().expect("utf-8").into(),
        created_at: "2026-10-30T00:00:00Z".into(),
    });
    cfg.nanty.filter.group_wait_secs = 0;
    let probe = Arc::new(FakeProbe {
        disk: Some(100 * GB_BYTES),
        tmp: Some(GB_BYTES),
        docker: Mutex::new(Some(vec!["ghost".into()])),
    });
    let st = AppState::new(dir.join("config.toml"), cfg, 3000)
        .with_data_dir(dir.join("data"))
        .with_nanty_clock(fixed_clock(at((2026, 10, 30), 12, 0, NOON_MS)))
        .with_nanty_probe(probe);
    let running = start(&st);
    let nanty = Arc::clone(st.nanty());
    wait_for(|| nanty.scans() >= 1).await;
    let snap = nanty.snapshot();
    let mut groups: Vec<&str> = snap.cards.iter().map(|c| c.group.as_str()).collect();
    groups.sort_unstable();
    assert_eq!(groups, ["*:tmp_low", "p1:plan_waiting"], "{snap:?}");
    let tmp = snap.cards.iter().find(|c| c.group == "*:tmp_low").expect("tmp");
    assert_eq!((tmp.urgency, tmp.delivery), (Some(Urgency::Urgent), Some(Delivery::Push)));
    assert_eq!(snap.digest.iter().map(|d| d.group.as_str()).collect::<Vec<_>>(), ["*:docker_foreign"]);
    let saved = Store::new(dir.join("data").join(NANTY_DIR)).load();
    assert_eq!(saved.cards.len(), 2, "gravado na pasta de dados");

    // Rodada sem novidade (um evento pede outra olhada): o estado e o registro não mudam.
    let state_file = dir.join("data").join(NANTY_DIR).join(store::STATE_FILE);
    let before = std::fs::read(&state_file).expect("estado");
    let lines = nanty.journal_tail(1000).len();
    st.events().publish(ApiEvent::new(EventType::ProjectStatus, Some("p1".into()), json!({"status": "idle"})));
    wait_for(|| nanty.scans() >= 2).await;
    assert_eq!(std::fs::read(&state_file).expect("estado"), before, "rodada sem novidade não grava");
    assert_eq!(nanty.journal_tail(1000).len(), lines);

    // Aprovado pela tela normal e pausado por orçamento: o plano se resolve e o orçamento avisa.
    write_project(&root, "IN_PROGRESS", ("BLOCKED", Some("budget")));
    st.events().publish(ApiEvent::new(EventType::TaskUpdated, Some("p1".into()), json!({"status": "BLOCKED"})));
    wait_for(|| nanty.scans() >= 3).await;
    let snap = nanty.snapshot();
    assert!(snap.cards.iter().all(|c| c.group != "p1:plan_waiting"), "o cartão do plano fechou");
    let budget = snap.cards.iter().find(|c| c.group == "p1:budget").expect("orçamento");
    assert_eq!((&budget.params["task_id"], &budget.params["attempts"]), (&json!("T01"), &json!(2)));
    assert!(snap.done.iter().any(|d| d.result == RESOLVED && d.code == "nanty.why.plan_waiting"));

    // Marco pelo evento: a tarefa integrada vai ao resumo.
    st.events().publish(ApiEvent::new(
        EventType::TaskUpdated,
        Some("p1".into()),
        json!({"spec_id": "001-demo", "task_id": "T01", "status": "DONE", "integrated": true}),
    ));
    wait_for(|| nanty.snapshot().digest.iter().any(|d| d.group == "p1:task_delivered")).await;

    // Projeto excluído da observação: o que estava aberto dele se resolve na próxima olhada.
    st.config().write().await.nanty.excluded_projects.push("p1".into());
    st.events().publish(ApiEvent::new(EventType::ProjectStatus, Some("p1".into()), json!({"status": "idle"})));
    wait_for(|| nanty.snapshot().cards.iter().all(|c| c.project_id.is_none())).await;

    st.trigger_shutdown();
    running.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn removing_a_project_drops_its_conditions_and_digest() {
    let cfg = NantyConfig { filter: crate::config::NantyFilter { group_wait_secs: 0, ..Default::default() }, ..Default::default() };
    let mut st = NantyState::default();
    apply(&mut st, &[plan("p1"), plan("p10"), Signal::new(SignalKind::CostAlert, Some("p1")).fact("count", 1)], &PROJECT_KINDS, &cfg, noon_plus(0));
    assert!(st.forget_project("p1"));
    assert_eq!(st.alerts.keys().collect::<Vec<_>>(), ["p10:plan_waiting"]);
    assert!(st.digest.is_empty());
    let mut other = NantyState::default();
    apply(&mut other, &[plan("p1")], &PROJECT_KINDS, &cfg, noon_plus(0));
    assert!(other.retain_projects(&[]));
    assert!(other.alerts.is_empty());
}
