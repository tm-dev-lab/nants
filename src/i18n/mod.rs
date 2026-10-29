//! Idiomas do servidor e do terminal (spec 102): pt-BR e en.
//!
//! - [`Lang`]: o idioma ativo. A interface manda o dela a cada turno (cabeçalho `X-Nants-Lang`,
//!   ver `server::lang`); o terminal decide pela ordem POSIX `LC_ALL` > `LC_MESSAGES` > `LANG`
//!   ([`Lang::from_locale_vars`]). Sem informação, pt-BR (compatível com o que já existia).
//! - Dicionários próprios ([`pt_br::MESSAGES`] e [`en::MESSAGES`]), com as mesmas chaves e os
//!   mesmos parâmetros nomeados (`{name}`); há teste de paridade.
//! - [`Msg`]: mensagem por chave e parâmetros, renderizada no idioma pedido só na hora de exibir.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

pub mod en;
pub mod pt_br;

/// Idioma suportado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Lang {
    /// Português do Brasil (padrão: sem idioma informado).
    #[default]
    PtBr,
    En,
}

impl Lang {
    /// Todos os idiomas suportados (spec 129 T04: os leitores de marcadores aceitam a forma de cada um).
    pub const ALL: [Lang; 2] = [Lang::PtBr, Lang::En];

    /// Etiqueta usada pela interface e pela API: `pt-BR` ou `en`.
    pub fn tag(self) -> &'static str {
        match self {
            Lang::PtBr => "pt-BR",
            Lang::En => "en",
        }
    }

    /// Idioma de uma etiqueta da interface (`pt-BR`, `en`); qualquer outro valor é `None`.
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag.trim() {
            "pt-BR" => Some(Lang::PtBr),
            "en" => Some(Lang::En),
            _ => None,
        }
    }

    /// Idioma de um valor de locale POSIX (`pt_BR.UTF-8`, `en_US`, `C`, `POSIX`): `pt` vira
    /// pt-BR; `C`, `POSIX` e o resto, en.
    pub fn from_locale(value: &str) -> Self {
        let primary = value
            .trim()
            .split(['_', '-', '.', '@'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if primary == "pt" {
            Lang::PtBr
        } else {
            Lang::En
        }
    }

    /// Ordem POSIX: o primeiro não vazio entre `LC_ALL`, `LC_MESSAGES` e `LANG` decide. Nenhum
    /// definido: en (como o locale `C`).
    pub fn from_locale_vars(lc_all: Option<&str>, lc_messages: Option<&str>, lang: Option<&str>) -> Self {
        [lc_all, lc_messages, lang]
            .into_iter()
            .flatten()
            .find(|v| !v.trim().is_empty())
            .map_or(Lang::En, Lang::from_locale)
    }

    /// Idioma do terminal pelo ambiente do processo.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Lang::from_locale_vars(var("LC_ALL").as_deref(), var("LC_MESSAGES").as_deref(), var("LANG").as_deref())
    }

    fn index(self) -> u8 {
        match self {
            Lang::PtBr => 0,
            Lang::En => 1,
        }
    }

    fn from_index(i: u8) -> Self {
        if i == 1 {
            Lang::En
        } else {
            Lang::PtBr
        }
    }

    fn dictionary(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Lang::PtBr => pt_br::MESSAGES,
            Lang::En => en::MESSAGES,
        }
    }
}

/// Idioma compartilhado e mutável: os agentes leem o valor atual a cada mensagem, então a troca
/// de idioma no meio de um projeto vale para as próximas mensagens. Clones compartilham o valor.
#[derive(Debug, Clone, Default)]
pub struct SharedLang(Arc<AtomicU8>);

impl SharedLang {
    pub fn new(lang: Lang) -> Self {
        Self(Arc::new(AtomicU8::new(lang.index())))
    }

