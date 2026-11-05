//! Testes da voz (spec 138 T09 e 139 T07), só com o Gemini simulado num servidor local (nunca chave
//! real): a codificação de um áudio pequeno, a leitura do WAV, a conexão única do Gemini, a falha
//! que cai para o texto, o fim do orçamento, a transcrição sem armazenamento, a lista de vozes com
//! o cache, a migração registrada ao subir e a rota dos áudios com a rotação da conversa.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, State as AxState};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::encode::{ogg_opus, pcm_of_gemini, pcm_of_wav, raw_pcm, Pcm};
use super::*;
use crate::config::{Config, Connection, ConnectionKind, WireApi, DEFAULT_GEMINI_TTS_MODEL, DEFAULT_GEMINI_VOICE, DEFAULT_TRANSCRIBE_MODEL};
use crate::server::nanty::store::Speaker;
use crate::server::nanty::InstanceStatus;

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Chaves montadas em tempo de execução (nenhum literal com cara de segredo).
pub(crate) fn gemini_key() -> String {
    format!("{}-{}", "gk", "voz".repeat(8))
}

fn openai_key() -> String {
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
    /// Um erro com este corpo (o 400 do Gemini para a chave recusada, por exemplo).
    Error(u16, &'static str),
    /// O corpo como vem (o Gemini fora do formato, por exemplo).
    Raw(Vec<u8>),
    /// O texto da transcrição.
    Text(String),
}

/// O Gemini simulado: a fala, a transcrição e a lista de vozes. Sem roteiro, a fala dá certo (meio
/// segundo), a transcrição devolve `default_text` e a lista tem duas páginas.
#[derive(Default)]
pub(crate) struct FakeVoice {
    pub(crate) gemini: Mutex<VecDeque<Reply>>,
    pub(crate) transcribe: Mutex<VecDeque<Reply>>,
    pub(crate) voices: Mutex<VecDeque<Reply>>,
    pub(crate) default_text: Mutex<String>,
    /// Com a fala presa ([`Self::hold_speech`]), cada fala anota a chamada e só responde depois de
    /// uma liberação ([`Self::release_speech`]): os testes não dependem de quanto ela demora.
    pub(crate) speech_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    /// (rota, cabeçalho da chave, corpo; na lista de vozes, a consulta).
    pub(crate) calls: Mutex<Vec<(String, String, Value)>>,
}

impl FakeVoice {
    pub(crate) fn calls(&self, route: &str) -> Vec<Value> {
        lock(&self.calls).iter().filter(|(r, _, _)| r == route).map(|(_, _, b)| b.clone()).collect()
    }

    /// As próximas falas esperam a liberação.
    pub(crate) fn hold_speech(&self) {
        *lock(&self.speech_gate) = Some(Arc::new(tokio::sync::Semaphore::new(0)));
    }

    /// Solta `count` falas presas, na ordem em que chegaram.
    pub(crate) fn release_speech(&self, count: usize) {
        if let Some(gate) = lock(&self.speech_gate).as_ref() {
            gate.add_permits(count);
        }
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

fn error_with(code: u16, text: &'static str) -> Response {
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), text).into_response()
}

async fn gemini_speak(AxState(fake): AxState<Arc<FakeVoice>>, headers: HeaderMap, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    lock(&fake.calls).push(("gemini".into(), header_of(&headers, "x-goog-api-key"), body));
    let gate = lock(&fake.speech_gate).clone();
    if let Some(gate) = gate {
        if let Ok(permit) = gate.acquire().await {
            permit.forget();
        }
    }
    let reply = lock(&fake.gemini).pop_front();
    let audio = match reply {
        Some(Reply::Status(code)) => return status(code),
        Some(Reply::Error(code, text)) => return error_with(code, text),
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

/// A lista de vozes em duas páginas, com os campos ora direto na voz (o exemplo do SDK), ora dentro
/// de `prompted` (a referência), e uma voz sem id, que fica de fora.
async fn gemini_voices(
    AxState(fake): AxState<Arc<FakeVoice>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    lock(&fake.calls).push(("voices".into(), header_of(&headers, "x-goog-api-key"), json!(query)));
    match lock(&fake.voices).pop_front() {
        Some(Reply::Status(code)) => return status(code),
        Some(Reply::Error(code, text)) => return error_with(code, text),
        _ => {}
    }
    if query.get("page_token").map(String::as_str) == Some("p2") {
        return Json(json!({"voices": [
            {"type": "prompted", "id": "Aoede", "display_name": "Aoede", "prompted": {"language_code": "en-US", "gender": "female", "description": "Breezy"}},
            {"type": "prebuilt", "id": "fr-fr-assistant-1", "display_name": "Louise", "language_code": "fr-FR"},
        ]}))
        .into_response();
    }
    Json(json!({
        "voices": [
            {"type": "prebuilt", "id": "Kore", "display_name": "Kore", "language_code": "pt-BR", "gender": "female", "description": "Firm"},
            {"type": "prebuilt", "id": "Puck", "display_name": "Puck", "language_code": "en-US", "gender": "male", "description": "Upbeat"},
            {"type": "prebuilt", "display_name": "Sem id"},
        ],
        "next_page_token": "p2",
    }))
    .into_response()
}

/// A lista de modelos em duas páginas, como a de verdade: de texto, de fala e de outras famílias.
async fn gemini_models(
    AxState(fake): AxState<Arc<FakeVoice>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    lock(&fake.calls).push(("models".into(), header_of(&headers, "x-goog-api-key"), json!(query)));
    if query.get("pageToken").map(String::as_str) == Some("m2") {
        return Json(json!({"models": [{"name": "models/gemini-3.8-flash-lite-tts"}, {"name": "models/gemini-3.8-flash-image"}]})).into_response();
    }
    Json(json!({"models": [{"name": "models/gemini-3.8-flash"}, {"name": "models/gemini-3.8-flash-tts"}], "nextPageToken": "m2"})).into_response()
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
        Some(Reply::Error(code, text)) => return error_with(code, text),
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
        .route("/v1beta/voices", get(gemini_voices))
        .route("/v1beta/models", get(gemini_models))
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

/// Uma conexão por chave no endereço simulado (a OpenAI, que não tem mais voz).
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

/// Voz ligada na conexão do Gemini simulado (a OpenAI fica cadastrada, sem voz).
pub(crate) fn voice_config(cfg: &mut Config, base: &str) {
    cfg.connections.push(openai_conn(base));
    cfg.connections.push(gemini_conn(base));
    cfg.nanty.voice.enabled = true;
    cfg.nanty.voice.connection_id = "gem".into();
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
}

// ---- A conexão da voz ----

#[test]
fn the_voice_is_one_gemini_connection_with_the_initial_models() {
    let mut cfg = Config::default();
    let anthropic = Connection::native("ant", "A", ConnectionKind::Anthropic, gemini_key());
    let no_key = Connection::native("gem2", "G2", ConnectionKind::GoogleGemini, "");
    cfg.connections = vec![openai_conn("https://api.openai.com"), anthropic, no_key, gemini_conn("http://127.0.0.1:9")];
    // Automática: a primeira conexão do Gemini com chave fala e transcreve, com os iniciais.
    let step = speaker(&cfg).expect("fala");
    assert_eq!((step.connection.id.as_str(), step.model.as_str(), step.voice.as_str()), ("gem", DEFAULT_GEMINI_TTS_MODEL, DEFAULT_GEMINI_VOICE));
    assert_eq!(transcriber(&cfg).map(|(c, m)| (c.id, m)), Some(("gem".to_string(), DEFAULT_TRANSCRIBE_MODEL.to_string())));
    // A escolhida, com o modelo leve só porque a pessoa escolheu (nunca troca sozinho).
    cfg.nanty.voice.connection_id = "gem".into();
    cfg.nanty.voice.speech_model = "gemini-3.8-flash-lite-tts".into();
    cfg.nanty.voice.voice = "Puck".into();
    cfg.nanty.voice.transcribe_model = "m-ouvir".into();
    let step = speaker(&cfg).expect("fala");
    assert_eq!((step.model.as_str(), step.voice.as_str()), ("gemini-3.8-flash-lite-tts", "Puck"));
    assert_eq!(transcriber(&cfg).map(|(_, m)| m).as_deref(), Some("m-ouvir"));
    // A OpenAI, a Anthropic e a conexão que sumiu não falam nem transcrevem.
    for (id, code) in [("oai", "unsupported"), ("ant", "unsupported"), ("sumiu", "missing_connection")] {
        cfg.nanty.voice.connection_id = id.into();
        assert_eq!(speaker(&cfg).err(), Some(code), "{id}");
        assert!(transcriber(&cfg).is_none(), "{id}");
    }
    // Sem nenhuma conexão do Gemini com chave.
    cfg.nanty.voice.connection_id = String::new();
    cfg.connections.retain(|c| c.id != "gem");
    assert_eq!(speaker(&cfg).err(), Some("no_connection"));
    assert!([ConnectionKind::ApiKey, ConnectionKind::Anthropic, ConnectionKind::ChatgptPlan].iter().all(|k| !speaks(*k) && !transcribes(*k)));
    assert!(speaks(ConnectionKind::GoogleGemini) && transcribes(ConnectionKind::GoogleGemini));
}

/// O corpo da fala segue o formato atual da Interactions API; o estilo vai como anotação.
#[test]
fn the_speech_body_follows_the_current_interactions_format() {
    let body = provider::speech_body("gemini-3.8-flash-tts", "Kore", "Oi.", Tone::Neutral);
    assert_eq!(body["input"][0]["type"], "user_input");
    assert_eq!(body["input"][0]["content"][0], json!({"type": "text", "text": "Oi."}));
    assert_eq!(body["response_format"], json!({"type": "audio", "mime_type": "audio/wav"}));
    assert_eq!(body["generation_config"]["speech_config"], json!([{"voice": "Kore"}]));
    assert_eq!(body["store"], false);
    for tone in Tone::ALL.into_iter().filter(|t| *t != Tone::Neutral) {
        let styled = provider::speech_body("m", "Kore", "Oi.", tone);
        assert_eq!(styled["input"][0]["content"][0]["annotations"], json!([{"type": "speech_metadata", "style": tone.style()}]));
    }
}

// ---- A cadeia ----

fn world_with_voice(tag: &str, base: &str) -> World {
    let mut cfg = Config::default();
    voice_config(&mut cfg, base);
    World::new(tag, cfg)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gemini_speaks_and_the_spend_is_counted() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chain-gemini", &base);
    let spoken = speak(&w.state, &w.nanty(), "Tudo calmo por aqui.", Tone::Neutral).await.expect("fala");
    assert!(spoken.ogg.starts_with(b"OggS") && spoken.duration_secs == 1);
    assert_eq!(decoded_samples(&spoken.ogg, 24_000), tone(24_000, 5).len());
    let body = &fake.calls("gemini")[0];
    assert_eq!(body["model"], DEFAULT_GEMINI_TTS_MODEL);
    assert_eq!(body["generation_config"]["speech_config"][0]["voice"], DEFAULT_GEMINI_VOICE);
    assert_eq!(body["input"][0]["content"][0]["text"], "Tudo calmo por aqui.");
    assert_eq!(body["store"], false);
    assert_eq!(fake.keys(), [gemini_key()], "a chave só no cabeçalho");
    assert!(!body.to_string().contains(&gemini_key()));
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    assert_eq!((spend.voice_calls, spend.input_tokens, spend.output_tokens), (1, 12, 13));
    assert_eq!(w.journal("voice.ok")[0]["provider"], "gemini");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_gemini_falls_back_to_text() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chain-fail", &base);
    // O Gemini cai: sobra o texto, com o motivo, e nenhum outro provedor é chamado.
    lock(&fake.gemini).push_back(Reply::Status(500));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["provider_error"]));
    assert_eq!(w.journal("voice.failed")[0]["reason"], "provider_error");
    // O que não é áudio, e a chave recusada.
    lock(&fake.gemini).push_back(Reply::Raw(Vec::new()));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["empty"]));
    lock(&fake.gemini).push_back(Reply::Status(401));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["auth"]));
    let texts = w.journal("voice.text");
    assert_eq!(texts.len(), 3);
    assert_eq!(texts[2]["reasons"], json!(["auth"]));
    assert_eq!(fake.calls("gemini").len(), 3);
    // A conexão escolhida é a OpenAI (uma configuração feita à mão): texto, sem chamada.
    w.state.config().write().await.nanty.voice.connection_id = "oai".into();
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["unsupported"]));
    assert_eq!(fake.calls("gemini").len(), 3);
    // Nada do texto nem das chaves no registro.
    let journal = serde_json::to_string(&w.nanty().journal_tail(1000)).expect("json");
    assert!(!journal.contains("Oi.") && !journal.contains(&openai_key()) && !journal.contains(&gemini_key()), "{journal}");
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
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["budget"]));
    assert!(fake.calls("gemini").is_empty());
    assert_eq!(w.journal("voice.text")[0]["reasons"], json!(["budget"]));
    // Sem conexão de voz nenhuma: texto, com o motivo.
    let w2 = World::new("chain-none", Config::default());
    assert_eq!(speak(&w2.state, &w2.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["no_connection"]));
}

