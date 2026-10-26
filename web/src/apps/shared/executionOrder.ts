/*
 * Ordem de execução das tarefas (spec 128 T04). O servidor calcula a ordem (`execution_order`) e a
 * tela só a usa: a lista do plano e o quadro mostram as tarefas nessa ordem, com os ids como estão.
 */
import type { SpecView, TaskView } from "../../api/types";

/**
 * Tarefas da spec na ordem de execução. Sem o campo (ciclo, servidor antigo), a ordem recebida.
 * Tarefa fora do campo vai para o fim, na ordem recebida.
 */
export function tasksInExecutionOrder(spec: Pick<SpecView, "tasks" | "execution_order">): TaskView[] {
  const order = spec.execution_order;
  if (order === undefined || order.length === 0) return spec.tasks;
  const position = new Map(order.map((id, index) => [id, index]));
  const rank = (task: TaskView): number => position.get(task.id) ?? order.length;
  return spec.tasks
    .map((task, index) => ({ task, index }))
    .sort((a, b) => rank(a.task) - rank(b.task) || a.index - b.index)
    .map(({ task }) => task);
}

const TWO_DIGIT_ID = /^T\d{2}$/;

/**
 * Mapa da renumeração que a aprovação fará (spec 128 T05, C-07), na regra do servidor
 * (`execution_order::renumber_map`): cada posição da ordem de execução recebe o id da mesma posição
 * entre os ids ordenados. Só as tarefas que mudam, como pares `[antigo, novo]`, na ordem dos ids
 * novos. Vazio sem a ordem, com a ordem igual aos ids, ou com algum id fora de `T` + 2 dígitos.
 */
export function renumberMap(spec: Pick<SpecView, "execution_order">): Array<readonly [string, string]> {
  const order = spec.execution_order;
  if (order === undefined || order.length === 0 || !order.every((id) => TWO_DIGIT_ID.test(id))) return [];
  const sorted = [...order].sort();
  return order
    .map((old, index) => [old, sorted[index] ?? old] as const)
    .filter(([old, next]) => old !== next)
    .sort((a, b) => (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0));
}
