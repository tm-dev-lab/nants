/*
 * Contrato com o servidor (literais no formato de chat.rs, projects.rs e approvals.rs).
 */
import { describe, expect, it } from "vitest";
import { parseEventData } from "./api/events";
import type { ChatReply, ProjectState } from "./api/types";

describe("contrato do chat e do estado (T05)", () => {
  it("agent_message do Arquiteto com turn_id (literal do servidor) é válido", () => {
    const data =
      '{"event_type":"agent_message","project_id":"p1","payload":{"agent":"architect","text":"oi","turn_id":"0123456789abcdef0123456789abcdef"}}';
    const event = parseEventData(data);
    expect(event?.event_type).toBe("agent_message");
    if (event?.event_type !== "agent_message") throw new Error("evento inesperado");
    expect(event.payload.turn_id).toBe("0123456789abcdef0123456789abcdef");
  });

  it("agent_message sem turn_id continua válido (compatibilidade)", () => {
    const data = '{"event_type":"agent_message","project_id":"p1","payload":{"agent":"architect","text":"oi"}}';
    expect(parseEventData(data)).not.toBeNull();
  });

  it("turn_id presente e não-string torna o evento inválido", () => {
    const data = '{"event_type":"agent_message","project_id":"p1","payload":{"agent":"architect","text":"oi","turn_id":42}}';
    expect(parseEventData(data)).toBeNull();
  });

  it("ChatReply e ProjectState seguem o formato do servidor", () => {
    const reply: ChatReply = {
      reply: "Plano pronto.",
      draft_updated: true,
      turn_id: "0123456789abcdef0123456789abcdef",
    };
    const state: ProjectState = {
      id: "p1",
      name: "api",
      is_git_repo: true,
      initialized: true,
      base_branch: "main",
      test_command: "cargo test",
      max_attempts: 5,
      max_test_cycles: 5,
      test_command_redacted: false,
      running: false,
      run: null,
      specs: [],
      pending_permissions: [{ request_id: "r1", agent: "architect", command: "git log", task_id: null }],
    };
    expect(reply.turn_id).toHaveLength(32);
    expect(state.pending_permissions[0]?.task_id).toBeNull();
  });
});