// ---- O tom ----

/// A anotação da fala `n` do Gemini simulado (`Value::Null` sem anotação).
pub(crate) fn style_of(fake: &FakeVoice, n: usize) -> Value {
    fake.calls("gemini")[n]["input"][0]["content"][0]["annotations"][0]["style"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_fixed_style_of_the_tone_reaches_the_provider() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("tone-style", &base);
    for tone in Tone::ALL {
        speak(&w.state, &w.nanty(), "Oi.", tone).await.expect("fala");
    }
    let styles: Vec<Value> = (0..Tone::ALL.len()).map(|n| style_of(&fake, n)).collect();
    assert_eq!(styles, [Value::Null, json!("warm and upbeat"), json!("serious and steady"), json!("concerned but composed"), json!("calm and unhurried")]);
    assert!(fake.calls("gemini")[0]["input"][0]["content"][0].get("annotations").is_none(), "o neutro omite a anotação");
    assert_eq!(w.journal("voice.ok")[1]["tone"], "upbeat");
}

/// Um 400 com a anotação refaz uma vez sem ela, e a chamada que deu certo entra no gasto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_style_is_retried_once_without_it_and_counts_in_the_budget() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("tone-retry", &base);
    lock(&fake.gemini).push_back(Reply::Status(400));
    let spoken = speak(&w.state, &w.nanty(), "Tudo certo.", Tone::Calm).await.expect("fala");
    assert!(spoken.ogg.starts_with(b"OggS"));
    assert_eq!(fake.calls("gemini").len(), 2);
    assert_eq!(style_of(&fake, 0), json!("calm and unhurried"));
    assert_eq!(style_of(&fake, 1), Value::Null);
    assert_eq!(fake.calls("gemini")[1]["input"][0]["content"][0]["text"], "Tudo certo.");
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    assert_eq!((spend.voice_calls, spend.input_tokens, spend.output_tokens), (1, 12, 13));
    assert_eq!(w.journal("voice.style_refused")[0]["tone"], "calm");
    assert_eq!(w.journal("voice.ok")[0]["tone"], "calm");
    assert!(w.journal("voice.failed").is_empty());
}

