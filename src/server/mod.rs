//! Servidor HTTP local (axum): API REST e controles de segurança.
//!
//! Requisitos: bind padrão em loopback, validação de `Host`/`Origin`,
//! `Content-Type` JSON em mutações, limite de corpo e chaves sempre mascaradas.

pub mod advisor;
pub mod app;
pub mod app_guide;
pub mod approvals;
pub mod arbiter;
pub mod architecture;
pub mod attention;
pub mod browser_open;
pub mod captures;
pub mod survey;
pub mod tools_trail;
pub mod chat;
pub mod cost_alert;
pub mod chatgpt;
pub mod chat_attachments;
pub mod choices;
pub mod config_api;
pub mod console;
pub mod devices;
pub mod dialog;
pub mod doctor;
pub mod envs;
pub mod error;
pub mod events;
pub mod file_explorer;
pub mod fs_browse;
pub mod image_gen;
pub mod judge;
pub mod lan;
pub mod lang;
pub mod live_activity;
pub mod models_api;
pub mod models_native;
pub mod netinfo;
pub mod plan_changes;
pub mod plan_consensus;
pub mod plan_review;
pub mod portproxy;
pub mod price_catalog;
pub mod prereqs;
pub mod projects;
pub mod projects_root;
pub mod readiness;
pub mod recovery;
pub mod report;
pub mod runs;
pub mod safe_fs;
pub mod snapshot;
pub mod security;
pub mod spec_result;
pub mod usage;
pub mod state;
pub mod static_ui;
pub mod ui_state;

use axum::extract::DefaultBodyLimit;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use tokio::net::TcpListener;

pub use state::AppState;

/// Limite do corpo das requisições (256 KiB).
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Monta o roteador com todos os endpoints e middlewares de segurança.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/config", get(config_api::get_config))
        .route("/api/config/connections", put(config_api::put_connection))
        .route("/api/config/agents", put(config_api::put_agents))
        .route("/api/config/pricing", put(config_api::put_pricing))
        .route("/api/config/pricing/estimate", get(config_api::get_estimate))
        .route("/api/config/pricing/autofill", post(config_api::post_autofill))
        .route("/api/config/projects-root", put(config_api::put_projects_root))
        .route("/api/config/network", get(lan::get_network).put(lan::put_network))
        .route("/api/projects/:id/usage", get(usage::get_usage))
        .route("/api/projects/:id/specs/:spec/budget", put(usage::put_budget))
        .route(
            "/api/config/connections/test",
            post(config_api::test_connection),
        )
        .route(
            "/api/config/connections/:id/models",
            get(models_api::list_models),
        )
        // Conexão pelo plano ChatGPT (spec 116): início do login, retorno (passagem e conclusão,
        // spec 118), estado e sair.
        .route(chatgpt::START_PATH, post(chatgpt::post_start))
        .route(chatgpt::CALLBACK_PATH, get(chatgpt::get_callback))
        .route(chatgpt::FINISH_PATH, get(chatgpt::get_finish))
        .route("/api/connections/:id/chatgpt", get(chatgpt::get_status))
        .route("/api/connections/:id/chatgpt/logout", post(chatgpt::post_logout))
        .route("/api/projects", post(projects::create_project))
        .route("/api/projects/:id", delete(projects::remove_project))
        .route("/api/projects/:id/state", get(projects::get_state))
        .route("/api/projects/:id/activity", get(live_activity::get_activity))
        .route("/api/projects/:id/files", get(file_explorer::list_files))
        .route("/api/projects/:id/file", get(file_explorer::get_file))
        // Janela de execução: todo erro leva o próximo passo (`action`, spec 130 T04).
        .route("/api/projects/:id/app", get(app::get_app).layer(from_fn(app_guide::attach_action)))
        .route("/api/projects/:id/app/start", post(app::start_app).layer(from_fn(app_guide::attach_action)))
        .route("/api/projects/:id/app/stop", post(app::stop_app).layer(from_fn(app_guide::attach_action)))
        .route("/api/projects/:id/app/logs", get(app::get_logs).layer(from_fn(app_guide::attach_action)))
        // Console do projeto (spec 130 T03).
        .route(
            "/api/projects/:id/app/exec",
            get(console::get_exec).post(console::exec_app).layer(from_fn(app_guide::attach_action)),
        )
        .route("/api/projects/:id/app/exec/stop", post(console::stop_exec).layer(from_fn(app_guide::attach_action)))
        // Conteúdo da janela e "Pedir ao Arquiteto para corrigir" (spec 130 T04).
        .route("/api/projects/:id/app/guide/:spec_id", get(app_guide::get_guide).layer(from_fn(app_guide::attach_action)))
        .route("/api/projects/:id/app/fix", post(app_guide::post_fix).layer(from_fn(app_guide::attach_action)))
        .route("/api/projects/:id/settings", put(projects::put_settings))
        .route("/api/projects/:id/yolo", put(projects::put_yolo))
        .route("/api/projects/:id/internet", put(projects::put_internet))
        .route(
            "/api/projects/:id/specs/:spec_id/result",
            get(spec_result::get_result),
        )
        .route(
            "/api/projects/:id/specs/:spec_id/captures/:name",
            get(captures::get_capture),
        )
        .route(
            "/api/projects/:id/specs/:spec_id/dialog",
            get(dialog::get_dialog),
        )
        .route(
            "/api/projects/:id/specs/:spec_id/tools",
            get(tools_trail::get_tools),
        )
        .route(
            "/api/projects/:id/specs/:spec_id/report",
            get(report::get_report),
        )
        .route(
            "/api/projects/:id/specs/:spec_id/approve",
            post(runs::approve_spec),
        )
        .route(
            "/api/projects/:id/specs/:spec_id/resume",
            post(runs::resume_spec),
        )
        .route(
            "/api/projects/:id/specs/:spec_id/stop",
            post(runs::stop_spec),
        )
        .route(
            "/api/projects/:id/specs/:spec_id/tasks/:task_id/recovery-choice",
            post(runs::recovery_choice),
        )
        .route(
            "/api/projects/:id/permissions/:req_id",
            post(approvals::resolve_permission),
        )
        .route("/api/projects/:id/architecture", get(survey::get_architecture))
        .route("/api/projects/:id/architecture/survey", post(survey::post_survey))
        .route("/api/projects/:id/chat", get(chat::get_chat).post(chat::post_chat))
        .route("/api/projects/:id/chat/continue", post(chat::post_chat_continue))
        // Limite de corpo próprio (5 MiB) só nesta rota; o global continua em `MAX_BODY_BYTES`.
        .route(
            "/api/projects/:id/chat/attachments",
            post(chat_attachments::post_attachment)
                .layer(DefaultBodyLimit::max(chat_attachments::MAX_ATTACHMENT_BYTES)),
        )
        .route(
            "/api/projects/:id/chat/attachments/:name",
            get(chat_attachments::get_attachment),
        )
        // Estado da interface (spec 093): teto de 64 KiB por documento, também no corpo do PUT.
        .route(
            "/api/ui-state",
            get(ui_state::get_global)
                .put(ui_state::put_global)
                .layer(DefaultBodyLimit::max(ui_state::UI_STATE_MAX_BYTES)),
        )
        .route(
            "/api/projects/:id/ui-state",
            get(ui_state::get_project)
                .put(ui_state::put_project)
                .layer(DefaultBodyLimit::max(ui_state::UI_STATE_MAX_BYTES)),
        )
        .route("/api/attention", get(attention::get_attention))
        .route("/api/system/prereqs", get(prereqs::get_prereqs))
        .route("/api/readiness", get(readiness::get_readiness))
        .route("/api/readiness/refresh", post(readiness::post_refresh))
        .route("/api/fs/browse", get(fs_browse::browse))
        .route("/api/events", get(events::stream_events))
        // Dispositivos autorizados na rede local (spec 112).
        .route(devices::PAIR_LINK_PATH, get(devices::get_pair_link))
        .route(devices::PAIR_API_PATH, post(devices::post_pair))
        .route(
            "/api/devices",
            get(devices::get_devices).delete(devices::delete_devices),
        )
        .route(
            "/api/devices/code",
            get(devices::get_code).post(devices::post_code).delete(devices::delete_code),
        )
        .route(
            "/api/devices/:id",
            patch(devices::patch_device).delete(devices::delete_device),
        )
        .fallback(static_ui::static_handler)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(from_fn_with_state(state.clone(), lang::capture))
        // Depois da guarda de Host/Origin: em modo rede, exige dispositivo autorizado.
        .layer(from_fn_with_state(state.clone(), devices::require_device))
        .layer(from_fn_with_state(state.clone(), security::guard))
        .layer(from_fn(security::finalize))
        .with_state(state)
}

