/*
 * Rolagem restaurada da Conversa (spec 093, T06):
 * a posição vem do ui-state do espaço (`restore.initial`) e cada mudança vai para `restore.onSave`.
 */
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { AutoScrollBox, type ScrollPosition } from "./AutoScrollBox";

/** "ui-state" simulado: a última posição gravada. */
let saved: ScrollPosition | null = null;
const onSave = (position: ScrollPosition): void => {
  saved = position;
};

afterEach(() => {
  cleanup();
  saved = null;
  for (const prop of ["scrollHeight", "clientHeight", "scrollTop"]) {
    Reflect.deleteProperty(HTMLElement.prototype, prop);
  }
});

/** Geometria simulada para todos os elementos (jsdom não faz layout); a altura pode crescer. */
function mockGeometry(height: { value: number }, clientHeight: number): void {
  const tops = new WeakMap<object, number>();
  Object.defineProperty(HTMLElement.prototype, "scrollHeight", { configurable: true, get: () => height.value });
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
const items = (n: number): Item[] => Array.from({ length: n }, (_, i) => ({ id: i + 1 }));

function box(list: Item[]) {
  return (
    <AutoScrollBox items={list} label="Conversa" className="soft-scroll" restore={{ initial: saved, onSave }}>
      {list.map((item) => (
        <p key={item.id}>item {item.id}</p>
      ))}
    </AutoScrollBox>
  );
}

const region = (): HTMLElement => screen.getByRole("region", { name: "Conversa" });

describe("AutoScrollBox: posição restaurada", () => {
  it("rolada para cima: guarda a posição e volta nela ao remontar, sem pular para o fim", () => {
    const height = { value: 1000 };
    mockGeometry(height, 200);
    const first = render(box(items(10)));
    region().scrollTop = 300;
    fireEvent.scroll(region());
    expect(saved).toEqual({ atBottom: false, top: 300 });
    first.unmount();

    const second = render(box(items(10)));
    expect(region().scrollTop).toBe(300);
    expect(screen.getByRole("button", { name: /Ir para o fim/ })).not.toBeNull();
    // Nova mensagem não arrasta para o fim.
    second.rerender(box([...items(10), { id: 11, seq: 1 }]));
    expect(region().scrollTop).toBe(300);
  });

  it("no fim: volta no fim e segue as novas", () => {
    const height = { value: 1000 };
    mockGeometry(height, 200);
    saved = { atBottom: true };
    const { rerender } = render(box(items(10)));
    expect(region().scrollTop).toBe(1000);
    expect(screen.queryByRole("button", { name: /Ir para o fim/ })).toBeNull();
    height.value = 1200;
    rerender(box([...items(10), { id: 11, seq: 1 }]));
    expect(region().scrollTop).toBe(1200);
  });

  it("histórico chega depois: reposiciona quando o conteúdo alcança a altura guardada", () => {
    const height = { value: 0 };
    mockGeometry(height, 200);
    saved = { atBottom: false, top: 500 };
    const { rerender } = render(box([]));
    height.value = 400;
    rerender(box(items(3)));
    expect(region().scrollTop).toBe(200);
    height.value = 1500;
    rerender(box(items(20)));
    expect(region().scrollTop).toBe(500);
  });

  it("Ir para o fim grava a posição no fim", () => {
    const height = { value: 1000 };
    mockGeometry(height, 200);
    saved = { atBottom: false, top: 100 };
    render(box(items(10)));
    fireEvent.click(screen.getByRole("button", { name: /Ir para o fim/ }));
    expect(saved).toEqual({ atBottom: true });
  });

});
