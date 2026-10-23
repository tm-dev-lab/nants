/*
 * Spec 115 T02: todo erro da API chega com `code` e a interface traduz só por ele. Uma amostra de
 * cada área do servidor: a resposta real (texto de reserva em pt-BR, `code` e `params`) sai no
 * idioma ativo, nos dois idiomas, com os parâmetros preenchidos e sem português no inglês.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { getConfig } from "../api/client";
import { en } from "./en";
import type { MessageKey } from "./index";
import { looksPortuguese } from "./ptLeftovers";
import { ptBR } from "./ptBR";
import { apiFailureText, systemMessageText } from "./serverCodes";

function stubFetch(status: number, body: unknown) {
  const response = new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
  vi.stubGlobal("fetch", vi.fn(async () => response));
}

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Área do servidor, status, `code`, `params`, o texto esperado em pt-BR e em en. */
const SAMPLES: readonly (readonly [string, number, string, Record<string, string | number>, string, string])[] = [
  ["security.rs", 403, "host_forbidden", {}, "Endereço de acesso não permitido.", "Access address not allowed."],
  ["devices.rs", 400, "device_code_invalid", { attempts_left: 2 }, "Tentativas que restam: 2.", "Attempts left: 2."],
  ["app.rs", 409, "app_already_running", {}, "o sistema já está em execução neste projeto", "the system is already running in this project"],
  ["runs.rs", 409, "no_blocked_task", {}, "não há tarefa bloqueada para retomar", "there is no blocked task to resume"],
  ["runs.rs (params)", 409, "budget_exceeded", { reason: "1100 de 1000 tokens" }, "(1100 de 1000 tokens)", "(1100 de 1000 tokens)"],
  ["chat.rs", 422, "message_too_long", { max: 32000 }, "message deve ter no máximo 32000 caracteres", "message must have at most 32000 characters"],
  ["chat_attachments.rs", 413, "image_too_large", {}, "imagem maior que 5 MiB", "image larger than 5 MiB"],
  ["config_api.rs", 422, "model_required", { role: "developer" }, "developer: model é obrigatório", "developer: model is required"],
  ["config_api.rs (faixa)", 422, "max_steps_range", { role: "reviewer", min: 10, max: 1000 }, "entre 10 e 1000", "between 10 and 1000"],
  ["projects.rs", 422, "path_required", {}, "path é obrigatório", "path is required"],
  ["projects.rs (repositório)", 409, "project_nested_repo", { path: "/home/x" }, "repositório Git /home/x", "Git repository /home/x"],
  ["projects_root.rs", 422, "projects_root_system", {}, "diretório de sistema não é permitido", "system directories are not allowed"],
  ["fs_browse.rs", 403, "list_no_permission", {}, "sem permissão para listar o diretório", "no permission to list the folder"],
  ["file_explorer.rs", 400, "source_invalid", {}, "fonte inválida", "invalid source"],
  ["ui_state.rs", 422, "ui_section_invalid", {}, "section deve ter de 1 a 64 caracteres", "section must have 1 to 64 characters"],
  ["usage.rs", 422, "budget_invalid", {}, "orçamento inválido", "invalid budget"],
  ["approvals.rs", 422, "decision_invalid", {}, 'decision deve ser "once"', 'decision must be "once"'],
  ["dialog.rs", 422, "limit_invalid", {}, "limit deve ser um inteiro entre 1 e 500", "limit must be an integer between 1 and 500"],
  ["spec_result.rs", 504, "result_timeout", {}, "tempo limite na leitura do resultado", "timed out reading the result"],
  ["report.rs", 404, "report_not_found", {}, "relatório não encontrado", "report not found"],
  ["survey.rs", 409, "survey_busy", {}, "já há um turno, execução ou relatório", "a turn, run or report is already in progress"],
  ["captures.rs", 404, "capture_not_found", {}, "captura não encontrada", "capture not found"],
  ["models_api.rs", 404, "connection_not_found", {}, "conexão não encontrada", "connection not found"],
  ["static_ui.rs", 404, "not_found", {}, "não encontrado", "not found"],
  ["events.rs", 422, "project_id_invalid", {}, "project_id deve conter apenas", "project_id must contain only"],
  ["safe_fs.rs", 409, "project_dir_symlink", {}, "diretório do projeto inseguro (symlink)", "unsafe project folder (symlink)"],
];

