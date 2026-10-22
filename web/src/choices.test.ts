import { describe, expect, it } from "vitest";
import { formatAnswers, parseChoices } from "./choices";

const q = (n: number, opts = ["A", "B"]) =>
  Array.from({ length: n }, (_, i) => ({ id: `q${i}`, text: `P${i}`, multi: false, options: opts }));
const block = (obj: unknown) => "Texto\n```nants-choices\n" + JSON.stringify(obj) + "\n```\n";

describe("parseChoices", () => {
  it("extrai bloco válido e remove do texto", () => {
    const r = parseChoices(block({ questions: q(2) }));
    expect(r.questions).toHaveLength(2);
    expect(r.text).toBe("Texto");
  });
  it("JSON inválido: sem botões e sem bloco cru", () => {
    const r = parseChoices("Oi\n```nants-choices\n{nope\n```");
    expect(r.questions).toBeNull();
    expect(r.text).toBe("Oi");
  });
  it("respeita limites", () => {
    expect(parseChoices(block({ questions: q(5) })).questions).toBeNull();
    expect(parseChoices(block({ questions: q(1, ["A"]) })).questions).toBeNull();
    expect(parseChoices(block({ questions: q(1, ["1", "2", "3", "4", "5", "6", "7"]) })).questions).toBeNull();
    expect(parseChoices(block({ questions: q(4, ["1", "2", "3", "4", "5", "6"]) })).questions).toHaveLength(4);
  });
  it("bloco no meio do texto", () => {
    const r = parseChoices("Antes\n```nants-choices\n" + JSON.stringify({ questions: q(1) }) + "\n```\nDepois");
    expect(r.questions).toHaveLength(1);
    expect(r.text).toBe("Antes\nDepois");
  });
  it("usa o último bloco; sem bloco devolve o texto", () => {
    const two = block({ questions: q(1) }) + block({ questions: q(2) });
    expect(parseChoices(two).questions).toHaveLength(2);
    expect(parseChoices("só texto")).toEqual({ text: "só texto", questions: null });
  });
});

describe("formatAnswers", () => {
  it("uma linha por pergunta", () => {
    expect(formatAnswers(q(2), [["A"], ["A", "B"]])).toBe("P0: A\nP1: A, B");
    const withMark = [{ id: "x", text: "Login?", multi: false, options: ["a", "b"] }];
    expect(formatAnswers(withMark, [["Sim"]])).toBe("Login? Sim");
  });
});

import { extractPremises as _ep, parseChoices as _pc } from "./choices";
import { describe as _d, expect as _e, it as _i } from "vitest";

_d("recommended/why/premissas", () => {
  const blk = (q: object) => "```nants-choices\n" + JSON.stringify({ questions: [q] }) + "\n```";
  _i("aceita recommended string/lista e why; ignora inválido", () => {
    const a = _pc(blk({ id: "a", text: "Q?", options: ["x", "y"], recommended: "y", why: " porque " })).questions?.[0];
    _e(a?.recommended).toEqual(["y"]);
    _e(a?.why).toBe("porque");
    const b = _pc(blk({ id: "a", text: "Q?", multi: true, options: ["x", "y"], recommended: ["x", "z", "y"] })).questions?.[0];
    _e(b?.recommended).toEqual(["x", "y"]);
    const c = _pc(blk({ id: "a", text: "Q?", options: ["x", "y"], recommended: "z", why: "w" })).questions?.[0];
    _e(c?.recommended).toEqual([]);
    _e(c?.why).toBeUndefined();
  });
  _i("extrai premissas", () => {
    _e(_ep("# T\n## Premissas\n- a\n- b\n\n## Outro\n- c")).toEqual(["a", "b"]);
    _e(_ep("# T")).toEqual([]);
  });
});
