import { useMemo, useState } from "react";

import { registry, type AppCommand, type CommandCategory } from "../lib/commands";
import { bindingFromEvent, bindingLabel, effectiveBindings, findConflict } from "../lib/shortcuts";
import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

const CATEGORY_ORDER: CommandCategory[] = [
  "File",
  "Edit",
  "Data",
  "View",
  "Navigate",
  "Tabs",
  "Export",
  "Help",
];

/**
 * The shortcut editor (F11). Click a binding to record a new chord; conflicts
 * with another command must be explicitly confirmed (which unbinds the other
 * command) — they can never be saved silently. Changes persist to the
 * versioned settings file and apply immediately.
 */
export function ShortcutsDialog({ onClose }: { onClose: () => void }) {
  const overrides = useStore((s) => s.settings?.shortcutOverrides);
  const setShortcutOverride = useStore((s) => s.setShortcutOverride);

  const [recordingFor, setRecordingFor] = useState<string | null>(null);
  const [conflict, setConflict] = useState<{
    commandId: string;
    binding: string;
    holder: AppCommand;
  } | null>(null);

  const commands = useMemo(() => registry.staticCommands(), []);
  const defaults = useMemo(() => registry.defaultBindings(), []);
  const bindings = useMemo(() => effectiveBindings(defaults, overrides), [defaults, overrides]);

  const grouped = useMemo(() => {
    const byCategory = new Map<CommandCategory, AppCommand[]>();
    for (const command of commands) {
      const list = byCategory.get(command.category) ?? [];
      list.push(command);
      byCategory.set(command.category, list);
    }
    return CATEGORY_ORDER.filter((c) => byCategory.has(c)).map(
      (c) => [c, byCategory.get(c)!] as const,
    );
  }, [commands]);

  const capture = (commandId: string) => (e: React.KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      setRecordingFor(null);
      return;
    }
    const binding = bindingFromEvent(e.nativeEvent);
    if (!binding) return; // modifier-only or unbindable key: keep recording
    const holderId = findConflict(bindings, binding, commandId);
    if (holderId) {
      const holder = registry.byId(holderId);
      if (holder) {
        // Never save a duplicate silently: park it behind a confirmation.
        setConflict({ commandId, binding, holder });
        setRecordingFor(null);
        return;
      }
    }
    void setShortcutOverride(commandId, binding);
    setRecordingFor(null);
  };

  const confirmConflict = async () => {
    if (!conflict) return;
    // Unbind the current holder, then bind the new command.
    await setShortcutOverride(conflict.holder.id, null);
    await setShortcutOverride(conflict.commandId, conflict.binding);
    setConflict(null);
  };

  return (
    <Modal title="Keyboard shortcuts" onClose={onClose} size="lg">
      <div className="max-h-[60vh] space-y-4 overflow-y-auto pr-1 text-sm">
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          Click a shortcut to record a new one (Esc cancels recording). Changes apply immediately
          and are stored in your settings.
        </p>
        {grouped.map(([category, list]) => (
          <section key={category}>
            <h3 className="mb-1 text-[11px] font-semibold uppercase tracking-wider text-zinc-400 dark:text-zinc-500">
              {category}
            </h3>
            <ul className="divide-y divide-zinc-100 rounded border border-zinc-200 dark:divide-zinc-800 dark:border-zinc-800">
              {list.map((command) => {
                const bound = bindings.get(command.id);
                const isDefault = defaults.get(command.id) === bound;
                const overridden = overrides?.[command.id] !== undefined;
                return (
                  <li key={command.id} className="flex items-center gap-2 px-3 py-1.5">
                    <span className="flex-1 truncate">{command.title}</span>
                    {recordingFor === command.id ? (
                      <input
                        autoFocus
                        readOnly
                        value="Press keys…"
                        onKeyDown={capture(command.id)}
                        onBlur={() => setRecordingFor(null)}
                        className="w-36 rounded border border-violet-400 bg-violet-50 px-2 py-0.5 text-center text-xs outline-none dark:bg-violet-500/10"
                      />
                    ) : (
                      <button
                        onClick={() => setRecordingFor(command.id)}
                        title="Click to record a new shortcut"
                        className="min-w-24 rounded border border-zinc-200 px-2 py-0.5 font-mono text-xs text-zinc-600 hover:border-violet-400 dark:border-zinc-700 dark:text-zinc-300"
                      >
                        {bound ? bindingLabel(bound) : "—"}
                      </button>
                    )}
                    {bound && (
                      <button
                        title="Remove shortcut"
                        onClick={() => void setShortcutOverride(command.id, null)}
                        className="rounded px-1.5 py-0.5 text-xs text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600 dark:hover:bg-zinc-800"
                      >
                        Clear
                      </button>
                    )}
                    {overridden && !isDefault && (
                      <button
                        title="Reset to default"
                        onClick={() => void setShortcutOverride(command.id, undefined)}
                        className="rounded px-1.5 py-0.5 text-xs text-violet-600 hover:bg-violet-50 dark:text-violet-300 dark:hover:bg-violet-500/10"
                      >
                        Reset
                      </button>
                    )}
                  </li>
                );
              })}
            </ul>
          </section>
        ))}
      </div>

      {conflict && (
        <div className="mt-3 rounded border border-amber-300 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-300">
          <p>
            <span className="font-mono">{bindingLabel(conflict.binding)}</span> is already bound to{" "}
            <strong>{conflict.holder.title}</strong>. Reassign it?
          </p>
          <div className="mt-2 flex justify-end gap-2">
            <button
              onClick={() => setConflict(null)}
              className="rounded px-2 py-1 text-xs text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
            >
              Cancel
            </button>
            <button
              onClick={() => void confirmConflict()}
              className="rounded bg-amber-600 px-2 py-1 text-xs text-white hover:bg-amber-500"
            >
              Reassign to “{registry.byId(conflict.commandId)?.title}”
            </button>
          </div>
        </div>
      )}
    </Modal>
  );
}
