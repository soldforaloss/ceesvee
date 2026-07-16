import { useState } from "react";

import { profileFromDocument, profileMatches } from "../lib/profiles";
import { useActiveMeta, useStore } from "../store/useStore";
import type { FileProfile, ProfileMatch } from "../types";
import { Modal } from "./Modal";

/**
 * Manage reusable file profiles (F08): create one from the current document,
 * choose how it matches files, toggle auto-apply, validate the active
 * document against it, or delete it. Profiles hold configuration only —
 * never document data.
 */
export function ProfilesDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const settings = useStore((s) => s.settings);
  const saveProfiles = useStore((s) => s.saveProfiles);
  const applyProfile = useStore((s) => s.applyProfile);
  const validation = useStore((s) => s.profileValidation);
  const runValidation = useStore((s) => s.runProfileValidation);
  const clearValidation = useStore((s) => s.clearProfileValidation);

  const [newName, setNewName] = useState("");
  const profiles = settings?.profiles ?? [];

  const createFromCurrent = () => {
    if (!meta || !newName.trim()) return;
    const profile = profileFromDocument(newName.trim(), meta);
    void saveProfiles([...profiles, profile]);
    setNewName("");
  };

  const update = (id: string, patch: Partial<FileProfile>) => {
    void saveProfiles(profiles.map((p) => (p.id === id ? { ...p, ...patch } : p)));
  };

  const remove = (id: string) => {
    clearValidation();
    void saveProfiles(profiles.filter((p) => p.id !== id));
  };

  return (
    <Modal title="File profiles" onClose={onClose} size="xl">
      <div className="space-y-3 text-sm">
        {meta && (
          <div className="flex items-center gap-2 rounded border border-dashed border-zinc-300 px-3 py-2 dark:border-zinc-700">
            <input
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && createFromCurrent()}
              placeholder="New profile name…"
              className="flex-1 rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700"
            />
            <button
              onClick={createFromCurrent}
              disabled={!newName.trim()}
              className="rounded bg-violet-600 px-2.5 py-1 text-xs font-medium text-white hover:bg-violet-500 disabled:opacity-40"
            >
              Capture current document
            </button>
          </div>
        )}

        {profiles.length === 0 ? (
          <p className="py-6 text-center text-xs text-zinc-400">
            No profiles yet. Capture the current document's delimiter, encoding, header mode and
            columns to reuse them for recurring files.
          </p>
        ) : (
          <div className="max-h-[55vh] space-y-2 overflow-y-auto pr-1">
            {profiles.map((p) => (
              <ProfileCard
                key={p.id}
                profile={p}
                matchesActive={!!meta?.path && profileMatches(p.matcher, meta.path)}
                onUpdate={(patch) => update(p.id, patch)}
                onDelete={() => remove(p.id)}
                onApply={() => applyProfile(p)}
                onValidate={() => void runValidation(p)}
                validation={validation?.profileId === p.id ? validation : null}
                hasDoc={!!meta}
              />
            ))}
          </div>
        )}
      </div>
    </Modal>
  );
}

function ProfileCard({
  profile,
  matchesActive,
  onUpdate,
  onDelete,
  onApply,
  onValidate,
  validation,
  hasDoc,
}: {
  profile: FileProfile;
  matchesActive: boolean;
  onUpdate: (patch: Partial<FileProfile>) => void;
  onDelete: () => void;
  onApply: () => void;
  onValidate: () => void;
  validation: import("../types").ProfileValidation | null;
  hasDoc: boolean;
}) {
  const m = profile.matcher;
  const matchValue = matcherValue(m);

  return (
    <div className="rounded-lg border border-zinc-200 px-3 py-2 dark:border-zinc-800">
      <div className="flex items-center gap-2">
        <span className="font-medium text-zinc-800 dark:text-zinc-100">{profile.name}</span>
        {matchesActive && (
          <span className="rounded bg-violet-100 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-violet-700 dark:bg-violet-500/15 dark:text-violet-300">
            matches current
          </span>
        )}
        <div className="flex-1" />
        {hasDoc && (
          <>
            <button onClick={onValidate} className={btnSmall}>
              Validate
            </button>
            <button onClick={onApply} className={btnSmall}>
              Apply…
            </button>
          </>
        )}
        <button
          onClick={onDelete}
          className="rounded px-2 py-0.5 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
        >
          Delete
        </button>
      </div>

      <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-zinc-600 dark:text-zinc-300">
        <label className="flex items-center gap-1">
          Match
          <select
            value={m.type}
            onChange={(e) =>
              onUpdate({
                matcher: rebuildMatcher(e.target.value as ProfileMatch["type"], matchValue),
              })
            }
            className={selectSmall}
          >
            <option value="exactPath" className="dark:bg-zinc-800">
              exact path
            </option>
            <option value="directory" className="dark:bg-zinc-800">
              directory
            </option>
            <option value="extension" className="dark:bg-zinc-800">
              extension
            </option>
            <option value="glob" className="dark:bg-zinc-800">
              glob
            </option>
          </select>
        </label>
        <input
          value={matchValue}
          onChange={(e) => onUpdate({ matcher: rebuildMatcher(m.type, e.target.value) })}
          className="min-w-64 flex-1 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 font-mono text-[11px] outline-none focus:border-violet-500 dark:border-zinc-700"
        />
        <label
          className="flex cursor-pointer items-center gap-1.5 select-none"
          title="Reparse matching files with this profile automatically (clean documents only)"
        >
          <input
            type="checkbox"
            checked={profile.autoApply}
            onChange={(e) => onUpdate({ autoApply: e.target.checked })}
            className="accent-violet-600"
          />
          Auto-apply
        </label>
      </div>

      <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-0.5 text-[11px] text-zinc-400 dark:text-zinc-500">
        {profile.delimiter !== null && <span>delimiter “{profile.delimiter}”</span>}
        {profile.encoding !== null && <span>{profile.encoding}</span>}
        {profile.hasHeaderRow !== null && (
          <span>{profile.hasHeaderRow ? "header row" : "no header row"}</span>
        )}
        {profile.expectedColumns.length > 0 && (
          <span>
            {profile.expectedColumns.length} expected column
            {profile.expectedColumns.length === 1 ? "" : "s"}
            {profile.enforceOrder ? " (ordered)" : ""}
          </span>
        )}
      </div>

      {validation && (
        <div
          className={`mt-2 rounded px-2 py-1.5 text-xs ${
            validation.ok
              ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-500/10 dark:text-emerald-300"
              : "bg-amber-50 text-amber-700 dark:bg-amber-500/10 dark:text-amber-300"
          }`}
        >
          {validation.ok ? (
            "The current document satisfies this profile."
          ) : (
            <ul className="list-inside list-disc space-y-0.5">
              {validation.issues.map((issue, i) => (
                <li key={i}>{issue.detail}</li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

function matcherValue(m: ProfileMatch): string {
  switch (m.type) {
    case "exactPath":
      return m.path;
    case "directory":
      return m.directory;
    case "extension":
      return m.extension;
    case "glob":
      return m.pattern;
  }
}

function rebuildMatcher(type: ProfileMatch["type"], value: string): ProfileMatch {
  switch (type) {
    case "exactPath":
      return { type, path: value };
    case "directory":
      return { type, directory: value };
    case "extension":
      return { type, extension: value };
    case "glob":
      return { type, pattern: value };
  }
}

const btnSmall =
  "rounded border border-zinc-300 px-2 py-0.5 text-xs text-zinc-600 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectSmall =
  "rounded border border-zinc-300 bg-transparent px-1 py-0.5 text-xs outline-none focus:border-violet-500 dark:border-zinc-700";
