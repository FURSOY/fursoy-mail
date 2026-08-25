import { useCallback, useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import type { AppLocale } from "../i18n";
import { tauriApi } from "../tauriApi";
import { isNoUpdateError, STARTUP_UPDATE_DELAY_MS } from "../utils";

export interface AvailableUpdate {
  version: string;
  date: string;
  body: string;
}

export interface UpdateProgress {
  downloaded: number;
  total: number;
}

interface UseUpdaterOptions {
  locale: AppLocale;
  showToast: (message: string, type?: "error" | "success" | "info") => void;
  shouldDeferNetwork: (userInitiated?: boolean) => Promise<boolean>;
}

export function useUpdater({ locale, showToast, shouldDeferNetwork }: UseUpdaterOptions) {
  const [currentVersion, setCurrentVersion] = useState("");
  const [isCheckingUpdate, setIsCheckingUpdate] = useState(false);
  const [updateAvailable, setUpdateAvailable] = useState<AvailableUpdate | null>(null);
  const [updateProgress, setUpdateProgress] = useState<UpdateProgress | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [updateStatus, setUpdateStatus] = useState("");
  const notifiedVersionRef = useRef<string | null>(null);

  useEffect(() => {
    getVersion().then(setCurrentVersion).catch(console.error);
  }, []);

  const checkForUpdates = useCallback(async (showUIMessages = false) => {
    try {
      if (showUIMessages) setIsCheckingUpdate(true);
      setUpdateError(null);
      setUpdateStatus("");
      if (await shouldDeferNetwork(showUIMessages)) {
        console.log("System in fullscreen/game mode, skipping automatic update check.");
        return;
      }

      const update = await check();
      if (update) {
        setUpdateAvailable({ version: update.version, date: update.date || "", body: update.body || "" });
        setUpdateStatus(locale.update.available.replace("{version}", update.version));
        if (showUIMessages) {
          showToast(locale.update.available.replace("{version}", update.version), "info");
        } else if (notifiedVersionRef.current !== update.version) {
          notifiedVersionRef.current = update.version;
          await tauriApi.showCustomNotification({
            title: locale.update.readyTitle,
            body: locale.update.readyBody.replace("{version}", update.version),
            kind: "update",
            code: null,
            emailId: null,
            duration: 10_000,
            dismissAllLabel: locale.common.dismissAll,
          });
        }
      } else {
        setUpdateAvailable(null);
        setUpdateStatus(locale.update.upToDate);
        if (showUIMessages) showToast(locale.update.upToDate, "success");
      }
    } catch (error) {
      console.error("Update check failed:", error);
      if (isNoUpdateError(error)) {
        setUpdateAvailable(null);
        setUpdateError(null);
        setUpdateStatus(locale.update.upToDate);
        if (showUIMessages) showToast(locale.update.upToDate, "success");
        return;
      }
      const message = error instanceof Error ? error.message : String(error);
      setUpdateError(`${locale.update.checkFailed}: ${message}`);
      setUpdateStatus("");
      if (showUIMessages) showToast(locale.update.checkFailed, "error");
    } finally {
      if (showUIMessages) setIsCheckingUpdate(false);
    }
  }, [locale, shouldDeferNetwork, showToast]);

  const installUpdate = useCallback(async () => {
    try {
      setUpdateError(null);
      setUpdateStatus("");
      const update = await check();
      if (!update) {
        setUpdateAvailable(null);
        setUpdateStatus(locale.update.upToDate);
        return;
      }

      setUpdateProgress({ downloaded: 0, total: 100 });
      let downloaded = 0;
      let totalLength = 0;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            totalLength = event.data.contentLength || 0;
            setUpdateProgress({ downloaded: 0, total: totalLength });
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            setUpdateProgress({ downloaded, total: totalLength });
            break;
          case "Finished":
            setUpdateProgress(null);
            break;
        }
      });
      await relaunch();
    } catch (error) {
      console.error("Update install failed", error);
      if (isNoUpdateError(error)) {
        setUpdateAvailable(null);
        setUpdateError(null);
        setUpdateStatus(locale.update.upToDate);
        setUpdateProgress(null);
        return;
      }
      const message = error instanceof Error ? error.message : String(error);
      setUpdateError(`${locale.update.installFailed}: ${message}`);
      setUpdateProgress(null);
    }
  }, [locale]);

  const latestCheckRef = useRef(checkForUpdates);
  latestCheckRef.current = checkForUpdates;
  useEffect(() => {
    const timer = window.setTimeout(() => { void latestCheckRef.current(false); }, STARTUP_UPDATE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, []);

  return {
    currentVersion,
    isCheckingUpdate,
    updateAvailable,
    updateProgress,
    updateError,
    updateStatus,
    checkForUpdates,
    installUpdate,
  };
}
