import { describe, expect, it } from "vitest";

import { makeSpec, makeTask } from "../../test-utils";
import { buildColumns } from "../board/boardModel";
import { renumberMap, tasksInExecutionOrder } from "./executionOrder";

describe("renumberMap (spec 128 T05, C-07)", () => {
  it("follows the server rule for the Test 6 order", () => {
    const order = ["T01", "T08", "T10", "T02", "T03", "T04", "T05", "T13", "T14", "T12", "T15", "T11", "T06", "T09", "T07"];
    expect(renumberMap({ execution_order: order }).slice(0, 3)).toEqual([
      ["T08", "T02"],
      ["T10", "T03"],
      ["T02", "T04"],
    ]);
  });

  it("is empty when nothing changes, without the order or with other ids", () => {
    expect(renumberMap({ execution_order: ["T01", "T02", "T03"] })).toEqual([]);
    expect(renumberMap({})).toEqual([]);
    expect(renumberMap({ execution_order: ["T02", "auth"] })).toEqual([]);
  });
});

const ids = (tasks: { id: string }[]): string[] => tasks.map((task) => task.id);

describe("tasksInExecutionOrder (spec 128 T04)", () => {
  const tasks = ["T01", "T02", "T03", "T08", "T10"].map((id) => makeTask(id, `Tarefa ${id}`, "TODO"));

  it("lists the tasks in the server order, with the ids unchanged", () => {
    const spec = { ...makeSpec("001", "Plano", "AWAITING_APPROVAL", tasks), execution_order: ["T01", "T08", "T10", "T02", "T03"] };
    expect(ids(tasksInExecutionOrder(spec))).toEqual(["T01", "T08", "T10", "T02", "T03"]);
    expect(ids(spec.tasks)).toEqual(["T01", "T02", "T03", "T08", "T10"]);
  });

  it("keeps the received order without the field and puts unknown tasks last", () => {
    expect(ids(tasksInExecutionOrder(makeSpec("001", "Plano", "AWAITING_APPROVAL", tasks)))).toEqual(["T01", "T02", "T03", "T08", "T10"]);
    const partial = { ...makeSpec("001", "Plano", "AWAITING_APPROVAL", tasks), execution_order: ["T10", "T08"] };
    expect(ids(tasksInExecutionOrder(partial))).toEqual(["T10", "T08", "T01", "T02", "T03"]);
  });

  it("orders the board columns the same way", () => {
    const spec = { ...makeSpec("001", "Plano", "IN_PROGRESS", tasks), execution_order: ["T01", "T08", "T10", "T02", "T03"] };
    const todo = buildColumns(tasksInExecutionOrder(spec), new Set()).find((column) => column.key === "TODO");
    expect(ids(todo?.tasks ?? [])).toEqual(["T01", "T08", "T10", "T02", "T03"]);
  });
});
