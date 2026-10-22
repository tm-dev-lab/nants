//! Geração de imagens pela ferramenta hospedada (spec 090 T01): corpo, parse, recusa, ritmo e
//! repetição. Servidores locais falsos; nenhuma espera real.

use super::super::retry_tests::{http, server};
use super::super::tests::{body_of, head_of, spawn_server, KEY};
use super::super::{base64_encode, provider_failure, PaceClock, Pacer, RetrySleep, BAD_RESPONSE_RETRIES, THROTTLE_DELAYS};
use super::*;
use std::sync::{Arc, Mutex};

/// Assinatura PNG + alguns bytes, e o cabeçalho RIFF/WEBP: o cliente não valida o conteúdo, só
/// devolve os bytes decodificados.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR-fake";
const WEBP: &[u8] = b"RIFF\x10\x00\x00\x00WEBPVP8 fake";
/// Texto remoto que nunca pode aparecer em erro ou `Debug`.
const REMOTE: &str = "TEXTO-REMOTO-SECRETO";

fn responses_client(base: String) -> LlmClient {
    LlmClient::new(base, KEY.into()).expect("client").with_wire_api(WireApi::Responses)
}

/// Resposta completa com um item `image_generation_call` (campos extras em `extra`).
fn image_reply(result: &str, extra: &str) -> &'static str {
    let body = format!(
        r#"{{"id":"resp_i","object":"response","status":"completed","error":null,"output":[{{"type":"image_generation_call","id":"ig_1","status":"completed","result":"{result}"{extra}}}],"usage":{{"input_tokens":20,"output_tokens":30,"total_tokens":50}}}}"#
    );
    Box::leak(body.into_boxed_str())
}

fn opts() -> ImageOptions {
    ImageOptions {
        size: ImageSize::Landscape,
        background: ImageBackground::Transparent,
        quality: ImageQuality::High,
        format: ImageFormat::Png,
    }
}

