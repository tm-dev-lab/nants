//! Testes do limitador de ritmo (spec 089 T02), com relógio manual: nada dorme de verdade.

use super::*;
use reqwest::header::HeaderValue;

/// Relógio manual: só anda quando o teste manda.
#[derive(Default)]
struct ManualClock(Mutex<Duration>);

impl ManualClock {
    fn advance(&self, d: Duration) {
        let mut now = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *now = now.saturating_add(d);
    }
}

impl PaceClock for ManualClock {
    fn now(&self) -> Duration {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn pacer() -> (Arc<Pacer>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::default());
    (Pacer::new(Arc::clone(&clock) as Arc<dyn PaceClock>), clock)
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

const C: &str = "conn";
const M: &str = "m";

/// Reserva e marca como enviada; devolve a espera.
fn go(p: &Arc<Pacer>, est: u64) -> Duration {
    let (mut ticket, wait) = p.reserve(C, M, est);
    ticket.sent();
    wait
}

fn quota(pairs: &[(&'static str, &'static str)]) -> Option<Quota> {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(*k, HeaderValue::from_static(v));
    }
    Quota::from_headers(&h)
}

#[test]
fn reset_formats_are_parsed_and_invalid_values_ignored() {
    assert_eq!(parse_reset("6m0s"), Some(secs(360)));
    assert_eq!(parse_reset("1s"), Some(secs(1)));
    assert_eq!(parse_reset("20ms"), Some(Duration::from_millis(20)));
    assert_eq!(parse_reset("1.5s"), Some(Duration::from_millis(1500)));
    assert_eq!(parse_reset("30"), Some(secs(30)));
    assert_eq!(parse_reset(" 1h2m3s "), Some(secs(3723)));
    for bad in ["", "abc", "-1s", "1x", "s", "1.2.3s", "inf", "NaN", "1e5s", "99999999999999999999999s", "5 s"] {
        assert_eq!(parse_reset(bad), None, "{bad}");
    }
}

#[test]
fn quota_headers_are_read_and_invalid_ones_ignored() {
    let q = quota(&[
        ("x-ratelimit-remaining-requests", "0"),
        ("x-ratelimit-remaining-tokens", "1200"),
        ("x-ratelimit-reset-requests", "6m0s"),
        ("x-ratelimit-reset-tokens", "20ms"),
        ("x-ratelimit-limit-requests", "60"),
        ("x-ratelimit-limit-tokens", "90000"),
    ])
    .expect("cota");
    assert_eq!(q.remaining_requests, Some(0));
    assert_eq!(q.remaining_tokens, Some(1200));
    assert_eq!(q.reset_requests, Some(secs(360)));
    assert_eq!(q.reset_tokens, Some(Duration::from_millis(20)));
    assert_eq!(q.limit_requests, Some(60));
    assert_eq!(q.limit_tokens, Some(90_000));
    let bad = quota(&[
        ("x-ratelimit-remaining-requests", "-3"),
        ("x-ratelimit-reset-requests", "logo"),
        ("x-ratelimit-limit-tokens", "0"),
        ("x-ratelimit-limit-requests", "99999999999999999999999"),
    ]);
    assert_eq!(bad, None);
    assert_eq!(quota(&[]), None);
}

#[test]
fn without_any_signal_there_is_never_a_wait() {
    let (p, clock) = pacer();
    for _ in 0..200 {
        assert_eq!(go(&p, 50_000), Duration::ZERO);
    }
    clock.advance(secs(1));
    assert_eq!(go(&p, 1), Duration::ZERO);
    assert_eq!(p.ceilings(C, M), (None, None));
}

#[test]
fn exhausted_remaining_waits_for_the_reset_with_a_60_s_cap() {
    let (p, clock) = pacer();
    p.observe(C, M, &quota(&[("x-ratelimit-remaining-requests", "5"), ("x-ratelimit-reset-requests", "20s")]).expect("q"));
    assert_eq!(go(&p, 10), Duration::ZERO, "restante folgado");
    p.observe(C, M, &quota(&[("x-ratelimit-remaining-requests", "1"), ("x-ratelimit-reset-requests", "20s")]).expect("q"));
    assert_eq!(go(&p, 10), secs(20), "restante ≤ 1 espera o reset");
    clock.advance(secs(20));
    p.observe(C, M, &quota(&[("x-ratelimit-remaining-requests", "0"), ("x-ratelimit-reset-requests", "6m0s")]).expect("q"));
    assert_eq!(go(&p, 10), secs(60), "teto de 60 s");
    // Tokens: o restante menor que o custo estimado espera o reset dos tokens.
    let (p, clock) = pacer();
    p.observe(C, M, &quota(&[("x-ratelimit-remaining-tokens", "300"), ("x-ratelimit-reset-tokens", "1.5s")]).expect("q"));
    assert_eq!(go(&p, 200), Duration::ZERO);
    assert_eq!(go(&p, 200), Duration::from_millis(1500), "sobraram 100 tokens");
    // Depois do reset o portão some.
    clock.advance(secs(2));
    assert_eq!(go(&p, 200), Duration::ZERO);
}

#[test]
fn usage_corrects_the_estimated_cost() {
    let (p, _) = pacer();
    let (mut ticket, _) = p.reserve(C, M, 4000);
    ticket.sent();
    assert_eq!(p.window(C, M), vec![(Duration::ZERO, 4000)]);
    ticket.settle(130);
    drop(ticket);
    assert_eq!(p.window(C, M), vec![(Duration::ZERO, 130)], "a vaga fica com o custo real");
}

#[test]
fn ticket_dropped_before_sending_gives_the_slot_back() {
    let (p, _) = pacer();
    let (ticket, _) = p.reserve(C, M, 10);
    drop(ticket);
    assert!(p.window(C, M).is_empty());
}

#[test]
fn request_bigger_than_the_token_ceiling_passes_on_an_empty_window() {
    let (p, clock) = pacer();
    p.observe(C, M, &quota(&[("x-ratelimit-limit-tokens", "1000")]).expect("q"));
    assert_eq!(go(&p, 5000), Duration::ZERO, "janela vazia: passa");
    assert_eq!(go(&p, 10), secs(60), "o próximo espera o grande sair da janela");
    clock.advance(secs(121));
    assert_eq!(go(&p, 5000), Duration::ZERO);
}

#[test]
fn limit_learns_70_percent_of_the_last_minute() {
    let (p, clock) = pacer();
    // 10 pedidos, um por segundo, 100 tokens cada; o limite chega no 10º.
    for _ in 0..10 {
        assert_eq!(go(&p, 100), Duration::ZERO);
        clock.advance(secs(1));
    }
    p.on_limit(C, M);
    assert_eq!(p.ceilings(C, M), (Some(7), Some(700)));
    // Janela com 10 (em 0..9 s, agora 10 s): o próximo espera até sobrarem 6, quando o 4º sai (63 s).
    assert_eq!(go(&p, 100), secs(53));
    // Mínimo de 1 requisição/min.
    let (p, _) = pacer();
    go(&p, 1);
    p.on_limit(C, M);
    assert_eq!(p.ceilings(C, M), (Some(1), Some(1)));
}

#[test]
fn learned_ceilings_relax_every_5_minutes_and_are_discarded_above_double() {
    let (p, clock) = pacer();
    for _ in 0..10 {
        go(&p, 100);
    }
    p.on_limit(C, M);
    assert_eq!(p.ceilings(C, M), (Some(7), Some(700)));
    clock.advance(secs(299));
    assert_eq!(p.ceilings(C, M), (Some(7), Some(700)), "antes de 5 min nada muda");
    clock.advance(secs(1));
    assert_eq!(p.ceilings(C, M), (Some(9), Some(875)), "+25%");
    clock.advance(secs(300));
    assert_eq!(p.ceilings(C, M), (Some(12), Some(1094)));
    // Mais 3 passos: 15/1368, 19/1710, 24/2138 > o dobro do observado (10/1000): descartado.
    clock.advance(secs(600));
    assert_eq!(p.ceilings(C, M), (Some(19), Some(1710)));
    clock.advance(secs(300));
    assert_eq!(p.ceilings(C, M), (None, None));
    assert_eq!(go(&p, 100), Duration::ZERO);
}

#[test]
fn a_new_limit_recalculates_from_the_current_pace() {
    let (p, clock) = pacer();
    for _ in 0..10 {
        go(&p, 100);
    }
    p.on_limit(C, M);
    clock.advance(secs(240));
    for _ in 0..4 {
        go(&p, 50);
    }
    p.on_limit(C, M);
    assert_eq!(p.ceilings(C, M), (Some(2), Some(140)));
    clock.advance(secs(299));
    assert_eq!(p.ceilings(C, M), (Some(2), Some(140)), "o relógio do relaxamento recomeça");
}

#[test]
fn the_smaller_of_header_and_learned_ceiling_wins() {
    let (p, _) = pacer();
    for _ in 0..10 {
        go(&p, 100);
    }
    p.on_limit(C, M);
    p.observe(C, M, &quota(&[("x-ratelimit-limit-requests", "5"), ("x-ratelimit-limit-tokens", "99999")]).expect("q"));
    assert_eq!(p.ceilings(C, M), (Some(5), Some(700)));
    p.observe(C, M, &quota(&[("x-ratelimit-limit-requests", "100")]).expect("q"));
    assert_eq!(p.ceilings(C, M), (Some(7), Some(700)), "o cabeçalho sem tokens mantém o anterior");
}

#[test]
fn concurrent_reservations_keep_arrival_order_and_never_exceed_the_ceiling() {
    let (p, clock) = pacer();
    p.observe(C, M, &quota(&[("x-ratelimit-limit-requests", "3")]).expect("q"));
    clock.advance(secs(5));
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let p = Arc::clone(&p);
            std::thread::spawn(move || {
                let (mut ticket, wait) = p.reserve(C, M, 10);
                ticket.sent();
                (ticket.id, wait)
            })
        })
        .collect();
    let mut got: Vec<(u64, Duration)> = handles.into_iter().map(|h| h.join().expect("thread")).collect();
    got.sort();
    let waits: Vec<Duration> = got.iter().map(|(_, w)| *w).collect();
    // Ordem de chegada: quem reservou antes nunca sai depois.
    assert!(waits.windows(2).all(|w| w[0] <= w[1]), "{waits:?}");
    assert_eq!(waits, vec![Duration::ZERO, Duration::ZERO, Duration::ZERO, secs(60), secs(60), secs(60)]);
    // Nenhuma janela de 60 s com mais de 3 pedidos.
    let slots: Vec<Duration> = p.window(C, M).into_iter().map(|(at, _)| at).collect();
    for t in &slots {
        let inside = slots.iter().filter(|s| **s <= *t && s.saturating_add(PACE_WINDOW) > *t).count();
        assert!(inside <= 3, "{slots:?}");
    }
}

#[test]
fn pairs_are_independent() {
    let (p, _) = pacer();
    go(&p, 10);
    p.on_limit(C, M);
    assert_eq!(p.ceilings(C, M), (Some(1), Some(7)));
    assert_eq!(p.ceilings(C, "outro"), (None, None));
    assert_eq!(p.ceilings("outra", M), (None, None));
    let (mut t, wait) = p.reserve("outra", M, 10);
    t.sent();
    assert_eq!(wait, Duration::ZERO);
}

#[test]
fn pace_notice_is_fixed_text() {
    assert_eq!(pace_notice(Duration::from_millis(14_200)), "ajustando o ritmo ao limite do provedor: 15 s");
}

/// Spec 102: o aviso volta aos segundos (a atividade o publica com `code`); outro texto não.
#[test]
fn pace_notice_secs_reads_back_the_notice() {
    assert_eq!(pace_notice_secs(&pace_notice(Duration::from_millis(14_200))), Some(15));
    assert_eq!(pace_notice_secs("ajustando o ritmo ao limite do provedor: x s"), None);
    assert_eq!(pace_notice_secs("lendo src/a.rs"), None);
}