/// Serve até Ctrl-C, com encerramento gracioso das conexões em andamento.
pub async fn serve(listener: TcpListener, state: AppState) -> std::io::Result<()> {
    serve_with_shutdown(listener, state, shutdown_signal()).await
}

/// Serve até `shutdown` completar. Ao completar, aciona `AppState::trigger_shutdown`
/// antes do encerramento gracioso, para que streams SSE abertos também terminem.
pub async fn serve_with_shutdown<F>(
    listener: TcpListener,
    state: AppState,
    shutdown: F,
) -> std::io::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let trigger = state.clone();
    let apps = state.apps().clone();
    let env_state = state.clone();
    let grace = state.shutdown_grace();
    // Spec 128 T05: renumeração interrompida por uma queda abrupta é desfeita antes de servir.
    runs::recover_renumber_journals(&state).await;
    // Com a origem da conexão: a camada de dispositivos reconhece o loopback de verdade.
    let app = router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            trigger.trigger_shutdown();
        })
        .await;
    // Runs ativos recebem o cancelamento e gravam a mensagem final: espera por eles (com folga).
    runs::wait_active_runs(grace + RUNS_SHUTDOWN_SLACK).await;
    // Apps de teste não sobrevivem ao Nants (espera limitada: carência + SIGKILL).
    let _ = tokio::time::timeout(
        app::STOP_GRACE * 2 + RUNS_SHUTDOWN_SLACK,
        apps.stop_all(app::REASON_SHUTDOWN),
    )
    .await;
    // Ambientes Docker: `down` (sem -v) do -test e do -run de cada projeto, em tempo limitado.
    let projects = envs::registered_projects(&env_state).await;
    let provider = env_state.compose().clone();
    let _ = tokio::time::timeout(
        envs::CLEANUP_TIMEOUT,
        tokio::task::spawn_blocking(move || envs::shutdown_all(&provider, &projects)),
    )
    .await;
    // Grava o que ainda está na fila do diálogo (no máximo FLUSH_TIMEOUT).
    dialog::flush(dialog::FLUSH_TIMEOUT);
    result
}