    pub fn get(&self) -> Lang {
        Lang::from_index(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, lang: Lang) {
        self.0.store(lang.index(), Ordering::Relaxed);
    }
}

/// Texto da chave no idioma, ou `None` se a chave não existe.
pub fn lookup(lang: Lang, key: &str) -> Option<&'static str> {
    lang.dictionary().iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Formas da chave em todos os idiomas ([`Lang::ALL`]), sem repetição (spec 129 T04): quem lê um
/// marcador estrutural aceita qualquer uma, para os registros gravados em outro idioma continuarem
/// legíveis.
pub fn forms(key: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for lang in Lang::ALL {
        if let Some(text) = lookup(lang, key) {
            if !out.contains(&text) {
                out.push(text);
            }
        }
    }
    out
}

/// Texto da chave no idioma; chave ausente devolve a própria chave (a paridade é testada).
pub fn text(lang: Lang, key: &str) -> String {
    lookup(lang, key).map_or_else(|| key.to_string(), str::to_string)
}

/// Texto da chave com os parâmetros `{name}` preenchidos.
pub fn tr(lang: Lang, key: &str, params: &[(&str, &str)]) -> String {
    let template = lookup(lang, key).unwrap_or(key);
    interpolate(template, |name| params.iter().find(|(n, _)| *n == name).map(|(_, v)| (*v).to_string()))
}

/// Assunto de commit do Nants no repositório da pessoa (spec 115 T04, C-09): o prefixo convencional
/// (`feat(<spec>): T01 - `, `chore(<spec>): T01 - `, `docs: `...) fica fixo, porque o estado da
/// tarefa e da spec é lido por ele (`feat(<spec>): Txx - ` marca o DONE); só o texto depois dele
/// sai no idioma do projeto.
pub fn commit_subject(lang: Lang, prefix: &str, key: &str) -> String {
    format!("{prefix}{}", text(lang, key))
}

/// Forma do plural de `count` (spec 115 T01), a mesma regra da web (`pluralForm` em
/// `web/src/i18n/index.ts`): `one` só para 1 nos dois idiomas. O zero usa o plural também em pt-BR
/// ("0 arquivos"), decisão da 101/T01, embora o `Intl.PluralRules` trate 0 como singular em português.
pub fn plural_form(lang: Lang, count: u64) -> &'static str {
    match (lang, count) {
        (_, 1) => "one",
        (Lang::PtBr | Lang::En, _) => "other",
    }
}

