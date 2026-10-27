/*
 * Spec 130 T05: os eventos da forma da execução (`app_mode_changed`) e do Console
 * (`app_exec_started`, `app_exec_output`, `app_exec_finished`) passam a ser conhecidos pela web; e o
 * estado do app traz o modo, o comando, o resultado de um "terminou" e a falha guardada.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { getAppStatus, toExecView } from "./client";
import { isConsoleEvent } from "./consoleEvents";
import { parseApiEvent } from "./events";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("eventos da spec 130", () => {
  it("app_mode_changed: aceito com o modo do vocabulário; modo desconhecido é descartado", () => {
    const ok = parseApiEvent({ event_type: "app_mode_changed", project_id: "p1", payload: { spec_id: "s1", mode: "finished", previous: "running", exit_code: 0 } });
    expect(ok?.payload).toEqual({ spec_id: "s1", mode: "finished", previous: "running", exit_code: 0 });
    expect(parseApiEvent({ event_type: "app_mode_changed", project_id: "p1", payload: { spec_id: "s1", mode: "docker" } })).toBeNull();
  });

  it("app_exec_*: started, output (seq inteiro) e finished; vão para o Console", () => {
    const started = parseApiEvent({ event_type: "app_exec_started", project_id: "p1", payload: { exec_id: "e1", spec_id: "s1", command: "ls", started_at: "x", timeout_secs: 300 } });
    expect(started !== null && isConsoleEvent(started)).toBe(true);
    expect(parseApiEvent({ event_type: "app_exec_output", project_id: "p1", payload: { exec_id: "e1", seq: 1, text: "ok\n", truncated: false } })).not.toBeNull();
    expect(parseApiEvent({ event_type: "app_exec_output", project_id: "p1", payload: { exec_id: "e1", seq: "1", text: "ok\n", truncated: false } })).toBeNull();
    expect(parseApiEvent({ event_type: "app_exec_finished", project_id: "p1", payload: { exec_id: "e1", status: "exited", exit_code: 0 } })).not.toBeNull();
    expect(parseApiEvent({ event_type: "app_exec_finished", project_id: "p1", payload: { exec_id: "e1", exit_code: 0 } })).toBeNull();
  });
});

describe("estado do app e Console (cliente)", () => {
  it("GET .../app: modo, comando, terminou e falha com o próximo passo; o que foge do formato some", async () => {
    const body = {
      state: "exited",
      mode: "finished",
      spec_id: "s1",
      url: null,
      started_at: "x",
      exit_code: 0,
      command: "ledger report",
      ports: [],
      notices: [],
      finished: { exit_code: 0, output: "ok", files: [{ path: "a.md", change: "added" }, { path: "b", change: "deleted" }, { path: 3 }], files_total: 2, outside_work: ["/tmp/x"], outside_work_total: 1 },
      failure: { spec_id: "s1", code: "app_docker_up_failed", detail: "boom", at: "2026-10-27T12:00:00Z", action: "ask_architect_fix" },
    };
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify(body), { status: 200 })));
    const r = await getAppStatus("p1");
    if (!r.ok) throw new Error("falhou");
    expect(r.data.mode).toBe("finished");
    expect(r.data.command).toBe("ledger report");
    expect(r.data.finished?.files).toEqual([{ path: "a.md", change: "added" }]);
    expect(r.data.finished?.files_total).toBe(2);
    expect(r.data.failure).toEqual({ spec_id: "s1", code: "app_docker_up_failed", detail: "boom", at: "2026-10-27T12:00:00Z", action: "ask_architect_fix" });
  });

  it("servidor antigo (mode docker, sem os campos novos): nulos", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ state: "running", mode: "docker", spec_id: "s1", ports: [] }), { status: 200 })));
    const r = await getAppStatus("p1");
    if (!r.ok) throw new Error("falhou");
    expect([r.data.mode, r.data.command, r.data.finished, r.data.failure]).toEqual([null, null, null, null]);
  });

  it("toExecView: aviso com o próximo passo; status desconhecido vira falha", () => {
    const v = toExecView({ exec_id: "e1", command: "ls", status: "timed_out", notice: { code: "app_exec_timeout", params: { seconds: 300 }, action: "edit_command" } });
    expect(v?.notice).toEqual({ code: "app_exec_timeout", params: { seconds: 300 }, action: "edit_command" });
    expect(toExecView({ exec_id: "e1", status: "weird" })?.status).toBe("failed");
    expect(toExecView({ status: "exited" })).toBeNull();
  });
});
