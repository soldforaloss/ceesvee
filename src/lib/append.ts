// Pure helpers for multi-file append (F20).

export const DELIMITED_EXTENSIONS = ["csv", "tsv", "tab", "txt", "psv", "dat"];

/** Whether a filename looks like a delimited text file we can append. */
export function isDelimitedFile(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return name.includes(".") && DELIMITED_EXTENSIONS.includes(ext);
}

/**
 * Expand a directory listing into sorted full paths of delimited files,
 * using the same separator style as the picked directory.
 */
export function delimitedFilesInDir(
  dir: string,
  entries: { name: string; isFile: boolean }[],
): string[] {
  const sep = dir.includes("\\") ? "\\" : "/";
  const base = dir.endsWith(sep) ? dir.slice(0, -1) : dir;
  return entries
    .filter((e) => e.isFile && isDelimitedFile(e.name))
    .map((e) => `${base}${sep}${e.name}`)
    .sort();
}