describe("erros da API em pt e en, uma amostra de cada área (spec 115 T02)", () => {
  it.each(SAMPLES)("%s: %s %s", async (_area, status, code, params, pt, english) => {
    const key = `serverError.${code}` as MessageKey;
    expect(Object.hasOwn(ptBR, key) && Object.hasOwn(en, key), key).toBe(true);
    stubFetch(status, { error: "texto de reserva do servidor", code, ...(Object.keys(params).length > 0 ? { params } : {}) });
    const r = await getConfig();
    if (r.ok) throw new Error("esperava falha");
    expect(r.error.code).toBe(code);
    const inPt = apiFailureText(r.error, "pt-BR");
    const inEn = apiFailureText(r.error, "en");
    expect(inPt).toContain(pt);
    expect(inEn).toContain(english);
    expect(inPt).not.toBe(inEn);
    expect(inPt).not.toContain("texto de reserva");
    expect(inEn).not.toMatch(/\{\w+\}/);
    // O parâmetro pode vir no idioma do projeto (o motivo do orçamento); o resto não tem pt.
    const fixed = Object.values(params).reduce<string>((text, value) => text.replace(String(value), ""), inEn);
    expect(looksPortuguese(fixed), inEn).toBe(false);
  });

  it("docker_unavailable mostra o nome do item (o de Primeiros passos), ou um genérico", async () => {
    for (const [item, pt, english] of [
      ["docker_permission", "falta: Permissão no socket do Docker", "missing: Docker permission"],
      ["docker_daemon", "falta: Docker daemon", "missing: Docker daemon"],
      ["outro_item", "falta: um requisito do Docker", "missing: a Docker requirement"],
    ] as const) {
      stubFetch(409, { error: "x", code: "docker_unavailable", params: { item }, reason: "docker_unavailable" });
      const r = await getConfig();
      if (r.ok) throw new Error("esperava falha");
      const inPt = apiFailureText(r.error, "pt-BR");
      const inEn = apiFailureText(r.error, "en");
      expect(inPt).toContain(pt);
      expect(inEn).toContain(english);
      expect(inPt).not.toContain(item);
      expect(inEn).not.toContain(item);
    }
  });

  it("relançamento recusado: o motivo pelo código da recusa, sem id cru, e o detalhe técnico", () => {
    const docker = {
      text: "o relançamento em modo docker foi recusado; use Retomar: O Docker é pré-requisito do Nants e não está disponível neste host (falta: docker_cli). Veja Primeiros passos.",
      code: "run.relaunch_failed",
      params: { message: "O Docker é pré-requisito do Nants e não está disponível neste host (falta: docker_cli).", error_code: "docker_unavailable", item: "docker_cli" },
    };
    const dockerPt = systemMessageText(docker, "pt-BR");
    const dockerEn = systemMessageText(docker, "en");
    expect(dockerPt).toContain("Motivo: O Docker é pré-requisito do Nants");
    expect(dockerPt).toContain("falta: Docker CLI");
    expect(dockerEn).toContain("Reason: Docker is a Nants prerequisite");
    expect(dockerEn).toContain("missing: Docker CLI");
    for (const text of [dockerPt, dockerEn]) expect(text).not.toContain("docker_cli");
    expect(looksPortuguese(dockerEn)).toBe(false);

    const detail = "compose.yaml não pode expor o docker.sock";
    const env = {
      text: "x",
      code: "run.relaunch_failed",
      params: { message: "os arquivos do ambiente docker (.nants/) do plano são inválidos", error_code: "environment_invalid", detail },
    };
    const envPt = systemMessageText(env, "pt-BR");
    const envEn = systemMessageText(env, "en");
    expect(envPt).toContain("Motivo: os arquivos do ambiente docker (.nants/) do plano são inválidos");
    expect(envPt).toContain(`Detalhe técnico: ${detail}`);
    expect(envEn).toContain("Reason: the plan's docker environment files (.nants/) are invalid");
    expect(envEn).toContain(`Technical detail: ${detail}`);
    expect(looksPortuguese(envEn.replace(detail, ""))).toBe(false);

    // Mensagem antiga, sem código da recusa: o texto livre gravado continua como motivo.
    const old = { text: "x", code: "run.relaunch_failed", params: { message: "motivo antigo" } };
    expect(systemMessageText(old, "en")).toBe("The relaunch in docker mode was refused; use Resume. Reason: motivo antigo");
  });

  it("todo serverError.* tem texto nos dois dicionários, e o en sem português", () => {
    const keys = Object.keys(ptBR).filter((k) => k.startsWith("serverError."));
    expect(keys.length).toBeGreaterThan(150);
    for (const key of keys) {
      const english = en[key as MessageKey];
      expect(english, key).toBeTruthy();
      expect(looksPortuguese(english), `${key}: ${english}`).toBe(false);
    }
  });
});
