//! Progresso real de um comando em curso (spec 123 T01): quantos bytes de saída já foram
//! capturados e a última linha não vazia. É o sinal de vida que o heartbeat leva à tela, igual
//! para qualquer comando e stack (nada específico de ferramenta).
//!
//! Quem captura a saída (o `exec` do harness, o executor do Compose, o runner dos testes) chama
//! [`CommandProgress::push`] a cada pedaço lido; quem emite o heartbeat lê
//! [`CommandProgress::snapshot`]. A captura do resultado não muda: isto é só uma cópia do ritmo.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use tokio::sync::Notify;

/// Teto da linha em formação guardada. Uma linha que passa disso nunca é exibida: cortada, ela
/// poderia começar no meio de um segredo que a redação não reconhece.
const MAX_PARTIAL_BYTES: usize = 4 * 1024;

/// Retrato do progresso: bytes já capturados e a última linha não vazia (sem controles).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub bytes: u64,
    pub last_line: Option<String>,
}

#[derive(Debug, Default)]
struct Inner {
    bytes: u64,
    /// Linha ainda sem fim (`\n` ou `\r`), em bytes crus. Nunca vai para a tela: um pedaço de
    /// leitura pode terminar no meio de um segredo.
    partial: Vec<u8>,
    /// A linha em formação passou de [`MAX_PARTIAL_BYTES`]: ao fechar, é descartada.
    overflowed: bool,
    /// Última linha completa não vazia, já limpa.
    last: Option<String>,
}

/// Progresso compartilhado entre a captura (threads ou tarefas) e o heartbeat.
///
/// Spec 123 T02: o progresso "com largada" ([`CommandProgress::gated`]) só conta como começado
/// quando quem executa chama [`CommandProgress::mark_started`], depois da autorização (pedido de
/// aprovação) e de preparos com limite próprio (download da imagem do navegador). Assim o tempo e o
/// limite mostrados valem só para o trecho que o limite cobre.
#[derive(Debug, Default)]
pub struct CommandProgress {
    inner: Mutex<Inner>,
    /// `true` no progresso com largada.
    gated: bool,
    started: AtomicBool,
    start: Notify,
}

impl CommandProgress {
    /// Progresso que já conta como começado (quem rastreia não espera a largada).
    pub fn new() -> Self {
        Self::default()
    }

    /// Progresso que só começa em [`Self::mark_started`].
    pub fn gated() -> Self {
        Self { gated: true, ..Self::default() }
    }

    /// O processo do comando vai começar agora (o limite de tempo passa a valer). Idempotente.
    pub fn mark_started(&self) {
        if !self.started.swap(true, Ordering::SeqCst) {
            self.start.notify_waiters();
        }
    }

    /// `true` quando o comando já começou (ou o progresso não tem largada).
    pub fn has_started(&self) -> bool {
        !self.gated || self.started.load(Ordering::SeqCst)
    }

    /// Termina quando o comando começa (na hora, sem largada).
    pub async fn wait_started(&self) {
        loop {
            let notified = self.start.notified();
            if self.has_started() {
                return;
            }
            notified.await;
        }
    }

    /// Soma um pedaço da saída (stdout ou stderr, como chegou).
    pub fn push(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        st.bytes = st.bytes.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        // `\r` também fecha a linha: barras de progresso reescrevem a mesma linha.
        for segment in data.split_inclusive(|b| *b == b'\n' || *b == b'\r') {
            let closed = segment.last().is_some_and(|b| *b == b'\n' || *b == b'\r');
            let body = if closed { &segment[..segment.len() - 1] } else { segment };
            if !st.overflowed {
                st.partial.extend_from_slice(body);
                if st.partial.len() > MAX_PARTIAL_BYTES {
                    st.partial.clear();
                    st.overflowed = true;
                }
            }
            if closed {
                let line = if st.overflowed { String::new() } else { clean_line(&st.partial) };
                st.partial.clear();
                st.overflowed = false;
                if !line.is_empty() {
                    st.last = Some(line);
                }
            }
        }
    }

