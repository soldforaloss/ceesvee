// Auto-update. On launch we ask GitHub — via the signed `latest.json` manifest
// referenced by `plugins.updater.endpoints` in tauri.conf.json — whether a
// newer release exists. If so we prompt the user, then download, install and
// relaunch. This is a no-op under `tauri dev` (no updater available), and any
// network/availability error is swallowed so a failed check never blocks
// startup or interrupts the user.
import { getVersion } from "@tauri-apps/api/app";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";

export async function checkForUpdates({ silent = true }: { silent?: boolean } = {}): Promise<void> {
  try {
    const update = await check();
    if (!update) {
      // When the user explicitly checks, confirm there's nothing to do rather
      // than appearing to do nothing.
      if (!silent) {
        const version = await getVersion();
        await message(`You're on the latest version (${version}).`, {
          title: "No updates available",
          kind: "info",
        });
      }
      return;
    }

    const accepted = await ask(
      `CEESVEE ${update.version} is available — you have ${update.currentVersion}.\n\n` +
        `Download and install it now? The app will restart when it's done.`,
      {
        title: "Update available",
        kind: "info",
        okLabel: "Update now",
        cancelLabel: "Later",
      },
    );
    if (!accepted) return;

    await update.downloadAndInstall();
    await relaunch();
  } catch (err) {
    // A failed check must never block using the app; surface it only when the
    // user explicitly asked to check.
    console.error("Update check failed:", err);
    if (!silent) {
      await message(`Couldn't check for updates.\n\n${String(err)}`, {
        title: "Update check failed",
        kind: "error",
      });
    }
  }
}
