/* ChoicesForm na Conversa (spec 093, T06). */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ChoicesForm } from "./ChoicesForm";
import { findForbiddenGlyphs } from "../../desktop/glyphs";

const questions = [
  { id: "a", text: "Login", multi: false, options: ["Sim", "Não"] },
  { id: "b", text: "Extras", multi: true, options: ["X", "Y"] },
];

afterEach(cleanup);

describe("ChoicesForm", () => {
  it("habilita o envio só com tudo respondido e formata Pergunta: resposta", () => {
    const onSubmit = vi.fn();
    render(<ChoicesForm questions={questions} answered={null} onSubmit={onSubmit} />);
    const send = screen.getByRole("button", { name: "Enviar respostas" });
    expect((send as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("radio", { name: "Sim" }));
    expect((send as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: "X" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Y" }));
    expect((send as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(send);
    expect(onSubmit).toHaveBeenCalledWith("Login: Sim\nExtras: X, Y");
  });
  it("Outro exige texto", () => {
    const onSubmit = vi.fn();
    render(<ChoicesForm questions={[questions[0]!]} answered={null} onSubmit={onSubmit} />);
    fireEvent.click(screen.getByRole("radio", { name: "Outro" }));
    expect((screen.getByRole("button", { name: "Enviar respostas" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Outro: Login"), { target: { value: "Talvez" } });
    fireEvent.click(screen.getByRole("button", { name: "Enviar respostas" }));
    expect(onSubmit).toHaveBeenCalledWith("Login: Talvez");
  });
  it("Enviar respostas tem a mesma altura e tipografia de Decida você, habilitado ou não (spec 081/T03)", () => {
    render(<ChoicesForm questions={[questions[0]!]} answered={null} onSubmit={() => {}} />);
    const send = screen.getByRole("button", { name: "Enviar respostas" });
    const decide = screen.getByRole("button", { name: "Decida você" });
    const metrics = (el: HTMLElement): string[] =>
      el.className.split(/\s+/).filter((c) => /^(px-|py-|text-xs$|rounded-lg$)/.test(c)).sort();
    expect(metrics(send)).toEqual(metrics(decide));
    expect((send as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("radio", { name: "Sim" }));
    expect((send as HTMLButtonElement).disabled).toBe(false);
    expect(metrics(send)).toEqual(metrics(decide));
  });
  it("respondido mostra resumo estático sem botões", () => {
    render(<ChoicesForm questions={questions} answered={"Login: Sim"} onSubmit={() => {}} />);
    expect(screen.getByText("Login: Sim")).not.toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
  });
});

import { fireEvent as _fe, render as _r, screen as _s } from "@testing-library/react";
import { describe as _d2, expect as _e2, it as _i2, vi as _vi } from "vitest";
import { ChoicesForm as _CF } from "./ChoicesForm";

_d2("ChoicesForm: recomendação", () => {
  const qs = [{ id: "a", text: "Banco?", multi: false, options: ["SQLite", "Postgres"], recommended: ["SQLite"], why: "simples" }];
  _i2("pré-seleciona, mostra selo e porquê, envia", () => {
    const fn = _vi.fn();
    _r(<_CF questions={qs} answered={null} onSubmit={fn} />);
    _e2(_s.getByText("recomendado")).toBeTruthy();
    _e2(_s.getByText("simples")).toBeTruthy();
    _fe.click(_s.getByRole("button", { name: "Enviar respostas" }));
    _e2(fn).toHaveBeenCalledWith("Banco? SQLite");
  });
  _i2("Decida você envia o texto fixo", () => {
    const fn = _vi.fn();
    _r(<_CF questions={qs} answered={null} onSubmit={fn} />);
    _fe.click(_s.getByRole("button", { name: "Decida você" }));
    _e2(fn).toHaveBeenCalledWith("Siga com as recomendações.");
  });
  it("marcas de múltipla escolha são ícones (sem caractere)", () => {
    const { container } = render(<ChoicesForm questions={questions} answered={null} onSubmit={() => undefined} />);
    fireEvent.click(screen.getByRole("checkbox", { name: "X" }));
    expect(container.querySelector("[data-checked='true'] svg[data-icon='check']")).not.toBeNull();
    expect(container.textContent).not.toContain("\u2713");
    expect(findForbiddenGlyphs(container)).toEqual([]);
  });
});
