import { useState } from "react";

import { useActiveMeta, useStore } from "../store/useStore";
import type { Conjunction, FilterCondition, FilterGroup, FilterNode, FilterOp } from "../types";
import { Close } from "./Icons";
import { Modal } from "./Modal";

const OP_OPTIONS: { value: FilterOp; label: string }[] = [
  { value: "contains", label: "contains" },
  { value: "notContains", label: "does not contain" },
  { value: "equals", label: "equals" },
  { value: "notEquals", label: "does not equal" },
  { value: "startsWith", label: "starts with" },
  { value: "endsWith", label: "ends with" },
  { value: "gt", label: ">" },
  { value: "gte", label: "≥" },
  { value: "lt", label: "<" },
  { value: "lte", label: "≤" },
  { value: "isEmpty", label: "is empty" },
  { value: "notEmpty", label: "is not empty" },
  { value: "regex", label: "matches regex" },
];

const NO_VALUE: ReadonlySet<FilterOp> = new Set(["isEmpty", "notEmpty"]);
const MAX_DEPTH = 3;

// Monotonic id source for stable React keys on builder nodes (client-only;
// the backend ignores the `id` field).
let nodeIdSeq = 0;
const nextId = () => `n${nodeIdSeq++}`;

const newCondition = (): FilterCondition => ({
  type: "condition",
  id: nextId(),
  column: 0,
  op: "contains",
  value: "",
  caseSensitive: false,
});

const newGroup = (): FilterGroup => ({
  type: "group",
  id: nextId(),
  conjunction: "and",
  nodes: [newCondition()],
});

export function FilterDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const storedSpec = useStore((s) => s.filter.spec);
  const applyFilter = useStore((s) => s.applyFilter);
  const clearFilter = useStore((s) => s.clearFilter);
  const updateFilterSpec = useStore((s) => s.updateFilterSpec);
  const [spec, setSpec] = useState<FilterGroup>(storedSpec);

  if (!meta) return null;

  const apply = () => {
    void applyFilter(spec);
    onClose();
  };
  const clear = () => {
    void clearFilter();
    onClose();
  };
  // Closing without applying preserves the builder edits for next time.
  const cancel = () => {
    updateFilterSpec(spec);
    onClose();
  };

  return (
    <Modal
      title="Filter rows"
      onClose={cancel}
      size="lg"
      footer={
        <>
          {meta.filtered && (
            <button
              onClick={clear}
              className="mr-auto rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
            >
              Clear filter
            </button>
          )}
          <button
            onClick={cancel}
            className="rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            Cancel
          </button>
          <button
            onClick={apply}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500"
          >
            Apply filter
          </button>
        </>
      }
    >
      <GroupEditor group={spec} headers={meta.headers} depth={0} onChange={setSpec} />
    </Modal>
  );
}