    /// Bytes até agora e a última linha COMPLETA não vazia. A linha em formação fica de fora: só a
    /// linha inteira passa pela redação de segredos com segurança.
    pub fn snapshot(&self) -> ProgressSnapshot {
        let st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        ProgressSnapshot { bytes: st.bytes, last_line: st.last.clone() }
    }
}

/// Texto visível de uma linha: sem sequências ANSI (cores, cursor) nem caracteres de controle,
/// espaços colapsados nas pontas. UTF-8 inválido vira o caractere de substituição.
fn clean_line(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI (`ESC [ ... final`) ou OSC (`ESC ] ... BEL|ESC \`); outros: só o ESC e o seguinte.
            match chars.next() {
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' || (n == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if c == '\t' {
            out.push(' ');
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_bytes_and_keeps_last_non_empty_line() {
        let p = CommandProgress::new();
        assert_eq!(p.snapshot(), ProgressSnapshot::default());
        let (a, b): (&[u8], &[u8]) = (b"Running 5 tests using 1 worker\n\n", b"  ok 1 home.spec.ts:3 abre\n   \n");
        p.push(a);
        p.push(b);
        let snap = p.snapshot();
        assert_eq!(snap.bytes, (a.len() + b.len()) as u64);
        assert_eq!(snap.last_line.as_deref(), Some("ok 1 home.spec.ts:3 abre"));
    }

    #[test]
    fn only_complete_lines_and_carriage_return_progress() {
        let p = CommandProgress::new();
        p.push(b"baixando 10%\rbaixando 55%\rbaix");
        assert_eq!(p.snapshot().last_line.as_deref(), Some("baixando 55%"), "a linha em formação fica de fora");
        p.push(b"ando 90%");
        assert_eq!(p.snapshot().last_line.as_deref(), Some("baixando 55%"));
        p.push(b"\n");
        assert_eq!(p.snapshot().last_line.as_deref(), Some("baixando 90%"));
    }

    /// Um segredo partido entre dois pedaços de leitura nunca aparece pela metade: só a linha
    /// inteira chega ao snapshot, e a redação dela pega o segredo completo.
    #[test]
    fn secret_split_between_chunks_never_shows_a_fragment() {
        let secret = "SEGREDO-de-teste-1234";
        let scrub = |line: &str| line.replace(secret, "***");
        let p = CommandProgress::new();
        p.push(b"passo 1\nusando chave SEGREDO-de");
        let mid = p.snapshot().last_line.unwrap_or_default();
        assert_eq!(scrub(&mid), "passo 1");
        assert!(!mid.contains("SEGREDO"), "{mid}");
        p.push(b"-teste-1234 no deploy\n");
        assert_eq!(scrub(&p.snapshot().last_line.unwrap_or_default()), "usando chave *** no deploy");
    }

    #[test]
    fn strips_ansi_and_control_characters() {
        let p = CommandProgress::new();
        p.push(b"\x1b[32m\xe2\x9c\x93\x1b[39m  \x1b]8;;http://x\x07link\x1b]8;;\x1b\\ fim\x07\n");
        assert_eq!(p.snapshot().last_line.as_deref(), Some("\u{2713}  link fim"));
    }

    /// Linha acima do teto não é exibida (cortada, poderia começar no meio de um segredo); a
    /// anterior continua e a seguinte volta ao normal.
    #[test]
    fn huge_line_is_never_shown() {
        let p = CommandProgress::new();
        p.push(b"antes\n");
        p.push(&vec![b'a'; 10_000]);
        p.push(b"FIM\n");
        let snap = p.snapshot();
        assert_eq!(snap.last_line.as_deref(), Some("antes"));
        assert_eq!(snap.bytes, 6 + 10_000 + 4);
        p.push(b"depois\n");
        assert_eq!(p.snapshot().last_line.as_deref(), Some("depois"));
    }
}
