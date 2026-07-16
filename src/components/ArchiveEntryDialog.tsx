import { formatBytes } from "../lib/save";
import { useStore } from "../store/useStore";
import { Modal } from "./Modal";

/**
 * ZIP entry chooser (F17): pick which archive member to open. Encrypted
 * entries are shown but cannot be opened; sizes, compression ratio, and the
 * sniffed delimiter/encoding help pick the right file.
 */
export function ArchiveEntryDialog() {
  const pick = useStore((s) => s.archivePick);
  const choose = useStore((s) => s.pickArchiveEntry);
  const dismiss = useStore((s) => s.dismissArchivePick);

  if (!pick) return null;
  const archiveName = pick.path.split(/[\\/]/).pop() ?? pick.path;

  return (
    <Modal title={`Open from ${archiveName}`} onClose={dismiss} size="lg">
      <div className="space-y-2 text-sm">
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          Choose the file to open. The archive itself is never modified — use Save As or Export to
          write changes elsewhere.
        </p>
        <div className="max-h-[50vh] overflow-y-auto rounded border border-zinc-200 dark:border-zinc-800">
          <table className="w-full border-collapse text-xs">
            <thead className="sticky top-0 bg-white text-left uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
              <tr>
                <th className="px-2 py-1.5 font-medium">Entry</th>
                <th className="px-2 py-1.5 text-right font-medium">Compressed</th>
                <th className="px-2 py-1.5 text-right font-medium">Uncompressed</th>
                <th className="px-2 py-1.5 text-right font-medium">Ratio</th>
                <th className="px-2 py-1.5 font-medium">Delimiter</th>
                <th className="px-2 py-1.5 font-medium">Encoding</th>
                <th className="px-2 py-1.5" />
              </tr>
            </thead>
            <tbody>
              {pick.entries.map((entry) => (
                <tr key={entry.name} className="border-t border-zinc-100 dark:border-zinc-800">
                  <td className="max-w-60 truncate px-2 py-1.5 font-mono" title={entry.name}>
                    {entry.name}
                  </td>
                  <td className="px-2 py-1.5 text-right tabular-nums">
                    {formatBytes(entry.compressedSize)}
                  </td>
                  <td className="px-2 py-1.5 text-right tabular-nums">
                    {formatBytes(entry.uncompressedSize)}
                  </td>
                  <td className="px-2 py-1.5 text-right tabular-nums">{entry.ratio.toFixed(1)}×</td>
                  <td className="px-2 py-1.5 font-mono">
                    {entry.likelyDelimiter === "\t" ? "\\t" : (entry.likelyDelimiter ?? "—")}
                  </td>
                  <td className="px-2 py-1.5">{entry.likelyEncoding ?? "—"}</td>
                  <td className="px-2 py-1.5 text-right">
                    {entry.encrypted ? (
                      <span className="text-zinc-400" title="Encrypted entries cannot be opened">
                        encrypted
                      </span>
                    ) : (
                      <button
                        onClick={() => void choose(entry.name)}
                        className="rounded bg-violet-600 px-2 py-0.5 text-white hover:bg-violet-500"
                      >
                        Open
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </Modal>
  );
}