/// A nova tentativa é uma só: recusada de novo, sobra o texto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_refusal_falls_back_to_text() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("tone-twice", &base);
    lock(&fake.gemini).extend([Reply::Status(400), Reply::Status(400)]);
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Serious).await.err(), Some(vec!["bad_request"]));
    assert_eq!(fake.calls("gemini").len(), 2);
    assert_eq!(w.journal("voice.failed")[0]["reason"], "bad_request");
    assert_eq!(w.nanty().snapshot().spend_on(&w.nanty().now().day_key()).voice_calls, 0);
}

/// Só a recusa do pedido com anotação refaz: não a falha do provedor, a chave recusada ou o neutro.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn other_failures_and_the_neutral_tone_are_not_retried() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("tone-other", &base);
    lock(&fake.gemini).push_back(Reply::Status(500));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Calm).await.err(), Some(vec!["provider_error"]));
    lock(&fake.gemini).push_back(Reply::Error(400, "{\"error\":{\"status\":\"INVALID_ARGUMENT\",\"details\":[{\"reason\":\"API_KEY_INVALID\"}]}}"));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Calm).await.err(), Some(vec!["auth"]));
    lock(&fake.gemini).push_back(Reply::Status(400));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["bad_request"]));
    assert_eq!(fake.calls("gemini").len(), 3);
    assert!(w.journal("voice.style_refused").is_empty());
}