#[tokio::test]
async fn request_body_forces_the_hosted_tool_without_reasoning_or_stream() {
    let reply = image_reply(&base64_encode(PNG), r#","output_format":"png""#);
    let (base, cap) = spawn_server(vec![(200, reply)]).await;
    let img = responses_client(base).generate_image("gpt-x", "um gato astronauta", &opts()).await.expect("imagem");
    assert_eq!(img.bytes, PNG);
    let reqs = cap.lock().expect("lock");
    assert_eq!(reqs.len(), 1);
    assert!(head_of(&reqs[0]).starts_with("POST /v1/responses "), "{}", head_of(&reqs[0]));
    assert!(head_of(&reqs[0]).contains(&format!("authorization: Bearer {KEY}")));
    let expected = serde_json::json!({
        "model": "gpt-x",
        "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "um gato astronauta" }] }],
        "store": false,
        "tools": [{
            "type": "image_generation",
            "size": "1536x1024",
            "background": "transparent",
            "quality": "high",
            "output_format": "png",
        }],
        "tool_choice": { "type": "image_generation" },
    });
    assert_eq!(body_of(&reqs[0]), expected);
}

#[tokio::test]
async fn parses_png_and_webp_with_usage_and_truncated_revised_prompt() {
    let long = "r".repeat(REVISED_PROMPT_MAX_CHARS + 50);
    let png = image_reply(&base64_encode(PNG), &format!(r#","output_format":"png","revised_prompt":"{long}""#));
    // Sem `output_format` no item: vale o formato pedido.
    let webp = image_reply(&base64_encode(WEBP), "");
    let (base, _) = spawn_server(vec![(200, png), (200, webp)]).await;
    let c = responses_client(base);
    let img = c.generate_image("m", "p", &opts()).await.expect("png");
    assert_eq!((img.bytes.as_slice(), img.format), (PNG, ImageFormat::Png));
    assert_eq!(img.usage, Some(Usage::new(20, 30)));
    assert_eq!(img.revised_prompt.as_deref().map(|s| s.chars().count()), Some(REVISED_PROMPT_MAX_CHARS));
    let webp_opts = ImageOptions { format: ImageFormat::Webp, ..opts() };
    let img = c.generate_image("m", "p", &webp_opts).await.expect("webp");
    assert_eq!((img.bytes.as_slice(), img.format, img.revised_prompt), (WEBP, ImageFormat::Webp, None));
    // `Debug` não mostra bytes nem o prompt reescrito.
    let png = image_reply(&base64_encode(PNG), &format!(r#","revised_prompt":"{REMOTE}""#));
    let (base, _) = spawn_server(vec![(200, png)]).await;
    let img = responses_client(base).generate_image("m", "p", &opts()).await.expect("png");
    assert_eq!(img.revised_prompt.as_deref(), Some(REMOTE));
    assert!(!format!("{img:?}").contains(REMOTE));
}

#[tokio::test]
async fn skips_failed_calls_and_takes_the_first_good_one() {
    let body = format!(
        r#"{{"status":"completed","output":[{{"type":"message","content":[]}},{{"type":"image_generation_call","status":"failed","result":"{}"}},{{"type":"image_generation_call","status":"completed","result":"{}"}}]}}"#,
        base64_encode(WEBP),
        base64_encode(PNG)
    );
    let (base, _) = spawn_server(vec![(200, Box::leak(body.into_boxed_str()))]).await;
    let img = responses_client(base).generate_image("m", "p", &opts()).await.expect("imagem");
    assert_eq!(img.bytes, PNG);
    assert_eq!(img.usage, None);
}

/// Spec 094 T01 (mudança intencional, decisão 1): a resposta `completed` sem imagem legível deixou
/// de ser o `Empty` repetido pela 088 (cada repetição gerava e cobrava outra imagem) e passa a ser o
/// motivo próprio `image_unreadable`, NÃO repetível: exatamente 1 pedido HTTP por chamada.
#[tokio::test]
async fn missing_or_invalid_result_is_image_unreadable_and_not_repeated() {
    let missing: &'static str = r#"{"status":"completed","output":[{"type":"image_generation_call","status":"completed"}]}"#;
    let only_text: &'static str = r#"{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"não posso"}]}]}"#;
    let invalid = image_reply("@@não é base64@@", "");
    let empty = image_reply("", "");
    let padding = image_reply("QQ=A", "");
    for body in [missing, only_text, invalid, empty, padding] {
        let (base, cap) = spawn_server(vec![(200, body); 3]).await;
        let err = responses_client(base).generate_image("m", "p", &opts()).await.expect_err("sem imagem");
        assert!(matches!(err, LlmError::Parse(r) if r == IMAGE_UNREADABLE), "{body}");
        assert!(is_image_unreadable(&err) && !is_image_unsupported(&err));
        assert_ne!(BAD_RESPONSE_RETRIES, 0, "a 088 repetiria; aqui não");
        assert_eq!(cap.lock().expect("lock").len(), 1, "{body}: não se repete");
        assert_eq!(
            provider_failure(&err).describe(),
            "provider_bad_response (o provedor não devolveu uma imagem legível (pode ter cobrado))"
        );
    }
}

/// Item da OpenRouter com os campos dados (texto JSON já montado).
fn openrouter_reply(fields: &str) -> &'static str {
    let body = format!(
        r#"{{"status":"completed","output":[{{"type":"reasoning","status":"completed","content":[]}},{{"type":"openrouter:image_generation","id":"st_1","status":"completed"{fields}}},{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{REMOTE}"}}]}}],"usage":{{"input_tokens":7,"output_tokens":9,"total_tokens":16}}}}"#
    );
    Box::leak(body.into_boxed_str())
}

/// Fixture real da OpenRouter (estrutura da sonda de 2026-10-19, imagem trocada por PNG 1×1).
const OPENROUTER_FIXTURE: &str =
    include_str!("testdata/openrouter-image-response.json");
/// HTTP 400 real da DeepInfra para a ferramenta hospedada.
const DEEPINFRA_FIXTURE: &str =
    include_str!("testdata/deepinfra-image-refusal.json");

#[tokio::test]
async fn openrouter_fixture_yields_the_png_from_the_data_url() {
    let (base, cap) = spawn_server(vec![(200, OPENROUTER_FIXTURE)]).await;
    // Pedido em WebP: o formato vem do MIME do data URL (não há `output_format` no item).
    let o = ImageOptions { format: ImageFormat::Webp, background: ImageBackground::Opaque, ..opts() };
    let img = responses_client(base).generate_image("deepseek/x", "p", &o).await.expect("imagem");
    assert_eq!(cap.lock().expect("lock").len(), 1);
    assert!(img.bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "PNG decodificado");
    assert_eq!(crate::sdd::images::dimensions(&img.bytes), Some((1, 1)));
    assert_eq!(img.format, ImageFormat::Png);
    assert_eq!(img.usage, Some(Usage::new(1468, 149)));
    // O item da OpenRouter não tem `revised_prompt`; o `prompt` dele não é usado.
    assert_eq!(img.revised_prompt, None);
}

#[tokio::test]
async fn deepinfra_fixture_stays_image_unsupported() {
    let (base, cap) = spawn_server(vec![(400, DEEPINFRA_FIXTURE), (200, "{}")]).await;
    let err = responses_client(base).generate_image("m", "p", &opts()).await.expect_err("recusa");
    assert!(is_image_unsupported(&err), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn openrouter_data_urls_mime_formats_and_image_url_fallback() {
    let png = base64_encode(PNG);
    let webp = base64_encode(WEBP);
    let cases: [(String, &[u8], ImageFormat); 6] = [
        (format!(r#","result":"data:image/png;base64,{png}""#), PNG, ImageFormat::Png),
        (format!(r#","result":"data:image/webp;base64,{webp}""#), WEBP, ImageFormat::Webp),
        (format!(r#","result":"data:image/jpeg;base64,{png}""#), PNG, ImageFormat::Jpeg),
        (format!(r#","result":"data:image/jpg;base64,{png}""#), PNG, ImageFormat::Jpeg),
        // Sem `result`: o `imageUrl` vale quando é data URL.
        (format!(r#","imageUrl":"data:image/webp;base64,{webp}""#), WEBP, ImageFormat::Webp),
        // `output_format` vence o MIME.
        (format!(r#","output_format":"png","result":"data:image/webp;base64,{png}""#), PNG, ImageFormat::Png),
    ];
    for (fields, bytes, format) in cases {
        let (base, _) = spawn_server(vec![(200, openrouter_reply(&fields))]).await;
        let img = responses_client(base).generate_image("m", "p", &opts()).await.expect("imagem");
        assert_eq!((img.bytes.as_slice(), img.format), (bytes, format), "{fields}");
        assert_eq!(img.usage, Some(Usage::new(7, 9)));
    }
    // Base64 puro no item da OpenRouter: formato do pedido.
    let (base, _) = spawn_server(vec![(200, openrouter_reply(&format!(r#","result":"{png}""#)))]).await;
    let o = ImageOptions { format: ImageFormat::Webp, ..opts() };
    let img = responses_client(base).generate_image("m", "p", &o).await.expect("puro");
    assert_eq!((img.bytes.as_slice(), img.format), (PNG, ImageFormat::Webp));
}

#[tokio::test]
async fn unreadable_openrouter_items_are_image_unreadable_with_one_request() {
    let png = base64_encode(PNG);
    let cases = [
        // URL http(s) nunca é baixada, nem no `result` nem no `imageUrl`.
        r#","imageUrl":"https://cdn.example.com/x.webp""#.to_string(),
        r#","result":"https://cdn.example.com/x.webp""#.to_string(),
        r#","imageUrl":"http://127.0.0.1:1/x.png""#.to_string(),
        // MIME fora da lista, sem `;base64` ou base64 inválido.
        format!(r#","result":"data:image/gif;base64,{png}""#),
        format!(r#","result":"data:image/svg+xml;base64,{png}""#),
        format!(r#","result":"data:text/plain;base64,{png}""#),
        format!(r#","result":"data:image/png,{png}""#),
        r#","result":"data:image/png;base64,@@@""#.to_string(),
        r#","result":"data:image/png;base64""#.to_string(),
        // `result` ilegível: o `imageUrl` não é consultado.
        format!(r#","result":"data:image/gif;base64,{png}","imageUrl":"data:image/png;base64,{png}""#),
        String::new(),
    ];
    for fields in cases {
        let (base, cap) = spawn_server(vec![(200, openrouter_reply(&fields)); 3]).await;
        let err = responses_client(base).generate_image("m", "p", &opts()).await.expect_err("ilegível");
        assert!(matches!(err, LlmError::Parse(r) if r == IMAGE_UNREADABLE), "{fields}: {err:?}");
        assert_eq!(cap.lock().expect("lock").len(), 1, "{fields}");
        let shown = format!("{err:?}{err}{}", provider_failure(&err).describe());
        assert!(!shown.contains("example.com") && !shown.contains(REMOTE) && !shown.contains("http"), "{shown}");
    }
    // Item da OpenRouter que falhou: ignorado, mesmo com imagem.
    let failed = format!(
        r#"{{"status":"completed","output":[{{"type":"openrouter:image_generation","status":"failed","result":"data:image/png;base64,{png}"}}]}}"#
    );
    let (base, cap) = spawn_server(vec![(200, Box::leak(failed.into_boxed_str()))]).await;
    let err = responses_client(base).generate_image("m", "p", &opts()).await.expect_err("falhou");
    assert!(is_image_unreadable(&err));
    assert_eq!(cap.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn image_generation_call_is_preferred_over_the_openrouter_item() {
    let body = format!(
        r#"{{"status":"completed","output":[{{"type":"openrouter:image_generation","status":"completed","result":"data:image/webp;base64,{}"}},{{"type":"image_generation_call","status":"completed","result":"{}"}}]}}"#,
        base64_encode(WEBP),
        base64_encode(PNG)
    );
    let (base, _) = spawn_server(vec![(200, Box::leak(body.into_boxed_str()))]).await;
    let img = responses_client(base).generate_image("m", "p", &opts()).await.expect("imagem");
    assert_eq!((img.bytes.as_slice(), img.format), (PNG, ImageFormat::Png));
    // `image_generation_call` ilegível: o item da OpenRouter legível é aproveitado.
    let body = format!(
        r#"{{"status":"completed","output":[{{"type":"image_generation_call","status":"completed","result":"@@"}},{{"type":"openrouter:image_generation","status":"completed","result":"data:image/webp;base64,{}"}}]}}"#,
        base64_encode(WEBP)
    );
    let (base, _) = spawn_server(vec![(200, Box::leak(body.into_boxed_str()))]).await;
    let img = responses_client(base).generate_image("m", "p", &opts()).await.expect("imagem");
    assert_eq!((img.bytes.as_slice(), img.format), (WEBP, ImageFormat::Webp));
}

/// Spec 094 T01: a resposta OpenAI e a Azure (mesma forma `image_generation_call`, base64 puro)
/// produzem exatamente a mesma imagem de antes: bytes, formato, `revised_prompt` e `usage`.
#[tokio::test]
async fn openai_and_azure_replies_are_equivalent_and_unchanged() {
    let openai = image_reply(&base64_encode(PNG), r#","output_format":"png","revised_prompt":"gato""#);
    // Azure: mesmos campos, com o item extra de filtro de conteúdo e outra ordem de chaves.
    let azure: &'static str = Box::leak(
        format!(
            r#"{{"object":"response","status":"completed","error":null,"output":[{{"type":"image_generation_call","status":"completed","revised_prompt":"gato","output_format":"png","result":"{}","id":"ig_az"}}],"usage":{{"total_tokens":50,"output_tokens":30,"input_tokens":20}},"content_filters":[]}}"#,
            base64_encode(PNG)
        )
        .into_boxed_str(),
    );
    let expected = GeneratedImage {
        bytes: PNG.to_vec(),
        format: ImageFormat::Png,
        revised_prompt: Some("gato".into()),
        usage: Some(Usage::new(20, 30)),
    };
    for body in [openai, azure] {
        let (base, cap) = spawn_server(vec![(200, body)]).await;
        let img = responses_client(base).generate_image("m", "p", &opts()).await.expect("imagem");
        assert_eq!(img, expected, "{body}");
        assert_eq!(cap.lock().expect("lock").len(), 1);
        assert_eq!(parse_image(body, ImageFormat::Webp).expect("parse"), expected);
    }
}

/// Spec 094 T01: o corpo do pedido não muda para nenhum provedor (igualdade com o JSON de antes).
#[test]
fn request_body_is_unchanged() {
    let body = image_body("deepseek/deepseek-v4.1-flash-1017", "uma estrela", &ImageOptions::default());
    let expected = serde_json::json!({
        "model": "deepseek/deepseek-v4.1-flash-1017",
        "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "uma estrela" }] }],
        "store": false,
        "tools": [{
            "type": "image_generation",
            "size": "auto",
            "background": "auto",
            "quality": "medium",
            "output_format": "webp",
        }],
        "tool_choice": { "type": "image_generation" },
    });
    assert_eq!(body, expected);
}

#[test]
fn base64_decoder_accepts_standard_and_rejects_garbage() {
    for len in 0..12 {
        let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
        let enc = base64_encode(&data);
        let dec = base64_decode(&enc);
        assert_eq!(dec, (!data.is_empty()).then(|| data.clone()), "{enc}");
        // Sem padding e com quebras de linha também vale.
        let bare = format!("{}\n", enc.trim_end_matches('='));
        assert_eq!(base64_decode(&bare), (!data.is_empty()).then_some(data), "{bare}");
    }
    for bad in ["A", "A===", "QQ=A", "Q-==", "QUJD*", "=", "QUJDRA=", "QUJD=="] {
        assert_eq!(base64_decode(bad), None, "{bad}");
    }
}

#[tokio::test]
async fn http_400_refusing_the_tool_capability_is_image_unsupported_and_not_repeated() {
    let bodies: [&'static str; 4] = [
        r#"{"error":{"message":"Tool type 'image_generation' is not supported for this model. TEXTO-REMOTO-SECRETO","type":"invalid_request_error","param":"tools[0].type","code":null}}"#,
        r#"{"error":{"message":"image_generation is not enabled for this deployment.","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#,
        r#"{"error":{"message":"The model does not support image generation.","type":"invalid_request_error"}}"#,
        r#"{"error":{"message":"Unsupported tool type.","type":"invalid_request_error","param":"tools[0].type","code":"unsupported_value"}}"#,
    ];
    for body in bodies {
        let (base, cap) = spawn_server(vec![(400, body), (200, "{}")]).await;
        let err = responses_client(base).generate_image("m", "p", &opts()).await.expect_err("recusa");
        assert!(is_image_unsupported(&err), "{body}: {err:?}");
        assert!(matches!(err, LlmError::Parse(r) if r == IMAGE_UNSUPPORTED));
        assert_eq!(cap.lock().expect("lock").len(), 1, "a recusa não se repete");
        assert_eq!(provider_failure(&err).describe(), "provider_bad_response (geração de imagens indisponível)");
        assert!(!format!("{err:?}{err}").contains(REMOTE));
    }
}

#[tokio::test]
async fn http_400_about_options_values_or_moderation_is_not_a_capability_refusal() {
    let bodies: [&'static str; 6] = [
        r#"{"error":{"message":"Transparent background is not supported for output_format jpeg","type":"invalid_request_error","param":"tools[0].background","code":"unsupported_value"}}"#,
        r#"{"error":{"message":"Unsupported value: 'quality' does not support 'ultra'.","type":"invalid_request_error","param":"tools[0].quality","code":"unsupported_value"}}"#,
        r#"{"error":{"message":"Your request was rejected by the image generation safety system. TEXTO-REMOTO-SECRETO","type":"invalid_request_error","param":null,"code":"moderation_blocked"}}"#,
        r#"{"error":{"message":"Invalid size for image_generation: '999x999' is not supported.","type":"invalid_request_error","param":"tools[0].size","code":"invalid_value"}}"#,
        r#"{"error":{"message":"The image_generation prompt violates the content policy (unsupported content).","type":"content_policy_violation","param":null,"code":null}}"#,
        r#"{"error":{"message":"Unsupported value for image_generation output.","type":"invalid_request_error","param":"input","code":"unsupported_value"}}"#,
    ];
    for body in bodies {
        let (base, cap) = spawn_server(vec![(400, body), (200, "{}")]).await;
        let err = responses_client(base).generate_image("m", "p", &opts()).await.expect_err("erro");
        assert!(!is_image_unsupported(&err), "{body}");
        assert_eq!(provider_failure(&err).to_string(), "provider_http_400", "{body}");
        assert_eq!(cap.lock().expect("lock").len(), 1, "{body}");
        assert!(!format!("{err:?}{err}").contains(REMOTE));
    }
    // Corpo que não é JSON nunca vira recusa, mesmo citando a ferramenta.
    assert!(!rejects_image_generation("image_generation is not supported"));
}

#[tokio::test]
async fn transparent_jpeg_is_sent_and_reported_as_png() {
    let reply = image_reply(&base64_encode(PNG), "");
    let (base, cap) = spawn_server(vec![(200, reply)]).await;
    let o = ImageOptions { background: ImageBackground::Transparent, format: ImageFormat::Jpeg, ..opts() };
    let img = responses_client(base).generate_image("m", "p", &o).await.expect("imagem");
    assert_eq!(img.format, ImageFormat::Png);
    let body = body_of(&cap.lock().expect("lock")[0]);
    assert_eq!(body["tools"][0]["output_format"], "png");
    assert_eq!(body["tools"][0]["background"], "transparent");
    // Fundo opaco mantém o JPEG.
    let o = ImageOptions { background: ImageBackground::Opaque, format: ImageFormat::Jpeg, ..opts() };
    assert_eq!(effective_format(&o), ImageFormat::Jpeg);
}

#[tokio::test]
async fn other_http_errors_keep_the_fixed_code_without_remote_text() {
    let not_found: &'static str = r#"{"error":{"message":"model image_generation TEXTO-REMOTO-SECRETO not found"}}"#;
    let unrelated: &'static str = r#"{"error":{"message":"prompt too long TEXTO-REMOTO-SECRETO"}}"#;
    for (status, body) in [(404, not_found), (400, unrelated), (401, unrelated)] {
        let (base, cap) = spawn_server(vec![(status, body), (200, "{}")]).await;
        let err = responses_client(base).generate_image("m", "p", &opts()).await.expect_err("erro");
        assert!(!is_image_unsupported(&err), "{status}");
        assert_eq!(provider_failure(&err).to_string(), format!("provider_http_{status}"));
        assert_eq!(cap.lock().expect("lock").len(), 1, "{status}: não se repete");
        assert!(!format!("{err:?}{err}").contains(REMOTE), "{status}");
    }
}

#[tokio::test]
async fn chat_wire_is_unsupported_without_any_request() {
    let (base, cap) = spawn_server(vec![(200, "{}")]).await;
    let c = LlmClient::new(base, KEY.into()).expect("client");
    let err = c.generate_image("m", "p", &opts()).await.expect_err("chat");
    assert!(is_image_unsupported(&err));
    assert!(cap.lock().expect("lock").is_empty());
}

/// Espera que só registra a duração pedida.
fn recorder() -> (RetrySleep, Arc<Mutex<Vec<Duration>>>) {
    let waits = Arc::new(Mutex::new(Vec::new()));
    let w = Arc::clone(&waits);
    let sleep: RetrySleep = Arc::new(move |d| {
        w.lock().expect("lock").push(d);
        Box::pin(async {})
    });
    (sleep, waits)
}

#[tokio::test]
async fn throttle_429_and_no_capacity_are_repeated_with_notices() {
    let ok = image_reply(&base64_encode(PNG), "");
    let no_capacity = r#"{"error":{"code":"no_capacity","message":"TEXTO-REMOTO-SECRETO"}}"#;
    let (base, count) = server(vec![
        http(429, "", "application/json", "{}"),
        http(200, "", "application/json", no_capacity),
        http(200, "", "application/json", ok),
    ])
    .await;
    let (sleep, waits) = recorder();
    let c = responses_client(base).with_retry_sleep(sleep);
    let notices = Mutex::new(Vec::new());
    let img = c
        .generate_image_noticed("m", "p", &opts(), |n| notices.lock().expect("lock").push(n))
        .await
        .expect("imagem depois do limite");
    assert_eq!(img.bytes, PNG);
    assert_eq!(*count.lock().expect("lock"), 3);
    assert_eq!(waits.lock().expect("lock").as_slice(), &THROTTLE_DELAYS[..2]);
    let notices = notices.lock().expect("lock");
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices[1].contains("no_capacity"), "{notices:?}");
    assert!(!notices.iter().any(|n| n.contains(REMOTE)));
}

#[tokio::test]
async fn transient_5xx_is_repeated() {
    let ok = image_reply(&base64_encode(PNG), "");
    let (base, count) = server(vec![http(503, "", "application/json", "{}"), http(200, "", "application/json", ok)]).await;
    let (sleep, _) = recorder();
    let img = responses_client(base).with_retry_sleep(sleep).generate_image("m", "p", &opts()).await.expect("ok");
    assert_eq!(img.bytes, PNG);
    assert_eq!(*count.lock().expect("lock"), 2);
}

struct FixedClock;

impl PaceClock for FixedClock {
    fn now(&self) -> Duration {
        Duration::from_secs(1)
    }
}

#[tokio::test]
async fn goes_through_the_pacer_with_image_estimate_and_real_usage() {
    let with_usage = image_reply(&base64_encode(PNG), "");
    let without_usage = format!(
        r#"{{"status":"completed","output":[{{"type":"image_generation_call","status":"completed","result":"{}"}}]}}"#,
        base64_encode(PNG)
    );
    let (base, _) = server(vec![
        http(200, "", "application/json", with_usage),
        http(200, "", "application/json", &without_usage),
        http(429, "", "application/json", "{}"),
    ])
    .await;
    let pacer = Pacer::new(Arc::new(FixedClock));
    let (sleep, _) = recorder();
    let c = responses_client(base).with_retry_sleep(sleep).with_throttle_delays(Vec::new()).with_pacer(Arc::clone(&pacer), "conn");
    c.generate_image("m", "p", &opts()).await.expect("1ª");
    let prompt = "x".repeat(400);
    c.generate_image("m", &prompt, &opts()).await.expect("2ª");
    let tokens: Vec<u64> = pacer.window("conn", "m").iter().map(|(_, t)| *t).collect();
    // `usage` real (20 + 30) na 1ª; sem `usage`, fica a estimativa: texto/4 + saída da imagem.
    assert_eq!(tokens, vec![50, 100 + IMAGE_OUTPUT_TOKENS]);
    assert_eq!(pacer.ceilings("conn", "m"), (None, None));
    let err = c.generate_image("m", "p", &opts()).await.expect_err("limite");
    assert_eq!(provider_failure(&err).to_string(), "provider_http_429");
    assert_ne!(pacer.ceilings("conn", "m"), (None, None), "o limite ensina o ritmo do par");
}

/// Spec 091: o erro HTTP do caminho de imagem leva o `error.code` (ou, na falta, o `error.type`)
/// saneado; nunca a mensagem. O `Display` continua `provider_http_NNN`.
#[tokio::test]
async fn http_errors_carry_the_sanitized_provider_code_without_the_message() {
    let cases: [(u16, &'static str, &str); 6] = [
        (
            400,
            r#"{"error":{"code":"invalid_value","type":"invalid_request_error","message":"SEGREDO TEXTO-REMOTO-SECRETO"}}"#,
            "provider_http_400 (código: invalid_value)",
        ),
        (
            403,
            r#"{"error":{"code":null,"type":"PermissionDenied","message":"TEXTO-REMOTO-SECRETO"}}"#,
            "provider_http_403 (código: permissiondenied)",
        ),
        // Código com cara de chave: omitido (sem `type`, fica só o status).
        (400, r#"{"error":{"code":"sk-proj-abc123def456ghi789","message":"TEXTO-REMOTO-SECRETO"}}"#, "provider_http_400"),
        // Código fora do vocabulário (espaços, URL): omitido.
        (401, r#"{"error":{"code":"https://h/v1 chave TEXTO-REMOTO-SECRETO"}}"#, "provider_http_401"),
        // Sem objeto `error` ou sem JSON: só o status.
        (404, r#"{"code":"model_not_found","message":"TEXTO-REMOTO-SECRETO"}"#, "provider_http_404"),
        (400, "TEXTO-REMOTO-SECRETO", "provider_http_400"),
    ];
    for (status, body, described) in cases {
        let (base, _cap) = spawn_server(vec![(status, body), (200, "{}")]).await;
        let err = responses_client(base).generate_image("m", "p", &opts()).await.expect_err("erro");
        let failure = provider_failure(&err);
        assert_eq!(failure.to_string(), format!("provider_http_{status}"), "{body}");
        assert_eq!(failure.describe(), described, "{body}");
        let shown = format!("{err:?}{err}{failure:?}{}", failure.describe());
        assert!(!shown.contains(REMOTE) && !shown.contains("SEGREDO") && !shown.contains("sk-proj"), "{shown}");
    }
}
