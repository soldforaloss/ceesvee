import { useEffect, useMemo, useRef, useState } from "react";

import { registry, type AppCommand } from "../lib/commands";
import { bindingLabel, effectiveBindings } from "../lib/shortcuts";
import { useStore } from "../store/useStore";

/**
 * The command palette (F11): fuzzy search over every registered command,
 * fully keyboard-driven. Unavailable commands stay listed with the reason
 * they cannot run. Commands with an argument (go to row/cell) switch the
 * input into a second, argument-collecting stage.
 */
export function CommandPalette() {
  const open = useStore((s) => s.paletteOpen);
  const argCommandId = useStore((s) => s.paletteArgCommandId);
  const setOpen = useStore((s) => s.setPaletteOpen);
  const overrides = useStore((s) => s.settings?.shortcutOverrides);

  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const [argFor, setArgFor] = useState<AppCommand | null>(null);
  const [argError, setArgError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const bindings = useMemo(
    () => effectiveBindings(registry.defaultBindings(), overrides),
    [overrides],
  );

  // Reset on every open; honour a requested argument mode (e.g. Ctrl+G).
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setIndex(0);
    setArgError(null);
    setArgFor(argCommandId ? (registry.byId(argCommandId) ?? null) : null);
    // Focus after the overlay mounts.
    setTimeout(() => inputRef.current?.focus(), 0);
  }, [open, argCommandId]);

  const results = useMemo(() => {
    if (!open || argFor) return [];
    return registry.search(query).slice(0, 50);
  }, [open, argFor, query]);

  useEffect(() => setIndex(0), [query]);

  // Keep the highlighted row visible while navigating with the keyboard.
  useEffect(() => {
    listRef.current?.querySelector(`[data-index="${index}"]`)?.scrollIntoView({ block: "nearest" });
  }, [index]);

  if (!open) return null;

  const close = () => setOpen(false);

  const execute = (command: AppCommand, unavailable: string | null) => {
    if (unavailable) return; // visible but inert; the row explains why
    if (command.runWith) {
      setArgFor(command);
      setQuery("");
      setArgError(null);
      setTimeout(() => inputRef.current?.focus(), 0);
      return;
    }
    close();
    command.run();
  };

  const submitArg = () => {
    if (!argFor?.runWith) return;
    const error = argFor.runWith(query);
    if (error) {
      setArgError(error);
      return;
    }
    close();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      if (argFor && !argCommandId) {
        setArgFor(null);
        setQuery("");
        setArgError(null);
      } else {
        close();
      }
      return;
    }
    if (argFor) {
      if (e.key === "Enter") {
        e.preventDefault();
        submitArg();
      }
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setIndex((i) => Math.min(i + 1, results.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const picked = results[index];
      if (picked) execute(picked.command, picked.unavailable);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/30 pt-[12vh]"
      onMouseDown={close}
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
    >
      <div
        className="w-[560px] max-w-[92vw] overflow-hidden rounded-xl border border-zinc-200 bg-white shadow-2xl dark:border-zinc-700 dark:bg-zinc-900"
        onMouseDown={(e) => e.stopPropagation()}
      >
        {argFor && (
          <div className="flex items-center gap-2 border-b border-zinc-100 px-3 pt-2.5 text-xs text-zinc-500 dark:border-zinc-800 dark:text-zinc-400">
            <span className="rounded bg-violet-100 px-1.5 py-0.5 font-medium text-violet-700 dark:bg-violet-500/15 dark:text-violet-300">
              {argFor.title.replace(/…$/, "")}
            </span>
            <span>Esc to go back</span>
          </div>
        )}
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={argFor ? (argFor.argPlaceholder ?? "Value") : "Type a command…"}
          aria-label={argFor ? argFor.argPlaceholder : "Search commands"}
          className="w-full border-b border-zinc-200 bg-transparent px-4 py-3 text-sm outline-none placeholder:text-zinc-400 dark:border-zinc-700"
        />
        {argFor ? (
          <div className="px-4 py-3 text-xs">
            {argError ? (
              <span className="text-red-600 dark:text-red-400">{argError}</span>
            ) : (
              <span className="text-zinc-400">Press Enter to run</span>
            )}
          </div>
        ) : (
          <div ref={listRef} className="max-h-[46vh] overflow-y-auto py-1" role="listbox">
            {results.length === 0 && (
              <p className="px-4 py-3 text-sm text-zinc-400">No matching commands</p>
            )}
            {results.map(({ command, unavailable }, i) => {
              const binding = bindings.get(command.id);
              return (
                <button
                  key={command.id}
                  data-index={i}
                  role="option"
                  aria-selected={i === index}
                  aria-disabled={!!unavailable}
                  onMouseEnter={() => setIndex(i)}
                  onClick={() => execute(command, unavailable)}
                  className={`flex w-full items-center gap-2 px-4 py-1.5 text-left text-sm ${
                    i === index ? "bg-violet-50 dark:bg-violet-500/10" : ""
                  } ${unavailable ? "cursor-default" : ""}`}
                >
                  <span
                    className={
                      unavailable
                        ? "text-zinc-400 dark:text-zinc-600"
                        : "text-zinc-800 dark:text-zinc-100"
                    }
                  >
                    {command.title}
                  </span>
                  {unavailable && (
                    <span className="truncate text-xs text-zinc-400 dark:text-zinc-600">
                      — {unavailable}
                    </span>
                  )}
                  <span className="flex-1" />
                  <span className="text-[10px] uppercase tracking-wide text-zinc-300 dark:text-zinc-600">
                    {command.category}
                  </span>
                  {binding && (
                    <kbd className="rounded border border-zinc-200 px-1.5 py-0.5 font-mono text-[10px] text-zinc-500 dark:border-zinc-700 dark:text-zinc-400">
                      {bindingLabel(binding)}
                    </kbd>
                  )}
                </button>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