/// A segunda chamada confere o orçamento: acabou enquanto a primeira estava no provedor, ela não sai.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_retry_is_not_sent_without_budget() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("tone-budget", &base);
    fake.hold_speech();
    lock(&fake.gemini).push_back(Reply::Status(400));
    let (state, nanty) = (w.state.clone(), w.nanty());
    let job = tokio::spawn(async move { speak(&state, &nanty, "Oi.", Tone::Calm).await });
    while fake.calls("gemini").is_empty() {
        tokio::task::yield_now().await;
    }
    let nanty = w.nanty();
    let (day, limit) = (nanty.now().day_key(), w.state.config().read().await.nanty.daily_budget_tokens);
    nanty.update(|st| {
        st.add_spend(&day, limit, 0, None);
        true
    });
    fake.release_speech(1);
    assert_eq!(job.await.expect("tarefa").err(), Some(vec!["budget"]));
    assert_eq!(fake.calls("gemini").len(), 1);
    assert_eq!(w.journal("voice.text")[0]["reasons"], json!(["budget"]));
}

// ---- A transcrição ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcription_sends_the_note_inline_without_storage_and_reads_only_the_answer() {
    let (fake, base) = fake_voice().await;
    lock(&fake.transcribe).push_back(Reply::Text("  olha o projeto Loja  ".into()));
    let ogg = small_ogg();
    let heard = provider::gemini_transcribe(&gemini_conn(&base), "m-ouvir", "transcreva", &ogg).await.expect("texto");
    assert_eq!((heard.text.as_str(), heard.input_tokens, heard.output_tokens), ("olha o projeto Loja", Some(40), Some(6)));
    let body = &fake.calls("transcribe")[0];
    assert_eq!(body["_call"], "m-ouvir:generateContent");
    // Privacidade (C-17): o pedido sai com o armazenamento desligado.
    assert_eq!(body["store"], json!(false));
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

// ---- As vozes ----

async fn json_of(resp: Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("corpo").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_voices_route_lists_every_page_and_keeps_a_short_cache() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("voices", &base);
    let (status, body) = json_of(w.get("/api/nanty/voices").await).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["connection_id"].as_str(), body["voice"].as_str()), (Some("gem"), Some(DEFAULT_GEMINI_VOICE)));
    let ids: Vec<&str> = body["voices"].as_array().expect("lista").iter().filter_map(|v| v["id"].as_str()).collect();
    assert_eq!(ids, ["Kore", "Puck", "Aoede"], "as duas páginas, sem a voz sem id nem a de outro idioma");
    // Spec 139 T12: os modelos de fala, de todas as páginas, que a lista das conexões deixa de fora.
    assert_eq!(body["speech_models"], json!(["gemini-3.8-flash-lite-tts", "gemini-3.8-flash-tts"]));
    assert_eq!(fake.calls("models").len(), 2);
    assert_eq!(body["voices"][0], json!({"id": "Kore", "display_name": "Kore", "type": "prebuilt", "language_code": "pt-BR", "gender": "female", "description": "Firm"}));
    assert_eq!((body["voices"][2]["language_code"].as_str(), body["voices"][2]["description"].as_str()), (Some("en-US"), Some("Breezy")), "lido dentro de `prompted`");
    let pages = fake.calls("voices");
    assert_eq!(pages.len(), 2);
    assert_eq!((pages[0]["page_size"].as_str(), pages[1]["page_token"].as_str()), (Some("1000"), Some("p2")));
    assert_eq!(fake.keys(), vec![gemini_key(); 4], "a chave só no cabeçalho");
    assert!(!body.to_string().contains(&gemini_key()));
    // Dentro do cache: nenhuma chamada nova.
    let (status, again) = json_of(w.get("/api/nanty/voices?connection_id=gem").await).await;
    assert_eq!((status, again["voices"].clone()), (StatusCode::OK, body["voices"].clone()));
    assert_eq!(fake.calls("voices").len(), 2);
}

/// O erro do Gemini para a chave recusada: 400, com o motivo no corpo.
const KEY_INVALID_BODY: &str = r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#;
const KEY_MESSAGE_BODY: &str = r#"{"error":{"code":400,"message":"API key not valid."}}"#;
const PLAIN_400_BODY: &str = r#"{"error":{"code":400,"message":"Invalid value at voice","status":"INVALID_ARGUMENT"}}"#;

