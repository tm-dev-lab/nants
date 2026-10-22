/* Rolagem da Conversa (spec 093, T06), com o botão "ir ao fim" com ícone. */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { AutoScrollBox } from "./AutoScrollBox";
import { findForbiddenGlyphs } from "../../desktop/glyphs";

afterEach(() => {
  cleanup();
  for (const prop of ["scrollHeight", "clientHeight", "scrollTop"]) {
    Reflect.deleteProperty(HTMLElement.prototype, prop);
  }
});

/** Simula a geometria de rolagem do elemento (jsdom não faz layout). */
function mockScrollBox(el: HTMLElement, scrollHeight: number, clientHeight: number) {
  let top = 0;
  Object.defineProperty(el, "scrollHeight", { configurable: true, get: () => scrollHeight });
  Object.defineProperty(el, "clientHeight", { configurable: true, get: () => clientHeight });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => top,
    set: (value: number) => {
      top = value;
    },
  });
}

/** Geometria simulada para TODOS os elementos (usada quando o elemento é montado com dados). */
function mockPrototypeScrollBox(scrollHeight: number, clientHeight: number) {
  const tops = new WeakMap<object, number>();
  Object.defineProperty(HTMLElement.prototype, "scrollHeight", { configurable: true, get: () => scrollHeight });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => clientHeight });
  Object.defineProperty(HTMLElement.prototype, "scrollTop", {
    configurable: true,
    get(this: HTMLElement) {
      return tops.get(this) ?? 0;
    },
    set(this: HTMLElement, value: number) {
      tops.set(this, value);
    },
  });
}

type Item = { id: number; seq?: number };

/** Mensagens ao vivo: ids e `seq` crescentes (como o store marca chegadas ao vivo). */
function ids(from: number, to: number): Item[] {
  return Array.from({ length: to - from + 1 }, (_, i) => ({ id: from + i, seq: from + i }));
}

/** Linhas hidratadas do histórico: sem `seq`. */
function hist(from: number, to: number): Item[] {
  return Array.from({ length: to - from + 1 }, (_, i) => ({ id: from + i }));
}

function renderList(list: Item[]) {
  return list.map((item) => <p key={item.id}>item {item.id}</p>);
}

