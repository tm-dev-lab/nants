//! Relógio do Nanty (spec 138 T01, C-15): a hora local vem de `libc::localtime_r` (o fuso do
//! sistema, sem `chrono` nem `time`) e é injetável nos testes. Usado no silêncio, no resumo da
//! manhã e na virada do gasto do dia.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Instante com a data e a hora locais.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    /// Milissegundos desde a época Unix (UTC).
    pub epoch_ms: u64,
    pub year: i32,
    /// 1 a 12.
    pub month: u8,
    /// 1 a 31.
    pub day: u8,
    /// 0 a 23.
    pub hour: u8,
    /// 0 a 59.
    pub minute: u8,
}

impl LocalTime {
    /// Minutos desde a meia-noite local.
    pub fn minute_of_day(&self) -> u16 {
        u16::from(self.hour) * 60 + u16::from(self.minute)
    }

    /// Dia local `AAAA-MM-DD`: a chave do gasto do dia.
    pub fn day_key(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// O minuto local cai na janela `[início, fim)` (minutos do dia). A janela pode passar da
    /// meia-noite (22:00 a 07:00); início igual ao fim = janela vazia.
    pub fn within(&self, (start, end): (u16, u16)) -> bool {
        let m = self.minute_of_day();
        match start.cmp(&end) {
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Less => m >= start && m < end,
            std::cmp::Ordering::Greater => m >= start || m < end,
        }
    }
}

/// Fonte da hora local (produção: [`system_clock`]; testes: [`fixed_clock`]).
pub type NantyClock = Arc<dyn Fn() -> LocalTime + Send + Sync>;

/// Hora local de `epoch_ms` pelo fuso do sistema (`localtime_r`). `None` se a conversão falhar.
pub fn local_time_at(epoch_ms: u64) -> Option<LocalTime> {
    let secs = libc::time_t::try_from(epoch_ms / 1000).ok()?;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `localtime_r` é reentrante e só escreve no `tm` que passamos; o retorno nulo
    // indica falha e, nesse caso, o `tm` não é lido.
    let ok = unsafe { !libc::localtime_r(&secs, tm.as_mut_ptr()).is_null() };
    if !ok {
        return None;
    }
    // SAFETY: inicializado com zeros e preenchido pelo `localtime_r` bem-sucedido.
    let tm = unsafe { tm.assume_init() };
    Some(LocalTime {
        epoch_ms,
        year: tm.tm_year + 1900,
        month: u8::try_from(tm.tm_mon + 1).ok()?,
        day: u8::try_from(tm.tm_mday).ok()?,
        hour: u8::try_from(tm.tm_hour).ok()?,
        minute: u8::try_from(tm.tm_min).ok()?,
    })
}

/// Milissegundos atuais desde a época Unix.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Relógio do sistema. Se o `localtime_r` falhar, cai na hora UTC (nunca entra em pânico).
pub fn system_clock() -> NantyClock {
    Arc::new(|| {
        let ms = now_ms();
        local_time_at(ms).unwrap_or_else(|| utc_time_at(ms))
    })
}

/// Hora UTC de `epoch_ms` (reserva do relógio do sistema), pelo algoritmo de dias civis.
fn utc_time_at(epoch_ms: u64) -> LocalTime {
    let secs = epoch_ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    LocalTime {
        epoch_ms,
        year: i32::try_from(year).unwrap_or(1970),
        month: u8::try_from(month).unwrap_or(1),
        day: u8::try_from(day).unwrap_or(1),
        hour: u8::try_from(rem / 3600).unwrap_or(0),
        minute: u8::try_from(rem % 3600 / 60).unwrap_or(0),
    }
}

/// Relógio parado num instante (testes).
pub fn fixed_clock(at: LocalTime) -> NantyClock {
    Arc::new(move || at)
}

/// Relógio dos testes que correm em tempo real (prazos, filas e esperas seguem o `epoch_ms` de
/// verdade), mas com a data e a hora locais fixas: fora do silêncio inicial (22:00 a 07:00), longe
/// do resumo (08:00) e da virada do dia. Assim o resultado não depende da hora em que a suíte roda.
#[cfg(test)]
pub(crate) fn test_clock() -> NantyClock {
    Arc::new(|| at((2026, 10, 30), 12, 0, now_ms()))
}

#[cfg(test)]
pub(crate) fn at(day: (i32, u8, u8), hour: u8, minute: u8, epoch_ms: u64) -> LocalTime {
    LocalTime { epoch_ms, year: day.0, month: day.1, day: day.2, hour, minute }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_handles_midnight_and_empty() {
        let t = |h, m| at((2026, 10, 30), h, m, 0);
        let night = (22 * 60, 7 * 60);
        assert!(t(22, 0).within(night) && t(23, 59).within(night) && t(0, 0).within(night) && t(6, 59).within(night));
        assert!(!t(7, 0).within(night) && !t(12, 0).within(night) && !t(21, 59).within(night));
        let day = (9 * 60, 17 * 60);
        assert!(t(9, 0).within(day) && !t(17, 0).within(day) && !t(8, 59).within(day));
        assert!(!t(10, 0).within((600, 600)), "início igual ao fim = sem silêncio");
        assert_eq!(t(8, 5).day_key(), "2026-10-30");
        assert_eq!(t(8, 5).minute_of_day(), 485);
    }

    #[test]
    fn injected_clock_is_what_callers_see() {
        let clock = fixed_clock(at((2026, 1, 2), 3, 4, 5));
        assert_eq!(clock().day_key(), "2026-01-02");
        assert_eq!(clock().epoch_ms, 5);
    }

    #[test]
    fn localtime_r_gives_a_plausible_local_time() {
        // 2026-10-30 12:00:00 UTC: qualquer fuso (UTC-12 a UTC+14) fica no dia 29, 30 ou 31.
        let ms = 1_793_361_600_000;
        let local = local_time_at(ms).expect("localtime_r");
        assert_eq!((local.year, local.month), (2026, 10));
        assert!((29..=31).contains(&local.day), "{local:?}");
        assert!(local.hour < 24 && local.minute < 60);
        let utc = utc_time_at(ms);
        assert_eq!((utc.year, utc.month, utc.day, utc.hour, utc.minute), (2026, 10, 30, 12, 0));
        assert_eq!(utc_time_at(0).day_key(), "1970-01-01");
        let sys = system_clock()();
        assert!(sys.epoch_ms > ms - 400 * 86_400_000, "relógio do sistema plausível");
    }
}
