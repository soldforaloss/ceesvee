// Pure helpers for file profiles (F08): path matching and profile assembly.

import type { DocumentMeta, FileProfile, ProfileMatch } from "../types";

/** Normalize a path for comparison: forward slashes, lower-case (Windows). */
function normalize(path: string): string {
  return path.replace(/\\/g, "/").toLowerCase();
}

function extensionOf(path: string): string {
  const base = normalize(path).split("/").pop() ?? "";
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1) : "";
}

/** Convert a glob (`*`, `?`, `**`) into an anchored RegExp. */
export function globToRegExp(pattern: string): RegExp {
  let out = "^";
  const p = normalize(pattern);
  for (let i = 0; i < p.length; i++) {
    const c = p[i];
    if (c === "*") {
      if (p[i + 1] === "*") {
        out += ".*";
        i++;
      } else {
        out += "[^/]*";
      }
    } else if (c === "?") {
      out += "[^/]";
    } else if ("\\^$.|+()[]{}".includes(c)) {
      out += `\\${c}`;
    } else {
      out += c;
    }
  }
  return new RegExp(out + "$");
}

/** Whether one profile applies to a file path. */
export function profileMatches(matcher: ProfileMatch, path: string): boolean {
  const p = normalize(path);
  switch (matcher.type) {
    case "exactPath":
      return p === normalize(matcher.path);
    case "directory": {
      const dir = normalize(matcher.directory).replace(/\/+$/, "");
      return p.startsWith(dir + "/");
    }
    case "extension":
      return extensionOf(path) === matcher.extension.replace(/^\./, "").toLowerCase();
    case "glob": {
      const re = safeGlob(matcher.pattern);
      if (!re) return false;
      // Patterns without a slash match the file name; with one, the full path.
      const target =
        matcher.pattern.includes("/") || matcher.pattern.includes("\\")
          ? p
          : (p.split("/").pop() ?? "");
      return re.test(target);
    }
  }
}

function safeGlob(pattern: string): RegExp | null {
  try {
    return globToRegExp(pattern);
  } catch {
    return null;
  }
}

/** All profiles matching a path, in their stored order. */
export function matchingProfiles(profiles: FileProfile[], path: string): FileProfile[] {
  return profiles.filter((p) => profileMatches(p.matcher, path));
}

/** Whether a profile's parse settings differ from a document's current ones. */
export function profileSettingsDiffer(profile: FileProfile, meta: DocumentMeta): boolean {
  if (profile.delimiter !== null && profile.delimiter !== meta.delimiter) return true;
  if (profile.encoding !== null && profile.encoding !== meta.encoding) return true;
  if (profile.hasHeaderRow !== null && profile.hasHeaderRow !== meta.hasHeaderRow) return true;
  return false;
}

/** A fresh profile capturing a document's current shape and settings. */
export function profileFromDocument(name: string, meta: DocumentMeta): FileProfile {
  return {
    id: `profile-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`,
    name,
    matcher: meta.path
      ? { type: "exactPath", path: meta.path }
      : { type: "extension", extension: "csv" },
    autoApply: false,
    delimiter: meta.delimiter,
    encoding: meta.encoding,
    hasHeaderRow: meta.hasHeaderRow,
    defaultExport: null,
    expectedColumns: meta.hasHeaderRow ? [...meta.headers] : [],
    enforceOrder: meta.hasHeaderRow,
    expectedTypes: [],
    requiredColumns: [],
    uniqueColumns: [],
    regexRules: [],
    rangeRules: [],
    semanticTypes: [],
    crossRules: [],
    namedViews: [],
    lastViewId: null,
  };
}