/// Número inteiro com o separador de milhar do idioma, como o `Intl.NumberFormat` da web:
/// "1.234" em pt-BR e "1,234" em en.
pub fn format_count(lang: Lang, count: u64) -> String {
    let sep = match lang {
        Lang::PtBr => '.',
        Lang::En => ',',
    };
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (k, c) in digits.chars().enumerate() {
        if k > 0 && (digits.len() - k) % 3 == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// Plural simples (spec 115 T01): o texto de `base.one` ou `base.other` (ver [`plural_form`]),
/// com `{count}` já preenchido no formato do idioma e os demais `params` (que vencem, como na web).
pub fn tr_plural(lang: Lang, base: &str, count: u64, params: &[(&str, &str)]) -> String {
    let key = format!("{base}.{}", plural_form(lang, count));
    let template = lookup(lang, &key).unwrap_or(&key);
    let formatted = format_count(lang, count);
    interpolate(template, |name| {
        params
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| (*v).to_string())
            .or_else(|| (name == "count").then(|| formatted.clone()))
    })
}

/// Substitui `{name}` pelo valor de `value(name)`; nome desconhecido fica como está.
fn interpolate(template: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(after.len());
        let closed = name_len > 0 && after[name_len..].starts_with('}');
        match closed.then(|| value(&after[..name_len])).flatten() {
            Some(v) => {
                out.push_str(&v);
                rest = &after[name_len + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Valor de um parâmetro de [`Msg`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    /// Texto igual nos dois idiomas (número, caminho, versão).
    Text(String),
    /// Texto diferente por idioma (ex.: número com vírgula ou ponto decimal).
    PerLang { pt_br: String, en: String },
    /// Outra mensagem, renderizada no mesmo idioma.
    Msg(Box<Msg>),
    /// Lista de mensagens unidas por um separador.
    Join(Vec<Msg>, &'static str),
    /// Tamanho em bytes, exibido em GB com uma casa decimal ("1,5 GB" em pt-BR, "1.5 GB" em en).
    /// Na forma neutra ([`Msg::neutral`]) vai como o número de bytes, em `<nome>_bytes`.
    Gb(u64),
}

/// "12,3 GB" em pt-BR e "12.3 GB" em en (uma casa decimal, base 1024, sem arredondar para cima).
pub fn format_gb(lang: Lang, bytes: u64) -> String {
    let tenths = bytes.saturating_mul(10) / (1024 * 1024 * 1024);
    let sep = match lang {
        Lang::PtBr => ',',
        Lang::En => '.',
    };
    format!("{}{sep}{} GB", tenths / 10, tenths % 10)
}

/// Mensagem na forma neutra de idioma (spec 102 T05), para a interface montar o texto pelo próprio
/// dicionário: o `code` (a chave sem o prefixo), os `params` iguais nos dois idiomas e, se houver
/// uma lista ([`Arg::Join`]), os itens dela na mesma forma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neutral {
    pub code: String,
    /// Textos iguais nos dois idiomas e números ([`Arg::Gb`] em `<nome>_bytes`). Texto por idioma
    /// e sub-mensagem não têm forma neutra: ficam de fora, e a interface usa o texto de reserva.
    pub params: serde_json::Map<String, serde_json::Value>,
    /// Itens da primeira lista da mensagem, unidos pela interface.
    pub parts: Vec<Neutral>,
}

/// Mensagem por chave e parâmetros, renderizada no idioma na hora de exibir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msg {
    pub key: &'static str,
    pub args: Vec<(&'static str, Arg)>,
}

impl Msg {
    pub fn new(key: &'static str) -> Self {
        Self { key, args: Vec::new() }
    }

    /// Parâmetro de texto igual nos dois idiomas.
    pub fn arg(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.args.push((name, Arg::Text(value.into())));
        self
    }

    /// Parâmetro com outra mensagem.
    pub fn arg_msg(mut self, name: &'static str, value: Msg) -> Self {
        self.args.push((name, Arg::Msg(Box::new(value))));
        self
    }

    /// Parâmetro com um texto por idioma.
    pub fn arg_per_lang(mut self, name: &'static str, pt_br: impl Into<String>, en: impl Into<String>) -> Self {
        self.args.push((name, Arg::PerLang { pt_br: pt_br.into(), en: en.into() }));
        self
    }

    /// Parâmetro com uma lista de mensagens unidas por `sep`.
    pub fn arg_join(mut self, name: &'static str, items: Vec<Msg>, sep: &'static str) -> Self {
        self.args.push((name, Arg::Join(items, sep)));
        self
    }

    /// Texto no idioma.
    pub fn render(&self, lang: Lang) -> String {
        let template = lookup(lang, self.key).unwrap_or(self.key);
        interpolate(template, |name| {
            self.args.iter().find(|(n, _)| *n == name).map(|(_, arg)| match arg {
                Arg::Text(t) => t.clone(),
                Arg::PerLang { pt_br, en } => match lang {
                    Lang::PtBr => pt_br.clone(),
                    Lang::En => en.clone(),
                },
                Arg::Msg(m) => m.render(lang),
                Arg::Join(items, sep) => items.iter().map(|m| m.render(lang)).collect::<Vec<_>>().join(sep),
                Arg::Gb(bytes) => format_gb(lang, *bytes),
            })
        })
    }

    /// Parâmetro de tamanho em bytes (ver [`Arg::Gb`]).
    pub fn arg_gb(mut self, name: &'static str, bytes: u64) -> Self {
        self.args.push((name, Arg::Gb(bytes)));
        self
    }

    /// Forma neutra de idioma, com o `code` = a chave sem `prefix` (ver [`Neutral`]).
    pub fn neutral(&self, prefix: &str) -> Neutral {
        let code = self.key.strip_prefix(prefix).unwrap_or(self.key).to_string();
        let mut params = serde_json::Map::new();
        let mut parts = Vec::new();
        for (name, arg) in &self.args {
            match arg {
                Arg::Text(t) => {
                    params.insert((*name).to_string(), serde_json::Value::from(t.as_str()));
                }
                Arg::Gb(bytes) => {
                    params.insert(format!("{name}_bytes"), serde_json::Value::from(*bytes));
                }
                Arg::Join(items, _) if parts.is_empty() => parts = items.iter().map(|m| m.neutral(prefix)).collect(),
                _ => {}
            }
        }
        Neutral { code, params, parts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close) if after[..close].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && close > 0 => {
                    out.push(after[..close].to_string());
                    rest = &after[close + 1..];
                }
                _ => rest = after,
            }
        }
        out.sort();
        out
    }

    #[test]
    fn dictionaries_have_the_same_keys_and_params() {
        let pt: Vec<&str> = pt_br::MESSAGES.iter().map(|(k, _)| *k).collect();
        let en: Vec<&str> = en::MESSAGES.iter().map(|(k, _)| *k).collect();
        let mut sorted_pt = pt.clone();
        sorted_pt.sort_unstable();
        sorted_pt.dedup();
        assert_eq!(sorted_pt.len(), pt.len(), "chave repetida no pt-BR");
        let mut sorted_en = en.clone();
        sorted_en.sort_unstable();
        sorted_en.dedup();
        assert_eq!(sorted_en.len(), en.len(), "chave repetida no en");
        assert_eq!(sorted_pt, sorted_en, "pt-BR e en com chaves diferentes");
        for (key, value) in pt_br::MESSAGES {
            let other = lookup(Lang::En, key).unwrap_or_default();
            assert_eq!(params(value), params(other), "parâmetros diferentes em {key}");
            assert!(!value.trim().is_empty() && !other.trim().is_empty(), "texto vazio em {key}");
        }
    }

    /// Spec 113 T04 (sentinela): as mensagens do sistema que chegam à pessoa pela conversa, pelo
    /// relatório ou citadas ao modelo (runner, conselheiro, levantamento, relatório e rótulos da
    /// interface) não podem ter texto em pt no dicionário en, e o pt-BR precisa diferir do en.
    /// A spec 114 T02 acrescenta os motivos de bloqueio e de pausa (`runner.block.*`,
    /// `runner.budget.*`, `runs.*`) e os pedidos e aprovações automáticas (`approvals.*`).
    /// A spec 115 T01 estende a sentinela a todas as famílias de mensagens de pessoa: os erros da
    /// API (`serverError.`), as aprovações (`approval.`), as mensagens do sistema (`system.`), o
    /// terminal (`cli.`, `doctor.`, `uninstall.`) os dispositivos (`devices.`), a recuperação (`recovery.`) e as dúvidas do juiz (`allowlist.`, spec 115 T03); a T04 acrescenta os commits do Nants (`commit.`); a spec 116 T01, o log do login pelo plano ChatGPT (`chatgpt.`). Toda chave do
    /// dicionário precisa cair numa família, para nenhuma família nova escapar da sentinela.
    #[test]
    fn system_messages_have_no_portuguese_in_english() {
        // A spec 119 acrescenta o motivo da resposta ruim do provedor (`provider.`); a spec 124 T06, o
        // apontamento de tarefa grande da revisão do plano (`plan_review.`); a spec 125, os títulos do
        // ARCHITECTURE.md (`architecture.`); a spec 133 T08, os títulos livres da spec (`sdd.`); a spec 133
        // T11, as mensagens injetadas como turno do usuário na sessão de um modelo (`agent.`); a spec 134
        // T02, o motivo do estado de um documento da base de conhecimento (`kb.`).
        const PREFIXES: [&str; 24] = [
            "runner.", "advisor.", "survey.", "report.", "ui.", "approvals.", "runs.", "serverError.", "approval.", "system.", "cli.",
            "doctor.", "uninstall.", "devices.", "recovery.", "allowlist.", "commit.", "chatgpt.", "provider.", "plan_review.",
            "architecture.", "sdd.", "agent.", "kb.",
        ];
        const PT_LETTERS: &str = "ãõçáéíóúâêôàÃÕÇÁÉÍÓÚÂÊÔÀ";
        const PT_WORDS: [&str; 19] = [
            "testes", "checagem", "checagens", "não", "esgotado", "arquivos", "coberto", "limite", "nao", "falhou", "reaproveitado", "arvore",
            "verificação", "bloqueante", "executado", "pergunta", "resposta", "levantamento", "iniciar",
        ];
        for (key, _) in en::MESSAGES {
            assert!(PREFIXES.iter().any(|p| key.starts_with(p)), "chave fora das famílias da sentinela: {key}");
        }
        let system: Vec<&(&str, &str)> = en::MESSAGES.iter().filter(|(k, _)| PREFIXES.iter().any(|p| k.starts_with(p))).collect();
        assert!(system.len() >= 20, "chaves do sistema: {}", system.len());
        for (key, value) in system {
            assert!(!value.chars().any(|c| PT_LETTERS.contains(c)), "letra do pt em en {key}: {value}");
            let lower = value.to_lowercase();
            for word in PT_WORDS {
                assert!(!lower.split(|c: char| !c.is_alphanumeric()).any(|w| w == word), "palavra do pt em en {key}: {value}");
            }
            let pt = lookup(Lang::PtBr, key).unwrap_or_default();
            let neutral = SAME_IN_BOTH.iter().any(|(k, _)| k == key);
            assert!(neutral || pt != *value, "pt-BR igual ao en em {key}: {value}");
        }
        for (key, why) in SAME_IN_BOTH {
            assert!(why.len() > 10, "{key}: motivo curto");
            assert!(lookup(Lang::En, key).is_some(), "{key} não existe mais: tire da lista");
            assert_eq!(lookup(Lang::PtBr, key), lookup(Lang::En, key), "{key} não é mais igual nos dois: tire da lista");
        }
    }

    /// Chaves com o mesmo texto nos dois idiomas, com o motivo (a sentinela acima exige pt-BR
    /// diferente do en no resto).
    const SAME_IN_BOTH: [(&str, &str); 10] = [
        ("doctor.status.ok", "marca de status do doctor, igual nos dois"),
        ("doctor.status.info", "marca de status do doctor, igual nos dois"),
        ("doctor.label.git", "nome de ferramenta"),
        ("doctor.label.docker_cli", "nome de ferramenta"),
        ("doctor.label.docker_daemon", "nome de ferramenta"),
        ("doctor.label.docker_compose", "nome de ferramenta"),
        ("doctor.label.wsl", "nome de ferramenta"),
        ("doctor.detail.disk", "só a lista, montada por idioma nos itens"),
        ("report.traceability.commit", "id e hash do commit, sem texto traduzível"),
        ("architecture.section.stack", "título de seção do ARCHITECTURE.md: \"Stack\" é o mesmo termo nos dois idiomas"),
    ];

    /// Spec 115 T01: plural com `.one` e `.other` sempre juntos, nos dois dicionários, sem a chave
    /// base solta ao lado (quem chama usa [`tr_plural`]).
    #[test]
    fn plural_keys_come_in_pairs() {
        let mut bases = 0;
        for (name, dict) in [("pt-BR", pt_br::MESSAGES), ("en", en::MESSAGES)] {
            let has = |key: &str| dict.iter().any(|(k, _)| *k == key);
            for (key, _) in dict {
                let (base, other) = match (key.strip_suffix(".one"), key.strip_suffix(".other")) {
                    (Some(base), _) => (base, format!("{base}.other")),
                    (_, Some(base)) => (base, format!("{base}.one")),
                    _ => continue,
                };
                bases += 1;
                assert!(has(&other), "{name}: {key} sem {other}");
                assert!(!has(base), "{name}: {base} solta ao lado do plural");
            }
        }
        assert!(bases >= 8, "chaves de plural: {bases}");
    }

    #[test]
    fn plural_follows_the_web_rule_with_zero_in_the_plural() {
        for lang in [Lang::PtBr, Lang::En] {
            assert_eq!(plural_form(lang, 1), "one");
            for n in [0, 2, 5, 11, 21, 101, 1000] {
                assert_eq!(plural_form(lang, n), "other", "{lang:?} {n}");
            }
        }
        let applied = |lang, n| tr_plural(lang, "runner.format.applied", n, &[("names", "rustfmt")]);
        assert_eq!(applied(Lang::PtBr, 1), "formatação automática: 1 arquivo (rustfmt)");
        assert_eq!(applied(Lang::PtBr, 0), "formatação automática: 0 arquivos (rustfmt)");
        assert_eq!(applied(Lang::PtBr, 1234), "formatação automática: 1.234 arquivos (rustfmt)");
        assert_eq!(applied(Lang::En, 1), "automatic formatting: 1 file (rustfmt)");
        assert_eq!(applied(Lang::En, 3), "automatic formatting: 3 files (rustfmt)");
        assert_eq!(applied(Lang::En, 1234), "automatic formatting: 1,234 files (rustfmt)");
        assert_eq!(tr_plural(Lang::En, "runner.block.rejections", 1, &[]), "1 reviewer rejection");
        assert_eq!(tr_plural(Lang::PtBr, "runner.block.rejections", 3, &[]), "3 reprovações do revisor");
        // Parâmetro passado vence o `{count}` montado (como na web).
        assert_eq!(tr_plural(Lang::En, "runner.block.rejections", 2, &[("count", "x")]), "x reviewer rejections");
        assert_eq!(format_count(Lang::PtBr, 0), "0");
        assert_eq!(format_count(Lang::PtBr, 999), "999");
        assert_eq!(format_count(Lang::En, 1_000_000), "1,000,000");
        assert_eq!(tr_plural(Lang::En, "nao.existe", 2, &[]), "nao.existe.other");
    }

    #[test]
    fn locale_order_is_lc_all_then_lc_messages_then_lang() {
        assert_eq!(Lang::from_locale_vars(Some("pt_BR.UTF-8"), Some("en_US"), Some("en_US")), Lang::PtBr);
        assert_eq!(Lang::from_locale_vars(Some("C"), Some("pt_BR"), Some("pt_BR")), Lang::En);
        assert_eq!(Lang::from_locale_vars(None, Some("pt_PT.UTF-8"), Some("en_US")), Lang::PtBr);
        assert_eq!(Lang::from_locale_vars(Some(""), Some(" "), Some("pt_BR.UTF-8")), Lang::PtBr);
        assert_eq!(Lang::from_locale_vars(None, None, Some("en_GB.UTF-8")), Lang::En);
        assert_eq!(Lang::from_locale_vars(None, None, None), Lang::En);
    }

    #[test]
    fn locale_values_map_pt_to_pt_br_and_the_rest_to_en() {
        for pt in ["pt", "pt_BR", "pt_BR.UTF-8", "PT_br", "pt-BR", "pt@euro"] {
            assert_eq!(Lang::from_locale(pt), Lang::PtBr, "{pt}");
        }
        for other in ["C", "POSIX", "C.UTF-8", "en_US.UTF-8", "es_ES", "fr", "portuguese", ""] {
            assert_eq!(Lang::from_locale(other), Lang::En, "{other}");
        }
    }

    #[test]
    fn ui_tags_round_trip_and_reject_others() {
        for lang in [Lang::PtBr, Lang::En] {
            assert_eq!(Lang::from_tag(lang.tag()), Some(lang));
        }
        for bad in ["pt", "pt-PT", "EN", "fr", "", "en-US"] {
            assert_eq!(Lang::from_tag(bad), None, "{bad}");
        }
        assert_eq!(Lang::default(), Lang::PtBr);
    }

    #[test]
    fn shared_lang_is_seen_by_every_clone() {
        let a = SharedLang::default();
        let b = a.clone();
        assert_eq!(b.get(), Lang::PtBr);
        a.set(Lang::En);
        assert_eq!(b.get(), Lang::En);
        assert_eq!(SharedLang::new(Lang::En).get(), Lang::En);
    }

    #[test]
    fn tr_and_msg_fill_params_in_each_language() {
        assert_eq!(tr(Lang::PtBr, "cli.unknown_command", &[("command", "x")]), "comando desconhecido: x");
        assert_eq!(tr(Lang::En, "cli.unknown_command", &[("command", "x")]), "unknown command: x");
        assert_eq!(tr(Lang::En, "chave.inexistente", &[]), "chave.inexistente");
        let msg = Msg::new("doctor.detail.disk")
            .arg_join(
                "list",
                vec![
                    Msg::new("doctor.detail.disk_free").arg_per_lang("size", "1,5 GB", "1.5 GB").arg("path", "/a"),
                    Msg::new("doctor.detail.disk_free").arg_per_lang("size", "2,0 GB", "2.0 GB").arg("path", "/b"),
                ],
                "; ",
            );
        assert_eq!(msg.render(Lang::PtBr), "1,5 GB livres em /a; 2,0 GB livres em /b");
        assert_eq!(msg.render(Lang::En), "1.5 GB free in /a; 2.0 GB free in /b");
        // Chaves literais que não são parâmetros (JSON, `{{.Tag}}`) ficam como estão.
        assert_eq!(interpolate("a {x} {{.Tag}} {y", |n| (n == "x").then(|| "1".to_string())), "a 1 {{.Tag}} {y");
    }

    #[test]
    fn gb_renders_per_language_and_neutral_keeps_only_language_free_values() {
        let gib: u64 = 1024 * 1024 * 1024;
        assert_eq!(format_gb(Lang::PtBr, gib * 3 / 2), "1,5 GB");
        assert_eq!(format_gb(Lang::En, gib * 3 / 2), "1.5 GB");
        assert_eq!(format_gb(Lang::En, 0), "0.0 GB");
        let item = |bytes: u64, path: &str| Msg::new("doctor.detail.disk_free").arg_gb("size", bytes).arg("path", path);
        let msg = Msg::new("doctor.detail.disk").arg_join("list", vec![item(gib, "/a"), item(gib * 12, "/b")], "; ");
        assert_eq!(msg.render(Lang::PtBr), "1,0 GB livres em /a; 12,0 GB livres em /b");
        assert_eq!(msg.render(Lang::En), "1.0 GB free in /a; 12.0 GB free in /b");
        let neutral = msg.neutral("doctor.detail.");
        assert_eq!(neutral.code, "disk");
        assert!(neutral.params.is_empty());
        assert_eq!(neutral.parts.len(), 2);
        assert_eq!(neutral.parts[0].code, "disk_free");
        assert_eq!(neutral.parts[0].params["size_bytes"], serde_json::json!(gib));
        assert_eq!(neutral.parts[0].params["path"], "/a");
        assert!(neutral.parts[0].parts.is_empty());
        // Texto por idioma e sub-mensagem ficam de fora; sem o prefixo pedido, a chave fica inteira.
        let mixed = Msg::new("doctor.detail.version_unknown").arg("local", "0.1.0").arg_msg("why", Msg::new("doctor.why.invalid"));
        let neutral = mixed.neutral("doctor.detail.");
        assert_eq!(neutral.code, "version_unknown");
        assert_eq!(serde_json::Value::Object(neutral.params), serde_json::json!({"local": "0.1.0"}));
        let per_lang = Msg::new("x.y").arg_per_lang("a", "1", "2").neutral("doctor.");
        assert_eq!(per_lang.code, "x.y");
        assert!(per_lang.params.is_empty());
    }
}