function GroupEditor({
  group,
  headers,
  depth,
  onChange,
  onRemove,
}: {
  group: FilterGroup;
  headers: string[];
  depth: number;
  onChange: (g: FilterGroup) => void;
  onRemove?: () => void;
}) {
  const setConjunction = (conjunction: Conjunction) => onChange({ ...group, conjunction });
  const setNode = (i: number, node: FilterNode) =>
    onChange({ ...group, nodes: group.nodes.map((n, idx) => (idx === i ? node : n)) });
  const removeNode = (i: number) =>
    onChange({ ...group, nodes: group.nodes.filter((_, idx) => idx !== i) });
  const addCondition = () => onChange({ ...group, nodes: [...group.nodes, newCondition()] });
  const addGroup = () => onChange({ ...group, nodes: [...group.nodes, newGroup()] });

  return (
    <div
      className={`space-y-2 rounded-lg border border-zinc-200 p-2.5 dark:border-zinc-700 ${
        depth > 0 ? "bg-zinc-50 dark:bg-zinc-800/40" : ""
      }`}
    >
      <div className="flex items-center gap-2 text-xs text-zinc-500">
        <span>Match</span>
        <div className="flex overflow-hidden rounded border border-zinc-300 dark:border-zinc-600">
          <button
            onClick={() => setConjunction("and")}
            className={`px-2 py-0.5 ${group.conjunction === "and" ? "bg-violet-600 text-white" : "text-zinc-500"}`}
          >
            ALL
          </button>
          <button
            onClick={() => setConjunction("or")}
            className={`px-2 py-0.5 ${group.conjunction === "or" ? "bg-violet-600 text-white" : "text-zinc-500"}`}
          >
            ANY
          </button>
        </div>
        <span>of the following:</span>
        {onRemove && (
          <button
            onClick={onRemove}
            title="Remove group"
            className="ml-auto rounded p-0.5 text-zinc-400 hover:bg-zinc-200 dark:hover:bg-zinc-700"
          >
            <Close className="h-3.5 w-3.5" />
          </button>
        )}
      </div>

      <div className="space-y-1.5">
        {group.nodes.map((node, i) =>
          node.type === "group" ? (
            <GroupEditor
              key={node.id}
              group={node}
              headers={headers}
              depth={depth + 1}
              onChange={(g) => setNode(i, g)}
              onRemove={() => removeNode(i)}
            />
          ) : (
            <ConditionRow
              key={node.id}
              condition={node}
              headers={headers}
              onChange={(c) => setNode(i, c)}
              onRemove={() => removeNode(i)}
            />
          ),
        )}
        {group.nodes.length === 0 && (
          <p className="px-1 text-xs text-zinc-400">No conditions — matches every row.</p>
        )}
      </div>

      <div className="flex gap-3 text-sm">
        <button
          onClick={addCondition}
          className="text-violet-600 hover:underline dark:text-violet-400"
        >
          + Condition
        </button>
        {depth < MAX_DEPTH && (
          <button
            onClick={addGroup}
            className="text-violet-600 hover:underline dark:text-violet-400"
          >
            + Group
          </button>
        )}
      </div>
    </div>
  );
}

function ConditionRow({
  condition,
  headers,
  onChange,
  onRemove,
}: {
  condition: FilterCondition;
  headers: string[];
  onChange: (c: FilterCondition) => void;
  onRemove: () => void;
}) {
  const needsValue = !NO_VALUE.has(condition.op);
  return (
    <div className="flex items-center gap-1.5">
      <select
        value={condition.column}
        onChange={(e) => onChange({ ...condition, column: Number(e.target.value) })}
        className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700"
      >
        {headers.map((h, c) => (
          <option key={c} value={c} className="dark:bg-zinc-800">
            {h || `Column ${c + 1}`}
          </option>
        ))}
      </select>
      <select
        value={condition.op}
        onChange={(e) => onChange({ ...condition, op: e.target.value as FilterOp })}
        className="rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700"
      >
        {OP_OPTIONS.map((o) => (
          <option key={o.value} value={o.value} className="dark:bg-zinc-800">
            {o.label}
          </option>
        ))}
      </select>
      {needsValue && (
        <input
          value={condition.value}
          onChange={(e) => onChange({ ...condition, value: e.target.value })}
          placeholder="value"
          className="min-w-0 flex-1 rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700"
        />
      )}
      {needsValue && (
        <button
          onClick={() => onChange({ ...condition, caseSensitive: !condition.caseSensitive })}
          title="Case sensitive"
          className={`rounded border px-1.5 py-1 text-xs ${
            condition.caseSensitive
              ? "border-violet-500 bg-violet-100 text-violet-700 dark:bg-violet-500/20 dark:text-violet-300"
              : "border-zinc-300 text-zinc-400 dark:border-zinc-700"
          }`}
        >
          Aa
        </button>
      )}
      <button
        onClick={onRemove}
        title="Remove condition"
        className="rounded p-1 text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800"
      >
        <Close className="h-4 w-4" />
      </button>
    </div>
  );
}
