//! Testes da voz (spec 138 T09), só com provedores simulados num servidor local (nunca chave real):
//! a codificação de um áudio pequeno, a leitura do WAV, a ordem das conexões, a cadeia com cada elo
//! falhando, o fim do orçamento, a transcrição e a rota dos áudios com a rotação da conversa.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, State as AxState};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::encode::{ogg_opus, pcm_of_gemini, pcm_of_wav, raw_pcm, Pcm};
use super::*;
use crate::config::{Config, Connection, ConnectionKind, VoiceRoute, WireApi};
use crate::server::nanty::store::Speaker;
use crate::server::nanty::InstanceStatus;

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Chaves montadas em tempo de execução (nenhum literal com cara de segredo).
pub(crate) fn gemini_key() -> String {
    format!("{}-{}", "gk", "voz".repeat(8))
}

pub(crate) fn openai_key() -> String {
    format!("{}-{}", "ok", "fala".repeat(6))
}

// ---- Áudio de teste ----

/// `secs_tenths` décimos de segundo de um tom, mono, na taxa `rate`.
pub(crate) fn tone(rate: u32, secs_tenths: u32) -> Vec<i16> {
    let n = rate * secs_tenths / 10;
    (0..n).map(|i| ((i as f32 * 0.07).sin() * 9000.0) as i16).collect()
}

/// Um WAV PCM de 16 bits.
pub(crate) fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2 * u32::from(channels)).to_le_bytes());
    out.extend_from_slice(&(2 * channels).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// Uma nota de voz pequena (meio segundo).
pub(crate) fn small_ogg() -> Vec<u8> {
    ogg_opus(&Pcm { rate: 24_000, samples: tone(24_000, 5) }).expect("ogg")
}

/// Amostras decodificadas do OGG, na taxa `rate` (sem o atraso do começo e o enchimento do fim).
fn decoded_samples(ogg: &[u8], rate: i32) -> usize {
    let mut reader = opus_pure::OggOpusReader::new(std::io::Cursor::new(ogg.to_vec())).expect("leitor");
    let mut decoder = reader.head().decoder(rate).expect("decoder");
    let mut trim = opus_pure::Trim::new(reader.head(), rate, 1).expect("trim");
    let mut pcm = vec![0f32; opus_pure::MAX_PACKET_SAMPLES];
    let mut total = 0;
    while let Some(packet) = reader.read_packet().expect("pacote") {
        let n = decoder.decode(&packet.data, opus_pure::MAX_PACKET_SAMPLES, &mut pcm).expect("decode");
        total += trim.keep(&packet, &pcm[..n]).len();
    }
    total
}

// ---- Provedores simulados ----

/// Resposta forçada de uma chamada.
#[derive(Clone)]
pub(crate) enum Reply {
    Status(u16),
    /// O corpo como vem (o Gemini fora do formato, por exemplo).
    Raw(Vec<u8>),
    /// O texto da transcrição.
    Text(String),
}

/// Os provedores de voz simulados: fala do Gemini e da OpenAI e a transcrição do Gemini. Sem
/// roteiro, a fala dá certo (meio segundo) e a transcrição devolve `default_text`.
#[derive(Default)]
pub(crate) struct FakeVoice {
    pub(crate) gemini: Mutex<VecDeque<Reply>>,
    pub(crate) openai: Mutex<VecDeque<Reply>>,
    pub(crate) transcribe: Mutex<VecDeque<Reply>>,
    pub(crate) default_text: Mutex<String>,
    /// (rota, cabeçalho da chave, corpo).
    pub(crate) calls: Mutex<Vec<(String, String, Value)>>,
}

impl FakeVoice {
    pub(crate) fn calls(&self, route: &str) -> Vec<Value> {
        lock(&self.calls).iter().filter(|(r, _, _)| r == route).map(|(_, _, b)| b.clone()).collect()
    }

    pub(crate) fn keys(&self) -> Vec<String> {
        lock(&self.calls).iter().map(|(_, k, _)| k.clone()).collect()
    }
}

fn header_of(headers: &HeaderMap, name: &str) -> String {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string()
}

fn status(code: u16) -> Response {
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(json!({"error": {"code": code}}))).into_response()
}