/// Spec 139 T09: o Gemini recusa a chave com 400, e os três caminhos dizem `auth`; o corpo só escolhe
/// o código. Um 400 comum continua `bad_request`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_refused_with_a_400_is_auth_on_every_path_and_the_body_stays_out() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("key-400", &base);
    // A lista de vozes, pela rota: a tela lê o `reason` e diz "Chave recusada".
    lock(&fake.voices).push_back(Reply::Error(400, KEY_INVALID_BODY));
    let (status, body) = json_of(w.get("/api/nanty/voices").await).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_GATEWAY, Some("nanty_voice_list_failed")));
    assert_eq!((body["params"]["reason"].as_str(), body["params"]["connection_id"].as_str()), (Some("auth"), Some("gem")));
    assert!(!body.to_string().contains("API key not valid"), "{body}");
    lock(&fake.voices).push_back(Reply::Error(400, PLAIN_400_BODY));
    let (_, body) = json_of(w.get("/api/nanty/voices").await).await;
    assert_eq!(body["params"]["reason"], "bad_request");
    // A fala: o motivo certo no registro, sem o corpo.
    lock(&fake.gemini).push_back(Reply::Error(400, KEY_INVALID_BODY));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["auth"]));
    assert_eq!(w.journal("voice.failed")[0]["reason"], "auth");
    lock(&fake.gemini).push_back(Reply::Error(400, PLAIN_400_BODY));
    assert_eq!(speak(&w.state, &w.nanty(), "Oi.", Tone::Neutral).await.err(), Some(vec!["bad_request"]));
    let record = format!("{:?}{:?}", w.journal("voice.failed"), w.journal("voice.text"));
    assert!(!record.contains("API key") && !record.contains("Invalid value"), "{record}");
    // A transcrição, com as duas formas do corpo.
    let ogg = b"OggS".to_vec();
    for (body, code) in [(KEY_INVALID_BODY, "auth"), (KEY_MESSAGE_BODY, "auth"), (PLAIN_400_BODY, "bad_request")] {
        lock(&fake.transcribe).push_back(Reply::Error(400, body));
        assert_eq!(provider::gemini_transcribe(&gemini_conn(&base), "m", "p", &ogg).await.err(), Some(code), "{body}");
    }
    // O corpo só conta num 400: em outro status, o código é o do status.
    lock(&fake.transcribe).push_back(Reply::Error(500, KEY_INVALID_BODY));
    assert_eq!(provider::gemini_transcribe(&gemini_conn(&base), "m", "p", &ogg).await.err(), Some("provider_error"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_voices_route_answers_coded_errors() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("voices-errors", &base);
    let code = |v: &Value| v["code"].as_str().map(str::to_string);
    // Conexão que não é do Gemini, ou que não existe.
    for uri in ["/api/nanty/voices?connection_id=oai", "/api/nanty/voices?connection_id=sumiu"] {
        let (status, body) = json_of(w.get(uri).await).await;
        assert_eq!((status, code(&body).as_deref()), (StatusCode::CONFLICT, Some("nanty_voice_no_connection")), "{uri}");
    }
    // O provedor falhou: 502 com o motivo em código, sem cache do erro.
    lock(&fake.voices).push_back(Reply::Status(500));
    let (status, body) = json_of(w.get("/api/nanty/voices").await).await;
    assert_eq!((status, code(&body).as_deref()), (StatusCode::BAD_GATEWAY, Some("nanty_voice_list_failed")));
    assert_eq!(body["params"]["reason"], "provider_error");
    assert_eq!(body["params"]["connection_id"], "gem", "a tela segue com os modelos da conexão");
    let (status, _) = json_of(w.get("/api/nanty/voices").await).await;
    assert_eq!(status, StatusCode::OK, "o erro não fica guardado");
    // Sem chave.
    let mut cfg = Config::default();
    cfg.connections.push(Connection::native("gk", "G", ConnectionKind::GoogleGemini, ""));
    cfg.nanty.voice.connection_id = "gk".into();
    let w2 = World::new("voices-nokey", cfg);
    let (status, body) = json_of(w2.get("/api/nanty/voices").await).await;
    assert_eq!((status, code(&body).as_deref()), (StatusCode::CONFLICT, Some("nanty_voice_no_key")));
    // Na automática, um Gemini sem chave também é "sem chave": a mesma regra dos Ajustes (T09).
    let mut cfg = Config::default();
    cfg.connections.push(Connection::native("gk", "G", ConnectionKind::GoogleGemini, ""));
    let w4 = World::new("voices-auto-nokey", cfg);
    let (status, body) = json_of(w4.get("/api/nanty/voices").await).await;
    assert_eq!((status, code(&body).as_deref()), (StatusCode::CONFLICT, Some("nanty_voice_no_key")));
    // A gravada sumiu: sem parâmetro, é ela; com o parâmetro vazio, a automática dos Ajustes (T09).
    let mut cfg = Config::default();
    voice_config(&mut cfg, &base);
    cfg.connections.insert(0, Connection::native("sem-chave", "G", ConnectionKind::GoogleGemini, ""));
    cfg.nanty.voice.connection_id = "sumiu".into();
    let w5 = World::new("voices-auto-saved-gone", cfg);
    let (status, body) = json_of(w5.get("/api/nanty/voices").await).await;
    assert_eq!((status, code(&body).as_deref()), (StatusCode::CONFLICT, Some("nanty_voice_no_connection")));
    let (status, body) = json_of(w5.get("/api/nanty/voices?connection_id=").await).await;
    assert_eq!((status, body["connection_id"].as_str()), (StatusCode::OK, Some("gem")), "a primeira com chave: {body}");
    // Sem conexão do Gemini nenhuma.
    let w3 = World::new("voices-none", Config::default());
    let (status, body) = json_of(w3.get("/api/nanty/voices").await).await;
    assert_eq!((status, code(&body).as_deref()), (StatusCode::CONFLICT, Some("nanty_voice_no_connection")));
    for lang in crate::i18n::Lang::ALL {
        for key in ["nanty_voice_no_connection", "nanty_voice_no_key", "nanty_voice_list_failed"] {
            let text = crate::i18n::tr(lang, &format!("serverError.{key}"), &[]);
            assert!(!text.is_empty() && !text.contains("serverError"), "{lang:?}: {text}");
        }
    }
}

// ---- A migração ao subir ----

/// Spec 139 T07 (C-16): a voz antiga carregada vira a nova; ao subir, o Nanty registra uma linha
/// (só números) e o arquivo passa à forma nova, sem a ordem antiga.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_old_voice_is_migrated_and_recorded_when_the_nanty_starts() {
    let dir = temp("migrate");
    let path = dir.join("config.toml");
    let old = r#"
[[connections]]
id = "gem"
name = "Gemini"
base_url = "https://generativelanguage.googleapis.com"
api_key = "env:NANTS_T07_SEM_CHAVE"
kind = "google_gemini"

[[connections]]
id = "oai"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key = "env:NANTS_T07_SEM_CHAVE"

[nanty.voice]
enabled = true
order = [{ connection_id = "gem", voice = "Puck" }, { connection_id = "oai", model = "gpt-4o-mini-tts-2025-12-15" }]
transcribe = { connection_id = "gem", model = "" }
"#;
    std::fs::write(&path, old).expect("grava");
    let cfg = Config::load_from(&path).expect("carrega");
    let state = AppState::new(path.clone(), cfg, 3000)
        .with_data_dir(dir.join("data"))
        .with_nanty_clock(crate::server::nanty::clock::test_clock());
    let running = crate::server::nanty::start(&state);
    let nanty = Arc::clone(state.nanty());
    let migrated = || {
        nanty.journal_tail(100).into_iter().filter(|e| e.kind == JournalKind::Voice && e.code == "voice.migrated").map(|e| e.params).collect::<Vec<_>>()
    };
    for _ in 0..500 {
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        if !migrated().is_empty() && !written.contains("order =") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let lines = migrated();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["dropped"], 1);
    assert_eq!((lines[0]["moved"].as_bool(), lines[0]["transcribe_dropped"].as_bool()), (Some(true), Some(false)));
    let written = std::fs::read_to_string(&path).expect("arquivo");
    assert!(!written.contains("order =") && !written.contains("gpt-4o"), "{written}");
    let again = Config::load_from(&path).expect("relê");
    assert_eq!((again.nanty.voice.connection_id.as_str(), again.nanty.voice.voice_name().as_str()), ("gem", "Puck"));
    assert_eq!(again.nanty.voice.migrated, None, "nada mais a migrar");
    assert_eq!(state.config().read().await.nanty.voice.migrated, None, "o recado foi consumido");
    state.trigger_shutdown();
    running.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Spec 139 T08: a migração sem conseguir gravar deixa o código no registro, e o resto sobe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_migration_that_cannot_be_saved_leaves_the_code_in_the_journal() {
    let dir = temp("migrate-fail");
    // O arquivo da configuração fica dentro de um arquivo: a gravação falha.
    std::fs::write(dir.join("arquivo"), "x").expect("grava");
    let mut cfg = Config::default();
    cfg.nanty.voice.migrated = Some(VoiceMigration { moved: true, dropped: 1, transcribe_dropped: false });
    let state = AppState::new(dir.join("arquivo").join("config.toml"), cfg, 3000)
        .with_data_dir(dir.join("data"))
        .with_nanty_clock(crate::server::nanty::clock::test_clock());
    let running = crate::server::nanty::start(&state);
    let nanty = Arc::clone(state.nanty());
    let code = |name: &str| nanty.journal_tail(100).into_iter().filter(|e| e.kind == JournalKind::Voice && e.code == name).map(|e| e.params).collect::<Vec<_>>();
    for _ in 0..500 {
        if !code("voice.migration_unsaved").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(code("voice.migrated").len(), 1);
    assert_eq!(code("voice.migration_unsaved")[0]["reason"], "config_save_failed");
    state.trigger_shutdown();
    running.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- A segunda linha da mensagem ----

/// Spec 139 T08: a mensagem que ganha o áudio tem duas linhas no arquivo. A conversa mostra uma só,
/// o áudio segue servido e guardado, e a remoção do projeto leva as duas.
#[test]
fn a_message_with_two_lines_follows_the_audio_rules_and_leaves_whole() {
    let w = World::new("two-lines", Config::default());
    let nanty = w.nanty();
    let audio = save_audio(&nanty, &small_ogg(), 1).expect("áudio");
    let mut message = message_with(&nanty, None);
    message.project_id = Some("p1".into());
    nanty.keep_message(&message);
    message.audio = Some(audio.clone());
    nanty.keep_message(&message);
    assert_eq!(nanty.conversation_all().len(), 2, "o arquivo só recebe acréscimos");
    let all = nanty.conversation_all();
    assert!(all[0].seq.is_some() && all[0].seq == all[1].seq, "a segunda linha leva o número da primeira");
    assert_eq!(nanty.conversation(), [ConversationMessage { seq: all[0].seq, ..message.clone() }]);
    assert!(nanty.read_audio(&audio.id).is_some());
    prune_older(&nanty, Duration::ZERO);
    assert!(nanty.read_audio(&audio.id).is_some(), "a segunda linha cita o áudio");
    nanty.forget_project("p1");
    assert!(nanty.conversation_all().is_empty());
    assert!(nanty.read_audio(&audio.id).is_none());
}

// ---- Os trechos da fala (spec 141 T08, C-08) ----

/// Os trechos terminam num fim de frase; sem nenhum, numa palavra inteira; e juntos somam o texto.
#[test]
fn the_speech_is_cut_into_chunks_at_the_end_of_a_sentence() {
    assert_eq!(speech_chunks("  Curto.  ", 100), ["Curto."]);
    assert_eq!(
        speech_chunks("Primeira frase. Segunda frase. Terceira que passa do limite", 40),
        ["Primeira frase. Segunda frase.", "Terceira que passa do limite"]
    );
    assert_eq!(speech_chunks("Tudo certo! Valeu? Sobra um pedaço que nao cabe", 25), ["Tudo certo! Valeu?", "Sobra um pedaço que nao", "cabe"]);
    // Um ponto de número não é fim de frase, e uma quebra de linha é.
    assert_eq!(speech_chunks("Custou 1.50 dolares hoje e segue sem parar", 25)[0], "Custou 1.50 dolares hoje");
    assert_eq!(speech_chunks("Loja\nCustou 1.50 dolares hoje e segue sem parar", 20)[0], "Loja");
    // Sem espaço nenhum: o limite, sem partir um caractere, e nada se perde.
    assert_eq!(speech_chunks(&"ç".repeat(30), 10), ["ç".repeat(10), "ç".repeat(10), "ç".repeat(10)]);
    assert!(speech_chunks("   ", 10).is_empty());
    let text = "Uma frase inteira sobre o projeto. ".repeat(100);
    let chunks = speech_chunks(&text, CHUNK_CHARS);
    assert!(chunks.len() > 1 && chunks.iter().all(|c| c.chars().count() <= CHUNK_CHARS && c.ends_with("projeto.")));
    let words = |t: &str| t.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(words(&chunks.join(" ")), words(&text));
}

/// Uma resposta de 3.000 caracteres sai numa fala só: os trechos em sequência, com a mesma voz e o
/// mesmo tom, que somam o texto inteiro; o áudio é um só, com a duração somada; o gasto conta por
/// trecho.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_3000_char_reply_is_one_note_from_all_its_chunks() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chunks", &base);
    let text = "Uma frase inteira sobre o projeto, com o detalhe que importa. ".repeat(49);
    let text = text.trim();
    assert!(text.chars().count() >= 3000, "{}", text.chars().count());
    let n = speech_chunks(text, CHUNK_CHARS).len();
    assert!(n >= 3, "{n}");
    let spoken = speak(&w.state, &w.nanty(), text, Tone::Calm).await.expect("fala");
    let calls = fake.calls("gemini");
    assert_eq!(calls.len(), n, "um pedido por trecho de até {CHUNK_CHARS}");
    let said: Vec<String> = calls.iter().map(|c| c["input"][0]["content"][0]["text"].as_str().unwrap_or_default().to_string()).collect();
    assert_eq!(said.join(" "), text, "os trechos somam o texto inteiro");
    for (n, call) in calls.iter().enumerate() {
        assert_eq!(call["generation_config"]["speech_config"][0]["voice"], DEFAULT_GEMINI_VOICE, "a mesma voz");
        assert_eq!(style_of(&fake, n), json!("calm and unhurried"), "o mesmo tom");
    }
    // Um áudio só: os trechos de meio segundo e as pausas entre eles.
    let gap = 24_000 / 1000 * 300;
    let samples = n * tone(24_000, 5).len() + (n - 1) * gap;
    assert_eq!(decoded_samples(&spoken.ogg, 24_000), samples);
    assert_eq!(spoken.duration_secs, u32::try_from(samples.div_ceil(24_000)).unwrap_or(0));
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    let calls = n as u64;
    assert_eq!((spend.voice_calls, spend.input_tokens, spend.output_tokens), (calls, 12 * calls, 13 * calls), "o gasto conta por trecho");
    let ok = w.journal("voice.ok");
    assert_eq!((ok.len(), ok[0]["chunks"].clone()), (1, json!(n)));
}

/// Um trecho que falha (ou o tom recusado num trecho do meio) não deixa áudio parcial: a fala inteira
/// falha, com o motivo e o trecho no registro, e o que o provedor já cobrou fica no gasto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_chunk_leaves_no_partial_audio() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chunk-fail", &base);
    let text = "Uma frase inteira sobre o projeto, com o detalhe que importa. ".repeat(48);
    let n = speech_chunks(&text, CHUNK_CHARS).len();
    lock(&fake.gemini).push_back(Reply::Raw(wav(24_000, 1, &tone(24_000, 5))));
    lock(&fake.gemini).push_back(Reply::Status(500));
    assert_eq!(speak(&w.state, &w.nanty(), &text, Tone::Neutral).await.err(), Some(vec!["provider_error"]));
    assert_eq!(fake.calls("gemini").len(), 2, "para no trecho que falhou");
    let failed = w.journal("voice.failed");
    assert_eq!((failed[0]["reason"].clone(), failed[0]["chunk"].clone(), failed[0]["chunks"].clone()), (json!("provider_error"), json!(1), json!(n)));
    assert_eq!(w.journal("voice.text")[0]["reasons"], json!(["provider_error"]));
    assert!(w.journal("voice.ok").is_empty());
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    assert_eq!(spend.voice_calls, 1, "o trecho falado conta");
    // O tom recusado no primeiro trecho: os outros já vão no neutro, e a nota tem um tom só.
    lock(&fake.calls).clear();
    lock(&fake.gemini).push_back(Reply::Status(400));
    speak(&w.state, &w.nanty(), &text, Tone::Calm).await.expect("fala");
    assert_eq!(fake.calls("gemini").len(), n + 1, "o primeiro trecho refeito uma vez");
    assert_eq!(style_of(&fake, 0), json!("calm and unhurried"));
    assert!((1..=n).all(|i| style_of(&fake, i) == Value::Null));
    assert_eq!(w.journal("voice.ok")[0]["spoken_tone"], "neutral");
}

/// Mais trechos que o teto: nenhuma chamada, e o texto segue inteiro.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_many_chunks_is_text_without_a_call() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chunk-many", &base);
    let text = "Uma frase inteira sobre o projeto. ".repeat(30 * (MAX_CHUNKS + 1));
    assert!(speech_chunks(&text, CHUNK_CHARS).len() > MAX_CHUNKS);
    assert_eq!(speak(&w.state, &w.nanty(), &text, Tone::Neutral).await.err(), Some(vec!["too_long"]));
    assert!(fake.calls("gemini").is_empty());
}

