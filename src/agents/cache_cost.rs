//! Custo do cache do provedor na decisão de compactar o histórico (spec 133 T04, ajuste C-01).
//!
//! Trocar uma saída antiga por uma nota tira `R` tokens de todas as chamadas seguintes, mas invalida
//! o cache de prefixo do primeiro item alterado até o fim (`I` tokens), que a próxima chamada paga de
//! novo: pela escrita de cache, quando o provedor a cobra, ou pela entrada normal. A compactação só
//! compensa quando
//!
//! `R x p_leitura x H >= I x (p_escrita_ou_entrada - p_leitura)`,
//!
//! com `H` o horizonte em chamadas (o menor entre os passos restantes e as chamadas até o limiar do
//! resumo). Os preços vêm do catálogo pelo modelo e pelo tipo da conexão, sem nome de modelo no
//! código; sem preço (o plano ChatGPT, por exemplo), vale a regra de antes, só pelo tamanho.

use crate::config::Pricing;

/// Preços (US$ por 1 M tokens) que decidem se uma compactação compensa.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CachePrices {
    /// Entrada lida do cache (sem preço de cache: a entrada normal, ou seja, sem desconto).
    pub read: f64,
    /// Entrada que volta a ser cobrada depois de invalidada: a escrita de cache quando o provedor a
    /// cobra, senão a entrada normal.
    pub rewrite: f64,
}

impl CachePrices {
    /// Preços de cache de uma tabela de preços. `None` quando a entrada não tem preço válido.
    pub fn from_pricing(pricing: &Pricing) -> Option<Self> {
        let valid = |p: f64| p.is_finite() && p >= 0.0;
        let input = pricing.input_per_mtok;
        if !valid(input) || input == 0.0 {
            return None;
        }
        let read = pricing.cached_input_per_mtok.filter(|p| valid(*p)).unwrap_or(input);
        let rewrite = pricing.cache_write_per_mtok.filter(|p| valid(*p)).unwrap_or(input);
        Some(Self { read, rewrite })
    }

    /// A compactação que tira `removed` tokens e invalida `invalidated` tokens de prefixo compensa
    /// num horizonte de `horizon` chamadas? Com o cache frio (`warm = false`), a invalidação não
    /// custa nada: a próxima chamada já pagaria o prefixo inteiro.
    pub fn worth(&self, removed: usize, invalidated: usize, horizon: usize, warm: bool) -> bool {
        if !warm || invalidated == 0 {
            return removed > 0;
        }
        let saved = tokens(removed) * self.read * tokens(horizon);
        let cost = tokens(invalidated) * (self.rewrite - self.read).max(0.0);
        saved >= cost
    }
}

/// Contagem como `f64` (as contagens daqui ficam muito abaixo da perda de precisão).
fn tokens(n: usize) -> f64 {
    u32::try_from(n).map_or(f64::from(u32::MAX), f64::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PriceSource;

    fn pricing(input: f64, cached: Option<f64>, write: Option<f64>) -> Pricing {
        Pricing {
            input_per_mtok: input,
            output_per_mtok: 5.0,
            cached_input_per_mtok: cached,
            cache_write_per_mtok: write,
            web_search_per_call: None,
            image_per_unit: None,
            source: PriceSource::Estimated,
            estimated_at: None,
        }
    }

    #[test]
    fn prices_fall_back_to_the_input_price() {
        let p = CachePrices::from_pricing(&pricing(1.0, Some(0.1), Some(1.25))).expect("preço");
        assert_eq!(p, CachePrices { read: 0.1, rewrite: 1.25 });
        let no_write = CachePrices::from_pricing(&pricing(1.0, Some(0.25), None)).expect("preço");
        assert_eq!(no_write, CachePrices { read: 0.25, rewrite: 1.0 }, "sem escrita cobrada: a entrada");
        let no_cache = CachePrices::from_pricing(&pricing(2.0, None, None)).expect("preço");
        assert_eq!(no_cache, CachePrices { read: 2.0, rewrite: 2.0 });
        assert_eq!(CachePrices::from_pricing(&pricing(0.0, Some(0.0), None)), None, "sem preço");
        assert_eq!(CachePrices::from_pricing(&pricing(f64::NAN, None, None)), None);
    }

    /// Os casos do Test 10 (escrita a 1,25x, leitura a 0,1x): tirar 241 tokens e regravar 36 mil
    /// não compensa; tirar 10 mil de um prefixo de 6 mil, num horizonte de 10 chamadas, compensa.
    #[test]
    fn small_removal_with_large_prefix_does_not_pay() {
        let p = CachePrices { read: 0.1, rewrite: 1.25 };
        assert!(!p.worth(241, 36_115, 10, true));
        assert!(p.worth(10_000, 6_000, 10, true));
        assert!(!p.worth(10_000, 6_000, 1, true), "horizonte curto");
        assert!(p.worth(241, 36_115, 10, false), "cache frio: invalidar não custa");
        assert!(!p.worth(0, 0, 10, true), "nada sai, nada a fazer");
    }

    #[test]
    fn without_cache_discount_compaction_always_pays() {
        let p = CachePrices { read: 2.0, rewrite: 2.0 };
        assert!(p.worth(1, 1_000_000, 1, true));
    }
}