async fn gemini_speak(AxState(fake): AxState<Arc<FakeVoice>>, headers: HeaderMap, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    lock(&fake.calls).push(("gemini".into(), header_of(&headers, "x-goog-api-key"), body));
    let reply = lock(&fake.gemini).pop_front();
    let audio = match reply {
        Some(Reply::Status(code)) => return status(code),
        Some(Reply::Raw(bytes)) => bytes,
        _ => wav(24_000, 1, &tone(24_000, 5)),
    };
    let data = base64::engine::general_purpose::STANDARD.encode(audio);
    Json(json!({
        "steps": [{"type": "model_output", "content": [{"type": "audio", "data": data, "mime_type": "audio/wav"}]}],
        "usage": {"total_input_tokens": 12, "total_output_tokens": 13},
    }))
    .into_response()
}

async fn openai_speak(AxState(fake): AxState<Arc<FakeVoice>>, headers: HeaderMap, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    lock(&fake.calls).push(("openai".into(), header_of(&headers, "authorization"), body));
    let reply = lock(&fake.openai).pop_front();
    match reply {
        Some(Reply::Status(code)) => status(code),
        Some(Reply::Raw(bytes)) => bytes.into_response(),
        _ => small_ogg().into_response(),
    }
}

async fn gemini_generate(
    AxState(fake): AxState<Arc<FakeVoice>>,
    AxPath(call): AxPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    body["_call"] = json!(call);
    lock(&fake.calls).push(("transcribe".into(), header_of(&headers, "x-goog-api-key"), body));
    let reply = lock(&fake.transcribe).pop_front();
    let text = match reply {
        Some(Reply::Status(code)) => return status(code),
        Some(Reply::Text(text)) => text,
        _ => lock(&fake.default_text).clone(),
    };
    Json(json!({
        "candidates": [{"content": {"parts": [{"text": "pensando", "thought": true}, {"text": text}]}}],
        "usageMetadata": {"promptTokenCount": 40, "candidatesTokenCount": 6},
    }))
    .into_response()
}

/// Sobe os provedores simulados numa porta livre do loopback.
pub(crate) async fn fake_voice() -> (Arc<FakeVoice>, String) {
    let fake = Arc::new(FakeVoice::default());
    *lock(&fake.default_text) = "/status".into();
    let app = Router::new()
        .route("/v1beta/interactions", post(gemini_speak))
        .route("/v1/audio/speech", post(openai_speak))
        .route("/v1beta/models/:call", post(gemini_generate))
        .with_state(Arc::clone(&fake));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("porta");
    let addr = listener.local_addr().expect("endereço");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (fake, format!("http://{addr}"))
}

/// A conexão do Google Gemini no endereço simulado.
pub(crate) fn gemini_conn(base: &str) -> Connection {
    let mut conn = Connection::native("gem", "Gemini", ConnectionKind::GoogleGemini, gemini_key());
    conn.base_url_override = Some(base.to_string());
    conn.normalize();
    conn
}

/// Uma conexão por chave no endereço simulado (a fala da OpenAI).
pub(crate) fn openai_conn(base: &str) -> Connection {
    Connection {
        id: "oai".into(),
        name: "OpenAI".into(),
        base_url: format!("{base}/v1"),
        api_key: openai_key(),
        wire_api: WireApi::Chat,
        kind: ConnectionKind::ApiKey,
        base_url_override: None,
    }
}

/// Voz ligada, com a ordem Gemini e depois OpenAI nos provedores simulados.
pub(crate) fn voice_config(cfg: &mut Config, base: &str) {
    cfg.connections.push(gemini_conn(base));
    cfg.connections.push(openai_conn(base));
    cfg.nanty.voice.enabled = true;
    cfg.nanty.voice.order = vec![
        VoiceRoute { connection_id: "gem".into(), ..VoiceRoute::default() },
        VoiceRoute { connection_id: "oai".into(), ..VoiceRoute::default() },
    ];
}