describe("AutoScrollBox", () => {
  it("cola no fim quando chega item e o usuário está no fim", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 1)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 1))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 500, 200);

    rerender(
      <AutoScrollBox items={ids(1, 2)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 2))}
      </AutoScrollBox>,
    );

    expect(box.scrollTop).toBe(500);
    expect(screen.queryByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ })).toBeNull();
  });

  it("não puxa após o usuário rolar para cima e mostra contagem de novas", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 1)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 1))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 1000, 200);
    box.scrollTop = 100;
    fireEvent.scroll(box);

    rerender(
      <AutoScrollBox items={ids(1, 3)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 3))}
      </AutoScrollBox>,
    );

    expect(box.scrollTop).toBe(100);
    const jump = screen.getByRole("button", { name: "2 novas" });
    fireEvent.click(jump);
    expect(box.scrollTop).toBe(1000);
    expect(screen.queryByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ })).toBeNull();
  });

  it("pinKey novo (envio próprio) desce ao fim mesmo com o usuário rolado para cima", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 1)} pinKey={0} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 1))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 1000, 200);
    box.scrollTop = 100;
    fireEvent.scroll(box);

    rerender(
      <AutoScrollBox items={ids(1, 2)} pinKey={1} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 2))}
      </AutoScrollBox>,
    );

    expect(box.scrollTop).toBe(1000);
    expect(screen.queryByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ })).toBeNull();
  });

  it("começa no fim ao montar com histórico (hidratação)", () => {
    mockPrototypeScrollBox(800, 200);
    render(
      <AutoScrollBox items={ids(1, 5)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 5))}
      </AutoScrollBox>,
    );

    expect(screen.getByRole("region", { name: "Caixa" }).scrollTop).toBe(800);
  });

  it("a área é rolável por teclado e usa a classe de barra discreta", () => {
    render(
      <AutoScrollBox items={ids(1, 0)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 0))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    expect(box.getAttribute("tabindex")).toBe("0");
    expect(box.className).toContain("soft-scroll");
  });

  it("com o teto de 500 itens, nova mensagem (com saída da mais antiga) ainda desce ao fim", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 500)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 500))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 5000, 200);

    rerender(
      <AutoScrollBox items={ids(2, 501)} label="Caixa" className="soft-scroll">
        {renderList(ids(2, 501))}
      </AutoScrollBox>,
    );

    expect(box.scrollTop).toBe(5000);
    expect(screen.queryByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ })).toBeNull();
  });

  it("com o teto de 500 itens e usuário rolado para cima, a contagem acumula a cada inserção", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 500)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 500))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 5000, 200);
    box.scrollTop = 100;
    fireEvent.scroll(box);

    rerender(
      <AutoScrollBox items={ids(2, 501)} label="Caixa" className="soft-scroll">
        {renderList(ids(2, 501))}
      </AutoScrollBox>,
    );
    expect(box.scrollTop).toBe(100);
    expect(screen.getByRole("button", { name: "1 nova" })).not.toBeNull();

    rerender(
      <AutoScrollBox items={ids(3, 502)} label="Caixa" className="soft-scroll">
        {renderList(ids(3, 502))}
      </AutoScrollBox>,
    );
    expect(screen.getByRole("button", { name: "2 novas" })).not.toBeNull();
    expect(box.scrollTop).toBe(100);
  });

  it("hidratação que substitui a lista inteira por histórico (sem seq), no fim, não conta novas", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 3)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 3))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 900, 200);

    rerender(
      <AutoScrollBox items={hist(1001, 1400)} label="Caixa" className="soft-scroll">
        {renderList(hist(1001, 1400))}
      </AutoScrollBox>,
    );

    expect(box.scrollTop).toBe(900);
    expect(screen.queryByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ })).toBeNull();
  });

  it("mudança de tailKey (cartão de plano) no fim desce sem contar novas", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 2)} tailKey={false} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 2))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 700, 200);

    rerender(
      <AutoScrollBox items={ids(1, 2)} tailKey={true} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 2))}
      </AutoScrollBox>,
    );

    expect(box.scrollTop).toBe(700);
    expect(screen.queryByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ })).toBeNull();
  });
  it("F-01: hidratação que insere histórico na frente mantendo a live, no fim, desce ao fim", () => {
    const live: Item[] = [{ id: 1, seq: 1 }];
    const { rerender } = render(
      <AutoScrollBox items={live} label="Caixa" className="soft-scroll">
        {renderList(live)}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 1200, 200);

    const prepended: Item[] = [...hist(10, 11), { id: 1, seq: 1 }];
    rerender(
      <AutoScrollBox items={prepended} label="Caixa" className="soft-scroll">
        {renderList(prepended)}
      </AutoScrollBox>,
    );

    expect(box.scrollTop).toBe(1200);
    expect(screen.queryByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ })).toBeNull();
  });

  it("F-02: rolado para cima, live removida pela hidratação e lista só com histórico não cria contagem", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 2)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 2))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 1000, 200);
    box.scrollTop = 100;
    fireEvent.scroll(box);

    rerender(
      <AutoScrollBox items={hist(50, 60)} label="Caixa" className="soft-scroll">
        {renderList(hist(50, 60))}
      </AutoScrollBox>,
    );

    expect(box.scrollTop).toBe(100);
    expect(screen.getByRole("button", { name: "Ir para o fim" })).not.toBeNull();
    expect(screen.queryByRole("button", { name: /nova/ })).toBeNull();
  });

  it("F-02/F-01: contagem de novas não muda quando a hidratação insere histórico na frente", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 1)} label="Caixa" className="soft-scroll">
        {renderList(ids(1, 1))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 1000, 200);
    box.scrollTop = 100;
    fireEvent.scroll(box);

    rerender(
      <AutoScrollBox items={[...ids(1, 1), ...ids(2, 3)]} label="Caixa" className="soft-scroll">
        {renderList([...ids(1, 1), ...ids(2, 3)])}
      </AutoScrollBox>,
    );
    expect(screen.getByRole("button", { name: "2 novas" })).not.toBeNull();

    const prepended: Item[] = [...hist(50, 52), ...ids(1, 3)];
    rerender(
      <AutoScrollBox items={prepended} label="Caixa" className="soft-scroll">
        {renderList(prepended)}
      </AutoScrollBox>,
    );
    expect(screen.getByRole("button", { name: "2 novas" })).not.toBeNull();
    expect(box.scrollTop).toBe(100);
  });

  describe("crescimento do conteúdo (ResizeObserver)", () => {
    type Callback = () => void;
    let callbacks: Callback[] = [];
    class FakeResizeObserver {
      constructor(callback: Callback) {
        callbacks.push(callback);
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }

    function setup() {
      callbacks = [];
      vi.stubGlobal("ResizeObserver", FakeResizeObserver);
      const view = render(
        <AutoScrollBox items={ids(1, 1)} label="Caixa" className="soft-scroll">
          {renderList(ids(1, 1))}
        </AutoScrollBox>,
      );
      const box = screen.getByRole("region", { name: "Caixa" });
      let height = 500;
      let top = 0;
      Object.defineProperty(box, "scrollHeight", { configurable: true, get: () => height });
      Object.defineProperty(box, "clientHeight", { configurable: true, get: () => 200 });
      Object.defineProperty(box, "scrollTop", { configurable: true, get: () => top, set: (v: number) => (top = v) });
      return { view, box, grow: (to: number) => { height = to; act(() => callbacks.forEach((cb) => cb())); } };
    }

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("no fim: altura que muda depois (sem nova mensagem) mantém o fim visível", () => {
      const { box, grow } = setup();
      box.scrollTop = 300;
      fireEvent.scroll(box);

      grow(900);

      expect(box.scrollTop).toBe(900);
    });

    it("a até 80 px do fim ainda conta como no fim", () => {
      const { box, grow } = setup();
      box.scrollTop = 230; // 500 - 230 - 200 = 70 px
      fireEvent.scroll(box);

      grow(800);

      expect(box.scrollTop).toBe(800);
    });

    it("rolado para cima: o crescimento não puxa e o botão '↓' leva ao fim", () => {
      const { box, grow } = setup();
      box.scrollTop = 50;
      fireEvent.scroll(box);

      grow(900);

      expect(box.scrollTop).toBe(50);
      fireEvent.click(screen.getByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ }));
      expect(box.scrollTop).toBe(900);
    });

    it("scroll tardio gerado pelo próprio código não solta o grudado (hidratação e crescimento)", () => {
      const { box, grow } = setup();

      grow(900);
      fireEvent.scroll(box);
      grow(1400);
      fireEvent.scroll(box);

      expect(screen.queryByRole("button", { name: /^(Ir para o fim|\d+ novas?)$/ })).toBeNull();
      expect(box.scrollTop).toBe(1400);
    });

    it("abertura começa no fim quando o conteúdo cresce após montar", () => {
      const { box, grow } = setup();

      grow(1200);

      expect(box.scrollTop).toBe(1200);
    });
  });
});