/// Revisão 1 (F-01): o texto que passa do que as três mensagens do Telegram mostram não fala, sem
/// chamada; o que cabe, fala. A duração máxima cabe com folga no que se guarda e numa nota (50 MB).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_text_past_what_telegram_shows_is_not_spoken() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chunk-units", &base);
    assert_eq!(MAX_SPOKEN_UNITS, 3 * 4096 - 512);
    let sentence = "Uma frase inteira sobre o projeto, com o detalhe. ";
    let over = sentence.repeat(MAX_SPOKEN_UNITS / sentence.len() + 2);
    assert!(format::units(over.trim()) > MAX_SPOKEN_UNITS && speech_chunks(&over, CHUNK_CHARS).len() <= MAX_CHUNKS);
    assert_eq!(speak(&w.state, &w.nanty(), &over, Tone::Neutral).await.err(), Some(vec!["too_long"]));
    assert!(fake.calls("gemini").is_empty(), "nenhuma chamada");
    let fits = sentence.repeat(MAX_SPOKEN_UNITS / sentence.len() - 1);
    assert!(format::units(fits.trim()) <= MAX_SPOKEN_UNITS && speech_chunks(&fits, CHUNK_CHARS).len() <= MAX_CHUNKS);
    let bytes = u64::from(MAX_SPEECH_SECS) * u64::try_from(encode::OPUS_BITRATE).unwrap_or(0) / 8;
    assert!(bytes < MAX_AUDIO_BYTES && bytes < 50 * 1024 * 1024, "{bytes}");
}

