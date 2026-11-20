import { check } from "@tauri-apps/plugin-updater";
import { ask } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import { toast } from "sonner";
import i18n from "./i18next";

export async function checkForUpdates() {
  const update = await check();
  if (update) {
    if (
      !(await ask(
        i18n.t("update.new_version_available", { version: update.version })
      ))
    )
      return;

    let promise = update.downloadAndInstall((event) => {
      switch (event.event) {
        case "Started":
          console.log("Started download");
          break;
        case "Progress":
          break;
        case "Finished":
          console.log("Download finished");
          break;
      }
    });

    promise.then(async () => {
      await relaunch();
    });

    toast.promise(promise, {
      loading: i18n.t("update.updating"),
      success: i18n.t("update.downloaded_restarting"),
      error: (e) => i18n.t("update.failed_download", { error: String(e) }),
    });
  }
}