// ---- Mundo de teste ----

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-t09-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

struct World {
    dir: PathBuf,
    state: AppState,
}

impl World {
    fn new(tag: &str, cfg: Config) -> Self {
        let dir = temp(tag);
        // Hora local fixa ao meio-dia: o resultado não depende da hora em que a suíte roda.
        let state = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_data_dir(dir.join("data"))
            .with_nanty_clock(crate::server::nanty::clock::test_clock());
        assert_eq!(state.nanty().acquire(), InstanceStatus::Active);
        Self { dir, state }
    }

    fn nanty(&self) -> Arc<Nanty> {
        Arc::clone(self.state.nanty())
    }

    fn journal(&self, code: &str) -> Vec<Map<String, Value>> {
        self.nanty().journal_tail(1000).into_iter().filter(|e| e.kind == JournalKind::Voice && e.code == code).map(|e| e.params).collect()
    }

    async fn get(&self, uri: &str) -> Response {
        let req = Request::builder().method(Method::GET).uri(uri).header(header::HOST, "127.0.0.1:3000").body(Body::empty()).expect("req");
        crate::server::router(self.state.clone()).oneshot(req).await.expect("resp")
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.nanty().release();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---- Codificação ----

#[test]
fn a_small_pcm_becomes_a_voice_note_with_the_exact_length() {
    let samples = tone(24_000, 5);
    let ogg = ogg_opus(&Pcm { rate: 24_000, samples: samples.clone() }).expect("ogg");
    assert!(ogg.starts_with(b"OggS"), "OGG");
    assert!(ogg.windows(8).any(|w| w == b"OpusHead"), "Opus");
    assert!(ogg.len() < samples.len() * 2 / 4, "comprime: {} bytes", ogg.len());
    assert_eq!(provider::ogg_duration_secs(&ogg), Some(1), "meio segundo, para cima");
    // Volta inteiro, sem o atraso do começo e sem o enchimento do fim (a 24 kHz, o mesmo número).
    assert_eq!(decoded_samples(&ogg, 24_000), samples.len());
    assert_eq!(Pcm { rate: 24_000, samples }.duration_secs(), 1);
}

#[test]
fn wav_is_read_and_audio_out_of_the_format_is_refused() {
    let mono = tone(24_000, 2);
    let pcm = pcm_of_wav(&wav(24_000, 1, &mono)).expect("wav");
    assert_eq!((pcm.rate, pcm.samples.len()), (24_000, mono.len()));
    // Estéreo vira mono pela média.
    let stereo: Vec<i16> = [1000i16, 3000, -2000, -4000].repeat(10);
    let pcm = pcm_of_wav(&wav(16_000, 2, &stereo)).expect("estéreo");
    assert_eq!(&pcm.samples[..2], &[2000, -3000]);
    // O Gemini sem cabeçalho: PCM cru de 24 kHz.
    let raw: Vec<u8> = mono.iter().flat_map(|s| s.to_le_bytes()).collect();
    assert_eq!(pcm_of_gemini(&raw).expect("cru"), raw_pcm(&raw, 24_000));
    // 8 bits, taxa fora do Opus, vazio e lixo.
    let mut eight = wav(24_000, 1, &mono);
    eight[34] = 8;
    assert_eq!(pcm_of_wav(&eight), Err("format"));
    assert_eq!(ogg_opus(&Pcm { rate: 44_100, samples: mono.clone() }), Err("rate"));
    assert_eq!(ogg_opus(&Pcm { rate: 24_000, samples: Vec::new() }), Err("empty"));
    assert_eq!(pcm_of_wav(b"RIFF....WAVEjunk"), Err("format"));
    assert_eq!(provider::ogg_duration_secs(b"nada"), None);
}

// ---- A ordem ----

#[test]
fn the_order_is_the_list_or_gemini_then_openai_with_the_initial_models() {
    let mut cfg = Config::default();
    let mut real_openai = openai_conn("https://api.openai.com");
    real_openai.base_url = "https://api.openai.com/v1".into();
    let other = Connection { id: "azure".into(), base_url: "https://x.openai.azure.com/v1".into(), ..openai_conn("x") };
    let anthropic = Connection::native("ant", "A", ConnectionKind::Anthropic, gemini_key());
    let no_key = Connection::native("gem2", "G2", ConnectionKind::GoogleGemini, "");
    cfg.connections = vec![real_openai, other, anthropic, no_key, gemini_conn("http://127.0.0.1:9")];
    // Automática: o Gemini com chave, depois a OpenAI; o resto fica fora.
    let (steps, skipped) = chain(&cfg);
    let got: Vec<(Provider, &str, &str, &str)> =
        steps.iter().map(|s| (s.provider, s.connection.id.as_str(), s.model.as_str(), s.voice.as_str())).collect();
    assert_eq!(
        got,
        [
            (Provider::Gemini, "gem", DEFAULT_GEMINI_TTS_MODEL, DEFAULT_GEMINI_VOICE),
            (Provider::OpenAi, "oai", DEFAULT_OPENAI_TTS_MODEL, DEFAULT_OPENAI_VOICE),
        ]
    );
    assert!(skipped.is_empty());
    // A lista vale como está: o modelo e a voz dela, a conexão que sumiu e o tipo sem voz contam.
    cfg.nanty.voice.order = vec![
        VoiceRoute { connection_id: "azure".into(), model: "m-fala".into(), voice: "v1".into() },
        VoiceRoute { connection_id: "sumiu".into(), ..VoiceRoute::default() },
        VoiceRoute { connection_id: "ant".into(), ..VoiceRoute::default() },
        VoiceRoute { connection_id: "gem".into(), ..VoiceRoute::default() },
    ];
    let (steps, skipped) = chain(&cfg);
    let got: Vec<(&str, &str, &str)> = steps.iter().map(|s| (s.connection.id.as_str(), s.model.as_str(), s.voice.as_str())).collect();
    assert_eq!(got, [("azure", "m-fala", "v1"), ("gem", DEFAULT_GEMINI_TTS_MODEL, DEFAULT_GEMINI_VOICE)]);
    assert_eq!(skipped, ["missing_connection", "unsupported"]);
    // A transcrição: a primeira conexão do Gemini com chave, ou a escolhida.
    assert_eq!(transcriber(&cfg).map(|(c, m)| (c.id, m)), Some(("gem".to_string(), DEFAULT_TRANSCRIBE_MODEL.to_string())));
    cfg.nanty.voice.transcribe = crate::config::NantyRoute { connection_id: "ant".into(), model: String::new() };
    assert!(transcriber(&cfg).is_none(), "só o Gemini transcreve");
}

// ---- A cadeia ----

fn world_with_voice(tag: &str, base: &str) -> World {
    let mut cfg = Config::default();
    voice_config(&mut cfg, base);
    World::new(tag, cfg)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gemini_speaks_first_and_the_spend_is_counted() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chain-gemini", &base);
    let spoken = speak(&w.state, &w.nanty(), "Tudo calmo por aqui.").await.expect("fala");
    assert_eq!(spoken.provider, Provider::Gemini);
    assert!(spoken.ogg.starts_with(b"OggS") && spoken.duration_secs == 1);
    assert_eq!(decoded_samples(&spoken.ogg, 24_000), tone(24_000, 5).len());
    assert!(fake.calls("openai").is_empty(), "o segundo elo não foi chamado");
    let body = &fake.calls("gemini")[0];
    assert_eq!(body["model"], DEFAULT_GEMINI_TTS_MODEL);
    assert_eq!(body["generation_config"]["speech_config"][0]["voice"], DEFAULT_GEMINI_VOICE);
    assert_eq!(body["input"][0]["content"][0]["text"], "Tudo calmo por aqui.");
    assert_eq!(fake.keys(), [gemini_key()], "a chave só no cabeçalho");
    assert!(!body.to_string().contains(&gemini_key()));
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    assert_eq!((spend.voice_calls, spend.input_tokens, spend.output_tokens), (1, 12, 13));
    assert_eq!(w.journal("voice.ok")[0]["provider"], "gemini");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_failing_link_passes_to_the_next_and_finally_to_text() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chain-fail", &base);
    // O Gemini cai: a OpenAI fala, com o Opus dela, e o modelo e a voz iniciais.
    lock(&fake.gemini).push_back(Reply::Status(500));
    let spoken = speak(&w.state, &w.nanty(), "Oi.").await.expect("fala");
    assert_eq!(spoken.provider, Provider::OpenAi);
    let body = &fake.calls("openai")[0];
    assert_eq!((body["model"].as_str(), body["voice"].as_str(), body["response_format"].as_str()), (Some(DEFAULT_OPENAI_TTS_MODEL), Some(DEFAULT_OPENAI_VOICE), Some("opus")));
    assert_eq!(w.journal("voice.failed")[0]["reason"], "provider_error");
    // O Gemini devolve o que não é áudio e a OpenAI recusa a chave: sobra o texto, com os motivos.
    lock(&fake.gemini).push_back(Reply::Raw(Vec::new()));
    lock(&fake.openai).push_back(Reply::Status(401));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.").await.err(), Some(vec!["empty", "auth"]));
    // A OpenAI manda algo que não é OGG.
    lock(&fake.gemini).push_back(Reply::Status(429));
    lock(&fake.openai).push_back(Reply::Raw(b"ID3 mp3".to_vec()));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.").await.err(), Some(vec!["rate_limited", "format"]));
    let texts = w.journal("voice.text");
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[1]["reasons"], json!(["rate_limited", "format"]));
    // Nada do texto nem da chave no registro.
    let journal = serde_json::to_string(&w.nanty().journal_tail(1000)).expect("json");
    assert!(!journal.contains("Oi.") && !journal.contains(&openai_key()), "{journal}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_budget_the_voice_is_not_called_and_text_follows() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chain-budget", &base);
    let day = w.nanty().now().day_key();
    let limit = w.state.config().read().await.nanty.daily_budget_tokens;
    w.nanty().update(|st| {
        st.add_spend(&day, limit, 0, None);
        true
    });
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.").await.err(), Some(vec!["budget"]));
    assert!(fake.calls("gemini").is_empty() && fake.calls("openai").is_empty());
    assert_eq!(w.journal("voice.text")[0]["reasons"], json!(["budget"]));
    // Sem conexão de voz nenhuma: texto, com o motivo.
    let w2 = World::new("chain-none", Config::default());
    assert_eq!(speak(&w2.state, &w2.nanty(), "Oi.").await.err(), Some(vec!["no_connection"]));
}

// ---- A transcrição ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcription_sends_the_note_inline_and_reads_only_the_answer() {
    let (fake, base) = fake_voice().await;
    lock(&fake.transcribe).push_back(Reply::Text("  olha o projeto Loja  ".into()));
    let ogg = small_ogg();
    let heard = provider::gemini_transcribe(&gemini_conn(&base), "m-ouvir", "transcreva", &ogg).await.expect("texto");
    assert_eq!((heard.text.as_str(), heard.input_tokens, heard.output_tokens), ("olha o projeto Loja", Some(40), Some(6)));
    let body = &fake.calls("transcribe")[0];
    assert_eq!(body["_call"], "m-ouvir:generateContent");
    let parts = &body["contents"][0]["parts"];
    assert_eq!(parts[0]["text"], "transcreva");
    assert_eq!(parts[1]["inline_data"]["mime_type"], "audio/ogg");
    let sent = base64::engine::general_purpose::STANDARD.decode(parts[1]["inline_data"]["data"].as_str().unwrap_or_default()).expect("b64");
    assert_eq!(sent, ogg);
    // Falha e resposta vazia viram código; modelo com forma estranha nem sai.
    lock(&fake.transcribe).push_back(Reply::Status(403));
    assert_eq!(provider::gemini_transcribe(&gemini_conn(&base), "m", "p", &ogg).await.err(), Some("auth"));
    lock(&fake.transcribe).push_back(Reply::Text("   ".into()));
    assert_eq!(provider::gemini_transcribe(&gemini_conn(&base), "m", "p", &ogg).await.err(), Some("empty"));
    assert_eq!(provider::gemini_transcribe(&gemini_conn(&base), "../x?y", "p", &ogg).await.err(), Some("config"));
    assert_eq!(provider::gemini_transcribe(&openai_conn(&base), "m", "p", &ogg).await.err(), Some("unsupported"));
}