/// Revisão 1: o teto total é o prazo de um trecho por trecho, com o teto de trechos, nunca além do
/// limite do aviso.
#[test]
fn the_total_deadline_is_per_chunk_and_never_past_the_limit() {
    let per = Duration::from_secs(45);
    assert_eq!(total_deadline(per, 0, Duration::MAX), per);
    assert_eq!(total_deadline(per, 3, Duration::MAX), per * 3);
    assert_eq!(total_deadline(per, 3, Duration::from_secs(60)), Duration::from_secs(60));
    assert_eq!(total_deadline(per, MAX_CHUNKS + 5, Duration::MAX), per * u32::try_from(MAX_CHUNKS).unwrap_or(1));
}

/// Revisão 1: o prazo vale por trecho. O trecho que passa dele encerra a fala na hora, os seguintes
/// nem são pedidos, e o trecho encerrado entra no gasto pela estimativa.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chunk_past_its_deadline_ends_the_speech_without_the_rest() {
    let (fake, base) = fake_voice().await;
    let w = world_with_voice("chunk-late", &base);
    let tuning = w.nanty().telegram().tuning();
    w.nanty().telegram().set_tuning(crate::server::nanty::telegram::Tuning { speech_deadline: Duration::from_millis(300), ..tuning });
    let text = "Uma frase inteira sobre o projeto, com o detalhe que importa. ".repeat(48);
    assert!(speech_chunks(&text, CHUNK_CHARS).len() >= 3);
    // O primeiro trecho responde; o segundo fica preso no provedor.
    fake.hold_speech();
    fake.release_speech(1);
    let started = std::time::Instant::now();
    assert_eq!(speak(&w.state, &w.nanty(), &text, Tone::Neutral).await.err(), Some(vec!["deadline"]));
    assert!(started.elapsed() < Duration::from_secs(3), "o texto sai na hora: {:?}", started.elapsed());
    let failed = w.journal("voice.failed");
    assert_eq!((failed[0]["reason"].clone(), failed[0]["chunk"].clone()), (json!("deadline"), json!(1)));
    // Soltas as falas presas, nenhum trecho a mais é pedido nem cobrado.
    fake.release_speech(MAX_CHUNKS);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(fake.calls("gemini").len(), 2, "os trechos que faltavam não saem");
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    assert_eq!(spend.voice_calls, 2, "o trecho falado e o encerrado, pela estimativa");
    assert!(w.journal("voice.ok").is_empty());
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
    store.conversation().retain(|m| m.audio.is_none()).expect("retain");
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
    store.conversation().retain(|m| m.audio.is_none()).expect("retain");
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
    let m = crate::server::nanty::chat::person_message_with(&nanty, &crate::config::NantyConfig::default(), &secrets, Channel::Telegram, Some(&raw), true, None).expect("mensagem");
    assert_eq!((m.from, m.voice, m.text.as_deref()), (Speaker::Person, true, Some("minha chave é ***")));
    // A nota que não virou texto entra sem texto, com a marca.
    let empty = crate::server::nanty::chat::person_message_with(&nanty, &crate::config::NantyConfig::default(), &secrets, Channel::Telegram, None, true, None).expect("nota");
    assert_eq!((empty.voice, empty.text), (true, None));
    assert!(crate::server::nanty::chat::person_message_with(&nanty, &crate::config::NantyConfig::default(), &secrets, Channel::Telegram, None, false, None).is_none());
    let line = serde_json::to_string(&nanty.conversation()).expect("json");
    assert!(!line.contains(&secret) && line.contains("\"voice\":true"), "{line}");
}
