/**
 * Ordered multi-column picker: clicking toggles membership, and the pick
 * ORDER is preserved (shown as a numbered badge) — used where column order
 * matters (merge transforms, duplicate keys).
 */
export function ColumnsPicker({
  headers,
  value,
  onChange,
}: {
  headers: string[];
  value: number[];
  onChange: (cols: number[]) => void;
}) {
  const toggle = (col: number) => {
    onChange(value.includes(col) ? value.filter((c) => c !== col) : [...value, col]);
  };
  return (
    <span className="flex max-w-md flex-wrap gap-1">
      {headers.map((h, i) => {
        const order = value.indexOf(i);
        return (
          <button
            key={i}
            onClick={(e) => {
              e.preventDefault();
              toggle(i);
            }}
            className={`rounded border px-1.5 py-0.5 text-[11px] ${
              order >= 0
                ? "border-violet-400 bg-violet-50 text-violet-700 dark:border-violet-500/50 dark:bg-violet-500/10 dark:text-violet-300"
                : "border-zinc-300 text-zinc-500 hover:bg-zinc-100 dark:border-zinc-700 dark:hover:bg-zinc-800"
            }`}
            title={order >= 0 ? `Position ${order + 1}` : "Add"}
          >
            {order >= 0 && <span className="mr-0.5 tabular-nums">{order + 1}·</span>}
            {h.trim() || `Column ${i + 1}`}
          </button>
        );
      })}
    </span>
  );
}