// ---- A rota dos áudios ----

fn message_with(nanty: &Nanty, audio: Option<AudioRef>) -> ConversationMessage {
    let mut m = ConversationMessage::from_nanty(crate::server::nanty::new_id(), nanty.now().epoch_ms, Channel::Telegram, "nanty.chat.busy", Map::new());
    m.audio = audio;
    m
}

#[tokio::test]
async fn the_route_serves_only_audio_of_the_conversation_and_follows_its_rotation() {
    let w = World::new("route", Config::default());
    let nanty = w.nanty();
    let ogg = small_ogg();
    let audio = save_audio(&nanty, &ogg, 1).expect("guardado");
    let path = w.dir.join("data/nanty/audio").join(format!("{}.ogg", audio.id));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&path).expect("arquivo").permissions().mode() & 0o777, 0o600);
    // Sem mensagem que o cite, o arquivo existe mas não é servido.
    let uri = format!("/api/nanty/audio/{}", audio.id);
    assert_eq!(w.get(&uri).await.status(), StatusCode::NOT_FOUND);
    nanty.push_message(&message_with(&nanty, Some(audio.clone())));
    let resp = w.get(&uri).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "audio/ogg");
    assert_eq!(resp.headers()[header::CACHE_CONTROL], "private, max-age=3600");
    assert_eq!(resp.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    let body = resp.into_body().collect().await.expect("corpo").to_bytes();
    assert_eq!(body.as_ref(), ogg.as_slice());
    // Id inválido ou desconhecido, ou tentativa de caminho: 404 sem eco.
    for bad in ["nada", "0123456789abcdeg", "0123456789abcdef", "..%2F..%2Fstate.json", "0123456789ABCDEF0"] {
        let resp = w.get(&format!("/api/nanty/audio/{bad}")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{bad}");
        let text = String::from_utf8_lossy(&resp.into_body().collect().await.expect("corpo").to_bytes()).to_string();
        assert!(!text.contains("state.json") && !text.contains("audio/"), "{text}");
    }
    // A mensagem sai da conversa (a rotação, ou o projeto removido): o áudio deixa de ser servido e
    // a limpeza apaga o arquivo.
    let store = nanty.store.as_ref().expect("pasta");
    store.conversation().retain::<ConversationMessage>(|m| m.audio.is_none()).expect("retain");
    assert_eq!(w.get(&uri).await.status(), StatusCode::NOT_FOUND);
    prune(&nanty);
    assert!(path.exists(), "o arquivo novo espera a folga: a mensagem dele pode estar a caminho");
    prune_older(&nanty, Duration::ZERO);
    assert!(!path.exists(), "sem mensagem, o arquivo sai");
    // Só o dono da trava grava.
    nanty.release();
    assert!(save_audio(&nanty, &ogg, 1).is_none());
}

#[test]
fn the_range_header_is_read_as_a_single_byte_interval() {
    assert_eq!(parse_range(None, 100), ByteRange::Full);
    assert_eq!(parse_range(Some("bytes=0-1"), 100), ByteRange::Part { start: 0, end: 1 });
    assert_eq!(parse_range(Some("bytes=10-"), 100), ByteRange::Part { start: 10, end: 99 });
    assert_eq!(parse_range(Some("bytes=-10"), 100), ByteRange::Part { start: 90, end: 99 });
    assert_eq!(parse_range(Some("bytes=-500"), 100), ByteRange::Part { start: 0, end: 99 });
    assert_eq!(parse_range(Some("bytes=50-999"), 100), ByteRange::Part { start: 50, end: 99 });
    assert_eq!(parse_range(Some("bytes=100-"), 100), ByteRange::Unsatisfiable);
    assert_eq!(parse_range(Some("bytes=9-3"), 100), ByteRange::Unsatisfiable);
    assert_eq!(parse_range(Some("bytes=-0"), 100), ByteRange::Unsatisfiable);
    // Vários intervalos, outra unidade ou lixo: o arquivo inteiro (a regra permite ignorar o Range).
    assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), ByteRange::Full);
    assert_eq!(parse_range(Some("items=0-1"), 100), ByteRange::Full);
    assert_eq!(parse_range(Some("bytes=a-b"), 100), ByteRange::Full);
}