/// Folga, além da carência de cancelamento, para as tarefas de run terminarem no encerramento.
const RUNS_SHUTDOWN_SLACK: std::time::Duration = std::time::Duration::from_secs(3);

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, Response, StatusCode};
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    use super::*;
    use crate::config::{AgentConfig, Config, Connection, Effort};

    const PORT: u16 = 3000;
    const HOST: &str = "127.0.0.1:3000";
    const SECRET: &str = "sk-SEGREDO-LITERAL-1234";
    const OK_CHAT: &str = r#"{"choices":[{"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}]}"#;

    /// Diretório temporário único por teste.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-server-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn sample_connection(id: &str, key: &str) -> Connection {
        Connection {
            id: id.into(),
            name: format!("Conexão {id}"),
            base_url: "https://api.example.com/v1".into(),
            api_key: key.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        }
    }

    fn sample_config() -> Config {
        let mut cfg = Config::default();
        cfg.connections.push(sample_connection("literal", SECRET));
        cfg.connections
            .push(sample_connection("envkey", "env:NANTS_SERVER_TEST_KEY"));
        cfg.agents.architect = AgentConfig {
            connection_id: "literal".into(),
            model: "gpt-4o".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        cfg
    }

    fn state_in(dir: &PathBuf, cfg: Config) -> AppState {
        AppState::new(dir.join("config.toml"), cfg, PORT)
    }

    fn request(method: Method, uri: &str, body: &str, content_type: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, HOST);
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        builder
            .body(Body::from(body.to_string()))
            .expect("requisição")
    }

    fn get_req(uri: &str) -> Request<Body> {
        request(Method::GET, uri, "", None)
    }

    async fn send(state: &AppState, req: Request<Body>) -> (StatusCode, Response<Body>) {
        let resp = router(state.clone()).oneshot(req).await.expect("resposta");
        (resp.status(), resp)
    }

    async fn body_text(resp: Response<Body>) -> String {
        let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("corpo");
        String::from_utf8_lossy(&bytes).to_string()
    }

    async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, String) {
        let (status, resp) = send(state, req).await;
        (status, body_text(resp).await)
    }

    fn json_of(text: &str) -> Value {
        serde_json::from_str(text).expect("json")
    }

    fn put_json(uri: &str, body: &Value) -> Request<Body> {
        request(Method::PUT, uri, &body.to_string(), Some("application/json"))
    }

    /// Servidor HTTP mínimo que responde uma vez com `(status, corpo)`.
    async fn spawn_chat_server(status: u16, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        // Responde sempre o mesmo a cada conexão: o cliente repete respostas ruins (spec 088).
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let n = match sock.read(&mut tmp).await {
                        Ok(n) if n > 0 => n,
                        _ => break,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(pos) = text.find("\r\n\r\n") {
                        let len: usize = text[..pos]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        if buf.len() >= pos + 4 + len {
                            break;
                        }
                    }
                }
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn invalid_host_is_forbidden() {
        let state = state_in(&temp_dir("host"), Config::default());
        let mut req = get_req("/api/config");
        req.headers_mut()
            .insert(header::HOST, "evil.example:3000".parse().expect("hv"));
        let (status, body) = call(&state, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(json_of(&body)["error"].is_string(), "{body}");
    }

    #[tokio::test]
    async fn missing_host_is_forbidden() {
        let state = state_in(&temp_dir("nohost"), Config::default());
        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .expect("req");
        let (status, _) = call(&state, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn cross_origin_is_forbidden_and_same_origin_is_allowed() {
        let state = state_in(&temp_dir("origin"), Config::default());
        let mut cross = get_req("/api/config");
        cross
            .headers_mut()
            .insert(header::ORIGIN, "http://evil.example".parse().expect("hv"));
        let (status, _) = call(&state, cross).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let mut same = get_req("/api/config");
        same.headers_mut()
            .insert(header::ORIGIN, "http://127.0.0.1:3000".parse().expect("hv"));
        let (status, _) = call(&state, same).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn mutation_without_json_content_type_is_415() {
        let dir = temp_dir("ct");
        let state = state_in(&dir, Config::default());
        let req = request(
            Method::PUT,
            "/api/config/agents",
            "{}",
            Some("text/plain"),
        );
        let (status, body) = call(&state, req).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(json_of(&body)["error"].is_string(), "{body}");

        let no_ct = request(Method::PUT, "/api/config/agents", "{}", None);
        let (status, _) = call(&state, no_ct).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(!dir.join("config.toml").exists());
    }

    #[tokio::test]
    async fn oversized_body_is_413() {
        let dir = temp_dir("big");
        let state = state_in(&dir, Config::default());
        let big = format!(
            r#"{{"id":"x","name":"x","provider_type":"openai","base_url":"https://a.b","api_key":"{}"}}"#,
            "a".repeat(MAX_BODY_BYTES + 1)
        );
        let req = request(
            Method::PUT,
            "/api/config/connections",
            &big,
            Some("application/json"),
        );
        let (status, body) = call(&state, req).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(json_of(&body)["error"].is_string(), "{body}");
        assert!(!body.contains("aaaa"));
    }

    #[tokio::test]
    async fn responses_carry_security_headers() {
        let state = state_in(&temp_dir("headers"), Config::default());
        let (_, resp) = send(&state, get_req("/api/config")).await;
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(resp.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    }

    #[tokio::test]
    async fn get_config_masks_keys_and_never_leaks_literal() {
        let state = state_in(&temp_dir("mask"), sample_config());
        let (status, body) = call(&state, get_req("/api/config")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains(SECRET), "chave vazou: {body}");
        let v = json_of(&body);
        assert_eq!(v["connections"][0]["api_key_masked"], "********1234");
        assert_eq!(
            v["connections"][1]["api_key_masked"],
            "env:NANTS_SERVER_TEST_KEY"
        );
        assert_eq!(v["server"]["port"], 3000);
        assert_eq!(v["agents"]["architect"]["connection_id"], "literal");
        assert!(v["projects"].is_array());
    }

    #[test]
    fn mask_handles_short_and_unicode_keys() {
        assert_eq!(config_api::mask_api_key("abc"), "********");
        assert_eq!(config_api::mask_api_key("12345678"), "********");
        assert_eq!(config_api::mask_api_key("123456789"), "********6789");
        assert_eq!(config_api::mask_api_key("çççççççççç"), "********çççç");
        assert_eq!(config_api::mask_api_key("env:FOO"), "env:FOO");
    }

    #[tokio::test]
    async fn put_connection_persists_with_mode_0600_and_roundtrips() {
        let dir = temp_dir("put");
        let state = state_in(&dir, Config::default());
        let body = json!({
            "id": "openai-direct",
            "name": "OpenAI",
            "provider_type": "openai",
            "base_url": "https://api.openai.com/v1/",
            "api_key": SECRET,
        });
        let (status, resp) = call(&state, put_json("/api/config/connections", &body)).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert!(!resp.contains(SECRET), "resposta vazou chave: {resp}");
        assert_eq!(json_of(&resp)["api_key_masked"], "********1234");

        let path = dir.join("config.toml");
        let saved = Config::load_from(&path).expect("load");
        assert_eq!(saved.connections.len(), 1);
        assert_eq!(saved.connections[0].api_key, SECRET);
        assert_eq!(saved.connections[0].base_url, "https://api.openai.com/v1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let (_, listing) = call(&state, get_req("/api/config")).await;
        assert!(!listing.contains(SECRET));
        assert_eq!(json_of(&listing)["connections"][0]["id"], "openai-direct");
        assert_eq!(
            json_of(&listing)["connections"][0]["api_key_masked"],
            "********1234"
        );
    }

    #[tokio::test]
    async fn update_without_api_key_keeps_existing_key() {
        let dir = temp_dir("keep");
        let state = state_in(&dir, sample_config());
        // Mesmo base_url: só o nome muda, a chave salva é mantida.
        let body = json!({
            "id": "literal",
            "name": "Novo nome",
            "provider_type": "openai",
            "base_url": "https://api.example.com/v1",
            "api_key": "",
        });
        let (status, _) = call(&state, put_json("/api/config/connections", &body)).await;
        assert_eq!(status, StatusCode::OK);
        let cfg = state.config().read().await;
        let conn = cfg.connections.iter().find(|c| c.id == "literal").expect("c");
        assert_eq!(conn.api_key, SECRET);
        assert_eq!(conn.name, "Novo nome");
        assert_eq!(cfg.connections.len(), 2, "upsert não duplica");
        drop(cfg);
        let saved = Config::load_from(&dir.join("config.toml")).expect("load");
        assert_eq!(saved.connections[0].api_key, SECRET);
    }

    #[tokio::test]
    async fn changing_base_url_without_api_key_is_422_and_config_intact() {
        let dir = temp_dir("endpoint");
        let state = state_in(&dir, sample_config());
        let body = json!({
            "id": "literal",
            "name": "Trocado",
            "provider_type": "openai",
            "base_url": "https://new.example.com/v1",
            "api_key": "",
        });
        let (status, resp) = call(&state, put_json("/api/config/connections", &body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
        assert_eq!(json_of(&resp)["error"], "api_key é obrigatória quando base_url muda");
        assert!(!resp.contains(SECRET), "resposta vazou chave: {resp}");
        let cfg = state.config().read().await;
        let conn = cfg.connections.iter().find(|c| c.id == "literal").expect("c");
        assert_eq!(conn.base_url, "https://api.example.com/v1");
        assert_eq!(conn.api_key, SECRET);
        assert_eq!(conn.name, "Conexão literal");
        drop(cfg);
        assert!(!dir.join("config.toml").exists(), "nada gravado em disco");
    }

    #[tokio::test]
    async fn new_connection_without_api_key_is_422() {
        let dir = temp_dir("nokey");
        let state = state_in(&dir, Config::default());
        let body = json!({
            "id": "novo",
            "name": "Novo",
            "provider_type": "openai",
            "base_url": "https://a.example.com",
            "api_key": "",
        });
        let (status, _) = call(&state, put_json("/api/config/connections", &body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!dir.join("config.toml").exists());
    }

    #[tokio::test]
    async fn invalid_connection_fields_are_422() {
        let state = state_in(&temp_dir("invalid-conn"), Config::default());
        let cases = [
            json!({"id":"","name":"n","provider_type":"openai","base_url":"https://a.b","api_key":"k"}),
            json!({"id":"com espaco","name":"n","provider_type":"openai","base_url":"https://a.b","api_key":"k"}),
            json!({"id":"ok","name":"n","provider_type":"openai","base_url":"ftp://a.b","api_key":"k"}),
            json!({"id":"ok","name":"n","provider_type":"openai","base_url":"nao é url","api_key":"k"}),
            json!({"id":"ok","name":"n","provider_type":"openai","base_url":"https://a.b","api_key":"env:"}),
        ];
        for body in cases {
            let (status, resp) = call(&state, put_json("/api/config/connections", &body)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body} -> {resp}");
        }
        let malformed = request(
            Method::PUT,
            "/api/config/connections",
            "{nao json",
            Some("application/json"),
        );
        let (status, _) = call(&state, malformed).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn agents_with_unknown_connection_are_422() {
        let dir = temp_dir("agents-422");
        let state = state_in(&dir, sample_config());
        let body = json!({
            "architect": {"connection_id": "nao-existe", "model": "m", "effort": "low"},
            "developer": {"connection_id": "literal", "model": "m", "effort": "low"},
            "reviewer": {"connection_id": "literal", "model": "m", "effort": "medium"},
        });
        let (status, resp) = call(&state, put_json("/api/config/agents", &body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
        assert_eq!(state.config().read().await.agents.architect.connection_id, "literal");
        assert!(!dir.join("config.toml").exists());
    }

    #[tokio::test]
    async fn agents_with_invalid_effort_or_empty_model_are_422() {
        let state = state_in(&temp_dir("agents-bad"), sample_config());
        let bad_effort = json!({
            "architect": {"connection_id": "literal", "model": "m", "effort": "extreme"},
            "developer": {"connection_id": "literal", "model": "m", "effort": "low"},
            "reviewer": {"connection_id": "literal", "model": "m", "effort": "low"},
        });
        let (status, _) = call(&state, put_json("/api/config/agents", &bad_effort)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let empty_model = json!({
            "architect": {"connection_id": "literal", "model": "  ", "effort": "low"},
            "developer": {"connection_id": "literal", "model": "m", "effort": "low"},
            "reviewer": {"connection_id": "literal", "model": "m", "effort": "low"},
        });
        let (status, _) = call(&state, put_json("/api/config/agents", &empty_model)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn valid_agents_are_persisted_and_returned() {
        let dir = temp_dir("agents-ok");
        let state = state_in(&dir, sample_config());
        let body = json!({
            "architect": {"connection_id": "literal", "model": "gpt-4o", "effort": "high"},
            "developer": {"connection_id": "envkey", "model": "o3-mini", "effort": "low"},
            "reviewer": {"connection_id": "envkey", "model": "claude", "effort": "medium"},
        });
        let (status, resp) = call(&state, put_json("/api/config/agents", &body)).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        let saved = Config::load_from(&dir.join("config.toml")).expect("load");
        assert_eq!(saved.agents.architect.effort, Effort::High);
        assert_eq!(saved.agents.developer.connection_id, "envkey");
        assert_eq!(saved.agents.reviewer.model, "claude");
    }

    #[tokio::test]
    async fn save_leaves_no_temporary_file() {
        let dir = temp_dir("atomic");
        let state = state_in(&dir, Config::default());
        let body = json!({
            "id": "a1", "name": "A", "provider_type": "local",
            "base_url": "http://localhost:11434/v1", "api_key": SECRET,
        });
        let (status, _) = call(&state, put_json("/api/config/connections", &body)).await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<String> = fs::read_dir(&dir)
            .expect("readdir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["config.toml".to_string()], "{names:?}");
    }

    #[tokio::test]
    async fn failed_save_keeps_memory_and_removes_temporary() {
        let dir = temp_dir("atomic-fail");
        // Destino ocupado por diretório: o rename final falha.
        let target = dir.join("config.toml");
        fs::create_dir_all(&target).expect("mkdir destino");
        let state = AppState::new(target.clone(), Config::default(), PORT);
        let body = json!({
            "id": "a1", "name": "A", "provider_type": "local",
            "base_url": "http://localhost:11434/v1", "api_key": SECRET,
        });
        let (status, resp) = call(&state, put_json("/api/config/connections", &body)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{resp}");
        assert!(!resp.contains(SECRET));
        assert!(state.config().read().await.connections.is_empty());
        assert!(!dir.join(".config.toml.tmp").exists());
    }

    #[tokio::test]
    async fn connection_test_succeeds_against_local_server() {
        let base = spawn_chat_server(200, OK_CHAT).await;
        let state = state_in(&temp_dir("test-ok"), Config::default());
        let body = json!({
            "provider_type": "openai",
            "base_url": base,
            "api_key": SECRET,
            "model": "modelo-teste",
        });
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            &body.to_string(),
            Some("application/json"),
        );
        let (status, resp) = call(&state, req).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert!(!resp.contains(SECRET), "vazou chave: {resp}");
        let v = json_of(&resp);
        assert_eq!(v["ok"], true);
        assert!(v["latency_ms"].is_u64());
        assert!(v.get("error").is_none());
    }

    #[tokio::test]
    async fn connection_test_failure_is_reported_without_key() {
        let base = spawn_chat_server(401, r#"{"error":"invalid key"}"#).await;
        let state = state_in(&temp_dir("test-fail"), Config::default());
        let body = json!({
            "provider_type": "openai",
            "base_url": base,
            "api_key": SECRET,
            "model": "m",
        });
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            &body.to_string(),
            Some("application/json"),
        );
        let (status, resp) = call(&state, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!resp.contains(SECRET), "vazou chave: {resp}");
        let v = json_of(&resp);
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "http_error", "{resp}");
        assert_eq!(v["status"], 401, "{resp}");
        assert!(v.get("latency_ms").is_none());
    }

    #[tokio::test]
    async fn connection_test_remote_500_body_is_never_echoed() {
        let body = "vazou sk-SEGREDO-LITERAL-1234 e https://u:SEG@h/v1 aqui";
        let base = spawn_chat_server(500, body).await;
        let state = state_in(&temp_dir("test-500"), Config::default());
        let req_body = json!({"provider_type": "openai", "base_url": base, "api_key": SECRET, "model": "m"});
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            &req_body.to_string(),
            Some("application/json"),
        );
        let (status, resp) = call(&state, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!resp.contains("SEG"), "{resp}");
        assert!(!resp.contains(SECRET), "{resp}");
        assert!(!resp.contains("aqui"), "corpo remoto ecoado: {resp}");
        let v = json_of(&resp);
        assert_eq!(v["error"], "http_error");
        assert_eq!(v["status"], 500);
    }

    #[tokio::test]
    async fn connection_test_userinfo_in_query_never_echoed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let state = state_in(&temp_dir("test-query"), Config::default());
        for tail in ["?ref=https://u:pa(ss)@h", "?ref=https://u:pa'ss?x#y@h"] {
            let base = format!("http://127.0.0.1:{port}/v1{tail}");
            let req_body = json!({"provider_type": "openai", "base_url": base, "api_key": SECRET, "model": "m"});
            let req = request(
                Method::POST,
                "/api/config/connections/test",
                &req_body.to_string(),
                Some("application/json"),
            );
            let (status, resp) = call(&state, req).await;
            // Query é rejeitada na validação (422), antes de qualquer requisição.
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
            for leak in ["pa(ss)", "pa'ss", "u:", "https://u", "127.0.0.1", SECRET] {
                assert!(!resp.contains(leak), "vazou {leak:?}: {resp}");
            }
        }
    }

    #[tokio::test]
    async fn connection_test_non_json_success_is_bad_response() {
        let base = spawn_chat_server(200, "<html>nao sou json</html>").await;
        let state = state_in(&temp_dir("test-html"), Config::default());
        let req_body = json!({"provider_type": "openai", "base_url": base, "api_key": SECRET, "model": "m"});
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            &req_body.to_string(),
            Some("application/json"),
        );
        let (_, resp) = call(&state, req).await;
        assert_eq!(json_of(&resp)["error"], "bad_response", "{resp}");
        assert!(!resp.contains("html"), "{resp}");
    }

    #[tokio::test]
    async fn connection_test_timeout_is_reported_as_timeout() {
        // Aceita a conexão e não responde nunca.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            if let Ok((_sock, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        let conn = Connection {
            id: String::new(),
            name: String::new(),
            base_url: base,
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        };
        let result = config_api::run_handshake_with(&conn, "m".into(), Duration::from_millis(200)).await;
        assert!(!result.ok);
        assert_eq!(result.error, Some("timeout"));
        assert!(!serde_json::to_string(&result).expect("json").contains(SECRET));
    }

    #[tokio::test]
    async fn connection_test_stays_out_of_the_pace_limiter() {
        // Spec 089 T02: o 429 do teste de conexão não ensina o ritmo do par; o mesmo 429 num
        // cliente de execução, sim.
        use crate::agents::llm_client::{tests::spawn_server, Pacer};
        let (base, _) = spawn_server(vec![(429, "{}"), (429, "{}")]).await;
        let conn = Connection {
            id: "c-test-pace".into(),
            name: String::new(),
            base_url: base,
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        };
        let result = config_api::run_handshake_with(&conn, "m".into(), Duration::from_secs(5)).await;
        assert_eq!(result.status, Some(429));
        assert!(!Pacer::global().knows("c-test-pace", "m"));
        let req = crate::agents::llm_client::ChatRequest {
            model: "m".into(),
            messages: vec![crate::agents::llm_client::ChatMessage::text(crate::agents::llm_client::Role::User, "oi")],
            tools: Vec::new(),
            effort: None,
            response_json: false,
            web_search: false,
            cache_key: None,
            web_search_max_uses: None,
        };
        let client = crate::agents::llm_client::LlmClient::from_connection(&conn)
            .expect("cliente")
            .with_throttle_delays(Vec::new());
        assert!(client.chat(&req).await.is_err());
        assert!(Pacer::global().knows("c-test-pace", "m"));
    }

    #[tokio::test]
    async fn connection_test_unreachable_host_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let state = state_in(&temp_dir("test-down"), Config::default());
        let body = json!({
            "provider_type": "local",
            "base_url": format!("http://127.0.0.1:{port}/v1"),
            "api_key": SECRET,
            "model": "m",
        });
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            &body.to_string(),
            Some("application/json"),
        );
        let (status, resp) = call(&state, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!resp.contains(SECRET));
        assert_eq!(json_of(&resp)["ok"], false);
        assert_eq!(json_of(&resp)["error"], "connect_failed");
    }

    #[tokio::test]
    async fn connection_test_by_saved_id_uses_saved_key() {
        let base = spawn_chat_server(200, OK_CHAT).await;
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "salva".into(),
            name: "Salva".into(),
            base_url: base,
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&temp_dir("test-id"), cfg);
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            r#"{"id":"salva","model":"m"}"#,
            Some("application/json"),
        );
        let (status, resp) = call(&state, req).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert_eq!(json_of(&resp)["ok"], true);
        assert!(!resp.contains(SECRET));
    }

    #[tokio::test]
    async fn connection_test_rejects_non_http_scheme_and_missing_target() {
        let state = state_in(&temp_dir("test-422"), Config::default());
        let bad_scheme = json!({
            "provider_type": "openai", "base_url": "file:///etc/passwd",
            "api_key": SECRET, "model": "m",
        });
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            &bad_scheme.to_string(),
            Some("application/json"),
        );
        let (status, _) = call(&state, req).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let unknown_id = request(
            Method::POST,
            "/api/config/connections/test",
            r#"{"id":"nao-existe"}"#,
            Some("application/json"),
        );
        let (status, _) = call(&state, unknown_id).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn base_url_with_userinfo_is_422_and_never_listed() {
        let dir = temp_dir("userinfo");
        let state = state_in(&dir, Config::default());
        let body = json!({
            "id": "u1", "name": "U", "provider_type": "openai",
            "base_url": "https://user:SEGREDO@host.example/v1", "api_key": "sk-outra-chave-9999",
        });
        let (status, resp) = call(&state, put_json("/api/config/connections", &body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
        assert!(!dir.join("config.toml").exists());

        // Configuração já existente (ex.: editada à mão) não vaza userinfo no GET.
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "legado".into(),
            name: "Legado".into(),
            base_url: "https://user:SEGREDO@host.example/v1".into(),
            api_key: "sk-outra-chave-9999".into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&temp_dir("userinfo-get"), cfg);
        let (_, listing) = call(&state, get_req("/api/config")).await;
        assert!(!listing.contains("SEGREDO"), "vazou userinfo: {listing}");
        assert_eq!(json_of(&listing)["connections"][0]["base_url"], "https://host.example/v1");
    }

    #[tokio::test]
    async fn legacy_saved_base_url_with_userinfo_is_rejected_on_test() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "legado".into(),
            name: "Legado".into(),
            base_url: format!("https://user:SEGREDO@127.0.0.1:{port}/v1"),
            api_key: "sk-outra-chave-9999".into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&temp_dir("legacy-test"), cfg);
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            r#"{"id":"legado","model":"m"}"#,
            Some("application/json"),
        );
        let (status, resp) = call(&state, req).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
        assert!(!resp.contains("SEGREDO"), "vazou userinfo: {resp}");
    }

    /// Lê uma requisição HTTP completa (cabeça e corpo pelo Content-Length).
    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = match sock.read(&mut tmp).await {
                Ok(n) if n > 0 => n,
                _ => break,
            };
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(pos) = text.find("\r\n\r\n") {
                let len: usize = text[..pos]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                if buf.len() >= pos + 4 + len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Servidor local: responde, em ordem, `(status, corpo)` e guarda cada requisição.
    async fn spawn_recording_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let captured = Arc::new(Mutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        tokio::spawn(async move {
            for (status, body) in responses {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let raw = read_request(&mut sock).await;
                cap.lock().expect("lock").push(raw);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/v1"), captured)
    }

    const MODELS_BODY: &str = r#"{"object":"list","data":[{"id":"a"},{"id":"b"},{"id":"c"}]}"#;

    fn test_post(body: &Value) -> Request<Body> {
        request(
            Method::POST,
            "/api/config/connections/test",
            &body.to_string(),
            Some("application/json"),
        )
    }

    #[tokio::test]
    async fn teste_sem_model_consulta_a_listagem_e_conta_modelos() {
        let (base, cap) = spawn_recording_server(vec![(200, MODELS_BODY)]).await;
        let state = state_in(&temp_dir("probe-ok"), Config::default());
        let (status, resp) = call(
            &state,
            test_post(&json!({"base_url": base, "api_key": SECRET})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert!(!resp.contains(SECRET), "vazou chave: {resp}");
        let v = json_of(&resp);
        assert_eq!(v["ok"], true, "{resp}");
        assert_eq!(v["models_count"], 3, "{resp}");
        assert!(v["latency_ms"].is_u64());
        assert!(v.get("error").is_none());

        let head = cap.lock().expect("lock")[0].to_ascii_lowercase();
        assert!(head.starts_with("get /v1/models http/1.1"), "{head}");
        assert!(head.contains(&format!("authorization: bearer {}", SECRET.to_ascii_lowercase())));
    }

    #[tokio::test]
    async fn teste_com_model_continua_usando_chat_completion() {
        let (base, cap) = spawn_recording_server(vec![(200, OK_CHAT)]).await;
        let state = state_in(&temp_dir("probe-chat"), Config::default());
        let (status, resp) = call(
            &state,
            test_post(&json!({"base_url": base, "api_key": SECRET, "model": "m"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        let v = json_of(&resp);
        assert_eq!(v["ok"], true, "{resp}");
        assert!(v.get("models_count").is_none(), "{resp}");
        let head = cap.lock().expect("lock")[0].to_ascii_lowercase();
        assert!(head.starts_with("post /v1/chat/completions http/1.1"), "{head}");
    }

    #[tokio::test]
    async fn modelo_do_arquiteto_nao_e_mais_usado_implicitamente() {
        let (base, cap) = spawn_recording_server(vec![(200, MODELS_BODY)]).await;
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "salva".into(),
            name: "Salva".into(),
            base_url: base,
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        cfg.agents.architect = AgentConfig {
            connection_id: "salva".into(),
            model: "modelo-do-arquiteto".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        let state = state_in(&temp_dir("probe-arch"), cfg);
        let (status, resp) = call(&state, test_post(&json!({"id": "salva"}))).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert_eq!(json_of(&resp)["models_count"], 3, "{resp}");
        let reqs = cap.lock().expect("lock").clone();
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].to_ascii_lowercase().starts_with("get /v1/models "), "{}", reqs[0]);
        assert!(!reqs[0].contains("modelo-do-arquiteto"));
    }

    #[tokio::test]
    async fn teste_sem_model_nao_grava_no_cache_de_modelos() {
        let (base, _cap) = spawn_recording_server(vec![(200, MODELS_BODY)]).await;
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "salva".into(),
            name: "Salva".into(),
            base_url: base.clone(),
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&temp_dir("probe-cache"), cfg);
        let key = super::models_api::CacheKey::new("salva", &base);
        assert!(state.models_cache().get(&key, std::time::Instant::now()).is_none());
        let (status, resp) = call(&state, test_post(&json!({"id": "salva"}))).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert_eq!(json_of(&resp)["ok"], true);
        assert!(
            state.models_cache().get(&key, std::time::Instant::now()).is_none(),
            "o teste não deve popular o cache de modelos"
        );
    }

    #[tokio::test]
    async fn teste_sem_model_404_405_501_viram_unsupported() {
        for status_code in [404u16, 405, 501] {
            let (base, _cap) = spawn_recording_server(vec![(status_code, "{}")]).await;
            let state = state_in(&temp_dir("probe-unsup"), Config::default());
            let (status, resp) = call(
                &state,
                test_post(&json!({"base_url": base, "api_key": SECRET})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{resp}");
            let v = json_of(&resp);
            assert_eq!(v["ok"], false, "{resp}");
            assert_eq!(v["error"], "unsupported", "{resp}");
            assert!(v.get("status").is_none(), "{resp}");
        }
    }

    #[tokio::test]
    async fn teste_sem_model_401_vira_http_error_com_status() {
        let body = r#"{"error":"chave recusada SECRETBODY"}"#;
        let (base, _cap) = spawn_recording_server(vec![(401, body)]).await;
        let state = state_in(&temp_dir("probe-401"), Config::default());
        let (status, resp) = call(
            &state,
            test_post(&json!({"base_url": base, "api_key": SECRET})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert!(!resp.contains(SECRET) && !resp.contains("SECRETBODY"), "{resp}");
        let v = json_of(&resp);
        assert_eq!(v["error"], "http_error");
        assert_eq!(v["status"], 401);
        assert!(v.get("models_count").is_none());
    }

    #[tokio::test]
    async fn teste_sem_model_corpo_sem_lista_vira_bad_response() {
        let (base, _cap) = spawn_recording_server(vec![(200, r#"{"data":["a"]}"#)]).await;
        let state = state_in(&temp_dir("probe-bad"), Config::default());
        let (status, resp) = call(
            &state,
            test_post(&json!({"base_url": base, "api_key": SECRET})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert_eq!(json_of(&resp)["error"], "bad_response", "{resp}");
        assert!(!resp.contains("\"a\""), "{resp}");
    }

    #[tokio::test]
    async fn teste_sem_model_sem_chave_salva_para_outro_endpoint_da_422() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let other = format!("http://{}/v1", listener.local_addr().expect("addr"));
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "salva".into(),
            name: "Salva".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&temp_dir("probe-f03"), cfg);
        let (status, resp) = call(
            &state,
            test_post(&json!({"id": "salva", "base_url": other})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
        assert!(!resp.contains(SECRET));
        let accepted =
            tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(accepted.is_err(), "novo host recebeu conexão com a chave salva");
    }

    #[tokio::test]
    async fn teste_sem_model_com_timeout_vira_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            if let Ok((_sock, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        let conn = Connection {
            id: String::new(),
            name: String::new(),
            base_url: base,
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        };
        let result =
            config_api::run_models_probe_with(&conn, Duration::from_millis(200)).await;
        assert!(!result.ok);
        assert_eq!(result.error, Some("timeout"));
        assert!(!serde_json::to_string(&result).expect("json").contains(SECRET));
    }

    async fn status_with_host(policy: security::HostPolicy, host: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let state = AppState::new(PathBuf::from("config.toml"), Config::default(), port)
            .with_host_policy(policy);
        let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_with_shutdown(listener, state, async move {
            let _ = stop_rx.await;
        }));
        let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("conectar");
        let req = format!("GET /api/config HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
        conn.write_all(req.as_bytes()).await.expect("escrever");
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("ler");
        let _ = stop.send(());
        let _ = server.await;
        String::from_utf8_lossy(&buf).lines().next().unwrap_or("").to_string()
    }

    #[tokio::test]
    async fn lan_policy_accepts_ip_host_and_default_rejects() {
        let lan = status_with_host(security::HostPolicy::Lan, "192.168.1.50").await;
        assert!(lan.contains(" 200"), "{lan}");
        let def = status_with_host(security::HostPolicy::Loopback, "192.168.1.50").await;
        assert!(def.contains("403"), "{def}");
    }

    #[tokio::test]
    async fn serve_completes_with_open_sse_connection_after_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let state = AppState::new(PathBuf::from("config.toml"), Config::default(), port);
        let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_with_shutdown(listener, state, async move {
            let _ = stop_rx.await;
        }));

        let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("conectar");
        let req = format!(
            "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
        );
        conn.write_all(req.as_bytes()).await.expect("escrever");
        let mut head = Vec::new();
        let mut chunk = [0u8; 512];
        while !String::from_utf8_lossy(&head).contains("text/event-stream") {
            let n = tokio::time::timeout(Duration::from_secs(2), conn.read(&mut chunk))
                .await
                .expect("timeout lendo cabeçalhos")
                .expect("leitura");
            assert!(n > 0, "conexão fechada antes do SSE");
            head.extend_from_slice(&chunk[..n]);
        }

        stop.send(()).expect("sinal de parada");
        let joined = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("servidor não encerrou em 2 s com SSE aberto");
        joined.expect("task").expect("serve");
    }

    #[tokio::test]
    async fn base_url_with_query_or_fragment_is_422_and_never_echoed() {
        let dir = temp_dir("query");
        let state = state_in(&dir, Config::default());
        for url in ["https://h/v1?key=SEGREDO", "https://h/v1#SEGREDO", "https://h/v1?"] {
            let body = json!({
                "id": "q1", "name": "Q", "provider_type": "openai",
                "base_url": url, "api_key": "sk-outra-chave-9999",
            });
            let (status, resp) = call(&state, put_json("/api/config/connections", &body)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{url} -> {resp}");
            assert!(!resp.contains("SEGREDO"), "{resp}");
        }
        assert!(!dir.join("config.toml").exists());
    }

    #[tokio::test]
    async fn hand_edited_query_in_saved_base_url_is_not_listed() {
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "legado".into(),
            name: "Legado".into(),
            base_url: "https://h/v1?key=SEGREDO".into(),
            api_key: "sk-outra-chave-9999".into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&temp_dir("query-get"), cfg);
        let (_, listing) = call(&state, get_req("/api/config")).await;
        assert!(!listing.contains("SEGREDO"), "vazou query: {listing}");
        assert_eq!(json_of(&listing)["connections"][0]["base_url"], "https://h/v1");
    }

    #[tokio::test]
    async fn control_chars_and_long_fields_are_422() {
        let state = state_in(&temp_dir("fields"), Config::default());
        let bad_name = json!({
            "id": "n1", "name": "a\u{7}b", "provider_type": "openai",
            "base_url": "https://a.example", "api_key": "sk-outra-chave-9999",
        });
        let (status, _) = call(&state, put_json("/api/config/connections", &bad_name)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let long_name = json!({
            "id": "n2", "name": "n".repeat(129), "provider_type": "openai",
            "base_url": "https://a.example", "api_key": "sk-outra-chave-9999",
        });
        let (status, _) = call(&state, put_json("/api/config/connections", &long_name)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let long_model = json!({
            "architect": {"connection_id": "literal", "model": "m".repeat(129), "effort": "low"},
            "developer": {"connection_id": "literal", "model": "m", "effort": "low"},
            "reviewer": {"connection_id": "literal", "model": "m", "effort": "low"},
        });
        let state = state_in(&temp_dir("fields-agents"), sample_config());
        let (status, _) = call(&state, put_json("/api/config/agents", &long_model)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn put_com_campos_legados_é_aceito_e_ignorado() {
        let dir = temp_dir("legado-put");
        let state = state_in(&dir, Config::default());
        let legacy = json!({
            "id": "av", "name": "A", "provider_type": "azure",
            "base_url": "https://a.example", "api_key": "sk-outra-chave-9999",
            "api_version": "sk-SEGREDO bad",
        });
        let (status, resp) = call(&state, put_json("/api/config/connections", &legacy)).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        assert!(json_of(&resp).get("provider_type").is_none(), "{resp}");
        assert!(json_of(&resp).get("api_version").is_none(), "{resp}");
        assert!(!resp.contains("SEGREDO"), "{resp}");
        let saved = fs::read_to_string(dir.join("config.toml")).expect("config");
        assert!(!saved.contains("provider_type") && !saved.contains("api_version"), "{saved}");
    }

    #[tokio::test]
    async fn read_view_sanitizes_hand_edited_fields() {
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "bad id\u{7}".into(),
            name: "n\u{7}".into(),
            base_url: "https://h/v1".into(),
            api_key: "sk-outra-chave-9999".into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        cfg.agents.architect = AgentConfig {
            connection_id: "bad id".into(),
            model: "gpt\u{7}".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        cfg.projects.push(crate::config::Project {
            id: "p1".into(),
            name: "pn\u{7}".into(),
            path: "/x/\u{7}".into(),
            created_at: "2026-10-13T21:00:00Z".into(),
        });
        let state = state_in(&temp_dir("view-sanitize"), cfg);
        let (_, listing) = call(&state, get_req("/api/config")).await;
        assert!(!listing.contains("gpt"), "{listing}");
        let v = json_of(&listing);
        assert_eq!(v["connections"][0]["id"], "[inválido]");
        assert_eq!(v["connections"][0]["name"], "[inválido]");
        assert_eq!(v["agents"]["architect"]["connection_id"], "[inválido]");
        assert_eq!(v["agents"]["architect"]["model"], "[inválido]");
        assert_eq!(v["projects"][0]["name"], "[inválido]");
        assert_eq!(v["projects"][0]["path"], "[inválido]");
        assert_eq!(v["projects"][0]["id"], "p1");
    }

    #[tokio::test]
    async fn default_config_read_view_keeps_empty_agent_fields() {
        // Primeira execução: agente não configurado é "" (não "[inválido]").
        let state = state_in(&temp_dir("first-run-view"), Config::default());
        let (status, listing) = call(&state, get_req("/api/config")).await;
        assert_eq!(status, StatusCode::OK, "{listing}");
        let v = json_of(&listing);
        for role in ["architect", "developer", "reviewer"] {
            assert_eq!(v["agents"][role]["connection_id"], "", "{role}: {listing}");
            assert_eq!(v["agents"][role]["model"], "", "{role}: {listing}");
        }
    }

    #[tokio::test]
    async fn read_view_marks_invalid_non_empty_agent_connection_id() {
        let mut cfg = Config::default();
        cfg.agents.developer.connection_id = "a b".into();
        let state = state_in(&temp_dir("first-run-invalid"), cfg);
        let (_, listing) = call(&state, get_req("/api/config")).await;
        let v = json_of(&listing);
        assert_eq!(v["agents"]["developer"]["connection_id"], "[inválido]", "{listing}");
        assert_eq!(v["agents"]["architect"]["connection_id"], "", "{listing}");
    }

    #[tokio::test]
    async fn put_agents_still_requires_existing_connection_id() {
        let state = state_in(&temp_dir("first-run-put"), Config::default());
        let body = json!({
            "architect": {"connection_id": "", "model": "m", "effort": "low"},
            "developer": {"connection_id": "", "model": "m", "effort": "low"},
            "reviewer": {"connection_id": "", "model": "m", "effort": "low"},
        });
        let (status, resp) = call(&state, put_json("/api/config/agents", &body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    }

    #[tokio::test]
    async fn valid_key_shaped_model_passes_risk_accepted_f10() {
        // Risco aceito (F-10): só controle e tamanho são validados; formato de chave não.
        let mut cfg = Config::default();
        cfg.agents.architect = AgentConfig {
            connection_id: String::new(),
            model: format!("sk-{}", "b".repeat(48)),
            effort: Effort::Low,
            max_steps: None,
        };
        let state = state_in(&temp_dir("f10"), cfg);
        let (_, listing) = call(&state, get_req("/api/config")).await;
        assert_eq!(
            json_of(&listing)["agents"]["architect"]["model"],
            format!("sk-{}", "b".repeat(48))
        );
    }

    #[tokio::test]
    async fn key_shaped_env_reference_is_rejected_on_put_and_masked_on_get() {
        let dir = temp_dir("env-f11");
        let state = state_in(&dir, Config::default());
        let body = json!({
            "id": "e1", "name": "E", "provider_type": "openai",
            "base_url": "https://a.example", "api_key": "env:sk-abc123segredo",
        });
        let (status, resp) = call(&state, put_json("/api/config/connections", &body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
        assert!(!resp.contains("segredo"), "{resp}");

        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "legado".into(),
            name: "Legado".into(),
            base_url: "https://h/v1".into(),
            api_key: "env:sk-abc123segredo".into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&temp_dir("env-f11-get"), cfg);
        let (_, listing) = call(&state, get_req("/api/config")).await;
        assert!(!listing.contains("segredo"), "{listing}");
    }

    #[tokio::test]
    async fn valid_env_reference_is_accepted_and_bad_names_are_422() {
        let state = state_in(&temp_dir("env-ok"), Config::default());
        let ok = json!({
            "id": "e2", "name": "E", "provider_type": "openai",
            "base_url": "https://a.example", "api_key": "env:OPENAI_API_KEY",
        });
        let (status, _) = call(&state, put_json("/api/config/connections", &ok)).await;
        assert_eq!(status, StatusCode::OK);
        for bad in [
            "env:".to_string(),
            "env:A B".to_string(),
            format!("env:{}", "A".repeat(65)),
        ] {
            let body = json!({
                "id": "e3", "name": "E", "provider_type": "openai",
                "base_url": "https://a.example", "api_key": bad,
            });
            let (status, _) = call(&state, put_json("/api/config/connections", &body)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        }
    }

    #[tokio::test]
    async fn saved_key_is_not_reused_for_different_base_url() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let other = format!("http://{}/v1", listener.local_addr().expect("addr"));
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "salva".into(),
            name: "Salva".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&temp_dir("f03"), cfg);
        let body = json!({
            "id": "salva", "provider_type": "openai", "base_url": other, "model": "m",
        });
        let req = request(
            Method::POST,
            "/api/config/connections/test",
            &body.to_string(),
            Some("application/json"),
        );
        let (status, resp) = call(&state, req).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
        assert!(!resp.contains(SECRET));
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(300), listener.accept()).await;
        assert!(accepted.is_err(), "novo host recebeu conexão com a chave salva");
    }

    #[tokio::test]
    async fn unknown_route_still_gets_json_error_and_security_headers() {
        let state = state_in(&temp_dir("404"), Config::default());
        let (status, resp) = send(&state, get_req("/api/inexistente")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
        let body = body_text(resp).await;
        assert!(json_of(&body)["error"].is_string(), "{body}");
    }
}

#[cfg(test)]
mod static_ui_routes_tests {
    use std::path::PathBuf;

    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    const HOST: &str = "127.0.0.1:3000";

    fn state() -> AppState {
        AppState::new(PathBuf::from("config.toml"), crate::config::Config::default(), 3000)
    }

    fn req(method: Method, uri: &str, host: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, host)
            .body(Body::empty())
            .expect("requisição")
    }

    async fn send(method: Method, uri: &str, host: &str) -> axum::response::Response {
        router(state()).oneshot(req(method, uri, host)).await.expect("resposta")
    }

    async fn text(resp: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("corpo");
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[tokio::test]
    async fn root_serves_spa_with_security_headers() {
        let resp = send(Method::GET, "/", HOST).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert!(resp.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .expect("csp")
            .contains("script-src 'self'"));
        assert_eq!(resp.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(resp.headers().get_all(header::X_CONTENT_TYPE_OPTIONS).iter().count(), 1);
        assert_eq!(resp.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(text(resp).await.contains("<div id=\"root\">"));
    }

    #[tokio::test]
    async fn space_url_returns_index_200() {
        let resp = send(Method::GET, "/?space=p1", HOST).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
    }

    #[tokio::test]
    async fn unknown_api_path_is_json_404() {
        let resp = send(Method::GET, "/api/nope", HOST).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(text(resp).await, r#"{"code":"not_found","error":"não encontrado"}"#);
    }

    #[tokio::test]
    async fn traversal_and_missing_files_are_404() {
        for uri in ["/../Cargo.toml", "/%2e%2e/etc", "/a//b", "/.git/config", "/missing.js"] {
            let resp = send(Method::GET, uri, HOST).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn post_to_root_is_405_json() {
        // Com Content-Type JSON o guard passa e o fallback responde 405.
        let mut post = req(Method::POST, "/", HOST);
        post.headers_mut()
            .insert(header::CONTENT_TYPE, "application/json".parse().expect("ct"));
        let resp = router(state()).oneshot(post).await.expect("resposta");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn head_root_is_200_without_body() {
        let resp = send(Method::HEAD, "/", HOST).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(text(resp).await, "");
    }

    #[tokio::test]
    async fn static_responses_are_guarded_by_host() {
        let resp = send(Method::GET, "/", "evil.com:3000").await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
        let resp = send(Method::GET, "/assets/x.js", "evil.com:3000").await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn api_config_keeps_no_store_and_json() {
        let resp = send(Method::GET, "/api/config", HOST).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
    }
}

#[cfg(test)]
mod e2e_tests;
