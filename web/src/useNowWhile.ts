import { useEffect, useState } from "react";

/**
 * Instante atual, com um render novo a cada `ms` só enquanto `active` (sem relógio parado na tela).
 * O valor é sempre a hora do render: um render causado por dados novos (outra operação, outro
 * início) não mede o tempo com o relógio do último passo.
 */
export function useNowWhile(active: boolean, ms: number): number {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!active) return undefined;
    const timer = setInterval(() => setTick((n) => n + 1), ms);
    return () => clearInterval(timer);
  }, [active, ms]);
  return Date.now();
}