describe("AutoScrollBox: ícone no botão de ir ao fim", () => {
  it("a seta é ícone, não caractere", () => {
    const { container, rerender } = render(
      <AutoScrollBox items={ids(1, 1)} label="Caixa" className="x">
        {renderList(ids(1, 1))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 1000, 200);
    box.scrollTop = 100;
    fireEvent.scroll(box);
    rerender(
      <AutoScrollBox items={ids(1, 2)} label="Caixa" className="x">
        {renderList(ids(1, 2))}
      </AutoScrollBox>,
    );
    const jump = screen.getByRole("button", { name: "1 nova" });
    expect(jump.querySelector("svg[data-icon='arrow-down']")).not.toBeNull();
    expect(findForbiddenGlyphs(container)).toEqual([]);
  });

  it("A-11 (094/T08): botão redondo só com o ícone; o rótulo no nome acessível e na dica; o número num selo", () => {
    const { rerender } = render(
      <AutoScrollBox items={ids(1, 1)} label="Caixa" className="x">
        {renderList(ids(1, 1))}
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Caixa" });
    mockScrollBox(box, 1000, 200);
    box.scrollTop = 100;
    fireEvent.scroll(box);
    const end = screen.getByRole("button", { name: "Ir para o fim" });
    expect(end.classList.contains("nd-iconbtn")).toBe(true);
    expect(end.classList.contains("nd-jump")).toBe(true);
    expect(end.getAttribute("title")).toBe("Ir para o fim");
    expect(end.textContent).toBe("");
    expect(end.querySelector("[data-jump-count]")).toBeNull();
    rerender(
      <AutoScrollBox items={ids(1, 3)} label="Caixa" className="x">
        {renderList(ids(1, 3))}
      </AutoScrollBox>,
    );
    const news = screen.getByRole("button", { name: "2 novas" });
    const count = news.querySelector("[data-jump-count]");
    expect(count?.textContent).toBe("2");
    expect(count?.getAttribute("aria-hidden")).toBe("true");
  });
});