#[tokio::test]
async fn the_route_answers_the_range_that_safari_asks_before_playing() {
    let w = World::new("range", Config::default());
    let nanty = w.nanty();
    let ogg = small_ogg();
    let len = ogg.len();
    let audio = save_audio(&nanty, &ogg, 1).expect("guardado");
    nanty.push_message(&message_with(&nanty, Some(audio.clone())));
    let uri = format!("/api/nanty/audio/{}", audio.id);
    let ranged = |range: &str| {
        Request::builder()
            .method(Method::GET)
            .uri(&uri)
            .header(header::HOST, "127.0.0.1:3000")
            .header(header::RANGE, range)
            .body(Body::empty())
            .expect("req")
    };
    // Sem Range: 200 com o arquivo inteiro e o aviso de que aceita intervalos.
    let full = w.get(&uri).await;
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(full.headers()[header::ACCEPT_RANGES], "bytes");
    // O primeiro pedido do Safari: os dois primeiros bytes.
    let resp = crate::server::router(w.state.clone()).oneshot(ranged("bytes=0-1")).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.headers()[header::CONTENT_RANGE], format!("bytes 0-1/{len}").as_str());
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "audio/ogg");
    assert_eq!(resp.headers()[header::ACCEPT_RANGES], "bytes");
    let body = resp.into_body().collect().await.expect("corpo").to_bytes();
    assert_eq!(body.as_ref(), &ogg[..2]);
    // O resto, a partir de um ponto.
    let resp = crate::server::router(w.state.clone()).oneshot(ranged("bytes=2-")).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.headers()[header::CONTENT_RANGE], format!("bytes 2-{}/{len}", len - 1).as_str());
    let body = resp.into_body().collect().await.expect("corpo").to_bytes();
    assert_eq!(body.as_ref(), &ogg[2..]);
    // Fora do arquivo: 416 com o tamanho.
    let resp = crate::server::router(w.state.clone()).oneshot(ranged(&format!("bytes={len}-"))).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(resp.headers()[header::CONTENT_RANGE], format!("bytes */{len}").as_str());
    // O intervalo não abre o áudio que saiu da conversa.
    let store = nanty.store.as_ref().expect("pasta");
    store.conversation().retain::<ConversationMessage>(|m| m.audio.is_none()).expect("retain");
    let resp = crate::server::router(w.state.clone()).oneshot(ranged("bytes=0-1")).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn the_person_voice_message_keeps_the_mark_and_the_redacted_transcript() {
    let w = World::new("person", Config::default());
    let nanty = w.nanty();
    let secret = gemini_key();
    let secrets = SecretSet::new([secret.clone()]);
    let raw = format!("minha chave é {secret}");
    let m = crate::server::nanty::chat::person_message_with(&nanty, &secrets, Channel::Telegram, Some(&raw), true, None).expect("mensagem");
    assert_eq!((m.from, m.voice, m.text.as_deref()), (Speaker::Person, true, Some("minha chave é ***")));
    // A nota que não virou texto entra sem texto, com a marca.
    let empty = crate::server::nanty::chat::person_message_with(&nanty, &secrets, Channel::Telegram, None, true, None).expect("nota");
    assert_eq!((empty.voice, empty.text), (true, None));
    assert!(crate::server::nanty::chat::person_message_with(&nanty, &secrets, Channel::Telegram, None, false, None).is_none());
    let line = serde_json::to_string(&nanty.conversation()).expect("json");
    assert!(!line.contains(&secret) && line.contains("\"voice\":true"), "{line}");
}
