//! Código de pareamento do Telegram (spec 138 T06, C-11), no padrão do código de dispositivo de
//! `server::devices`:
//! - só com a janela aberta pela pessoa na tela ([`PAIR_WINDOW_MS`], 10 minutos);
//! - o código troca a cada [`PAIR_ROTATE_MS`]. É mais que o minuto do dispositivo porque o código
//!   passa pelo link `t.me/<bot>?start=<código>`: a pessoa toca no link, o Telegram abre e ela ainda
//!   toca em "Iniciar";
//! - uso único: o acerto fecha a janela;
//! - [`MAX_PAIR_ERRORS`] erros (os mesmos 5 do dispositivo) travam o código até a pessoa gerar outro;
//! - o código tem 16 caracteres de `A-Za-z0-9_-` (96 bits, o alfabeto do parâmetro `start`, que
//!   aceita até 64) e nunca vai para o registro nem para o terminal.

use crate::server::devices::{ct_eq, MAX_CODE_ERRORS};

/// Duração máxima da janela de pareamento (10 minutos).
pub const PAIR_WINDOW_MS: u64 = 10 * 60 * 1000;
/// O código troca a cada 2 minutos.
pub const PAIR_ROTATE_MS: u64 = 2 * 60 * 1000;
/// Erros por janela antes de travar (os mesmos do dispositivo).
pub const MAX_PAIR_ERRORS: u32 = MAX_CODE_ERRORS;
/// Tamanho do código.
pub const CODE_CHARS: usize = 16;
/// Alfabeto do código: 64 símbolos, então um byte aleatório módulo 64 não tem viés.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";

/// Código novo do gerador seguro do sistema (`None` se ele falhar).
fn new_code() -> Option<String> {
    let mut buf = [0u8; CODE_CHARS];
    getrandom::getrandom(&mut buf).ok()?;
    Some(buf.iter().map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()])).collect())
}

/// O código tem a forma certa (o que chega do Telegram é conferido antes de comparar).
pub fn valid_code(code: &str) -> bool {
    code.len() == CODE_CHARS && code.bytes().all(|b| ALPHABET.contains(&b))
}

struct Window {
    expires_at: u64,
    code: String,
    code_since: u64,
    errors: u32,
}

/// Estado da janela para a tela.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairState {
    Closed,
    /// Travada por erros: só um código novo, gerado na tela, reabre.
    Locked,
    Open { code: String, seconds_to_next: u64, expires_in: u64 },
}

/// Resultado de uma tentativa (o `/start <código>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairAttempt {
    Paired,
    Invalid { attempts_left: u32 },
    Locked,
    Inactive,
}

/// A janela de pareamento (em memória; some num reinício).
#[derive(Default)]
pub struct Pairing {
    window: Option<Window>,
    locked: bool,
}

impl Pairing {
    /// Abre (ou reabre) a janela: código novo, erros zerados. `None` sem gerador aleatório.
    pub fn open(&mut self, now: u64) -> Option<PairState> {
        let code = new_code()?;
        self.window = Some(Window { expires_at: now.saturating_add(PAIR_WINDOW_MS), code, code_since: now, errors: 0 });
        self.locked = false;
        Some(self.state(now))
    }

    /// Fecha a janela e a trava (pareou, despareou ou trocou o token).
    pub fn close(&mut self) {
        self.window = None;
        self.locked = false;
    }

    /// Troca o código vencido e fecha a janela vencida.
    fn rotate(&mut self, now: u64) {
        if self.window.as_ref().is_some_and(|w| now >= w.expires_at) {
            self.window = None;
        }
        let mut failed = false;
        if let Some(w) = self.window.as_mut() {
            if now >= w.code_since.saturating_add(PAIR_ROTATE_MS) {
                let steps = (now - w.code_since) / PAIR_ROTATE_MS;
                w.code_since += steps * PAIR_ROTATE_MS;
                match new_code() {
                    Some(code) => w.code = code,
                    None => failed = true,
                }
            }
        }
        if failed {
            self.window = None;
        }
    }

    /// Estado atual (troca o código se passou a vez).
    pub fn state(&mut self, now: u64) -> PairState {
        self.rotate(now);
        if self.locked {
            return PairState::Locked;
        }
        match &self.window {
            None => PairState::Closed,
            Some(w) => {
                let next = w.code_since.saturating_add(PAIR_ROTATE_MS).min(w.expires_at);
                PairState::Open {
                    code: w.code.clone(),
                    seconds_to_next: next.saturating_sub(now).div_ceil(1000),
                    expires_in: w.expires_at.saturating_sub(now).div_ceil(1000),
                }
            }
        }
    }

    /// Confere o código recebido. O acerto fecha a janela (uso único).
    pub fn attempt(&mut self, code: &str, now: u64) -> PairAttempt {
        if self.locked {
            return PairAttempt::Locked;
        }
        self.rotate(now);
        let Some(window) = self.window.as_mut() else {
            return PairAttempt::Inactive;
        };
        if !ct_eq(window.code.as_bytes(), code.trim().as_bytes()) {
            window.errors += 1;
            let errors = window.errors;
            if errors >= MAX_PAIR_ERRORS {
                self.window = None;
                self.locked = true;
            }
            return PairAttempt::Invalid { attempts_left: MAX_PAIR_ERRORS.saturating_sub(errors) };
        }
        self.close();
        PairAttempt::Paired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(p: &mut Pairing, now: u64) -> String {
        match p.state(now) {
            PairState::Open { code, .. } => code,
            other => panic!("janela fechada: {other:?}"),
        }
    }

    #[test]
    fn code_has_the_start_alphabet_and_is_single_use() {
        let mut p = Pairing::default();
        assert_eq!(p.state(0), PairState::Closed);
        assert_eq!(p.attempt("x", 0), PairAttempt::Inactive);
        p.open(0).expect("abre");
        let c = code(&mut p, 0);
        assert!(valid_code(&c) && c.len() <= 64, "{c}");
        assert_eq!(p.attempt(&c, 1), PairAttempt::Paired);
        assert_eq!(p.attempt(&c, 2), PairAttempt::Inactive, "uso único");
    }

    #[test]
    fn code_rotates_and_window_expires() {
        let mut p = Pairing::default();
        p.open(0).expect("abre");
        let first = code(&mut p, 0);
        let second = code(&mut p, PAIR_ROTATE_MS);
        assert_ne!(first, second, "o código troca");
        assert_eq!(p.attempt(&first, PAIR_ROTATE_MS + 1), PairAttempt::Invalid { attempts_left: MAX_PAIR_ERRORS - 1 });
        assert_eq!(p.state(PAIR_WINDOW_MS), PairState::Closed, "a janela vence");
    }

    #[test]
    fn five_errors_lock_until_a_new_code() {
        let mut p = Pairing::default();
        p.open(0).expect("abre");
        let good = code(&mut p, 0);
        for i in 1..MAX_PAIR_ERRORS {
            assert_eq!(p.attempt("errado", 0), PairAttempt::Invalid { attempts_left: MAX_PAIR_ERRORS - i });
        }
        assert_eq!(p.attempt("errado", 0), PairAttempt::Invalid { attempts_left: 0 });
        assert_eq!(p.state(0), PairState::Locked);
        assert_eq!(p.attempt(&good, 0), PairAttempt::Locked, "nem o certo passa depois da trava");
        p.open(0).expect("reabre");
        let fresh = code(&mut p, 0);
        assert_eq!(p.attempt(&fresh, 0), PairAttempt::Paired);
    }
}
