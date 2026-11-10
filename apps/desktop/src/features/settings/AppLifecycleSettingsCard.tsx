import { useEffect, useState } from "react";
import {
  currentAppVersion,
  hasDockVisibilitySetting,
  hasNativeAppLifecycle,
  readAutostart,
  readDesktopSettings,
  writeAutostart,
  writeDockIconVisibility,
  writeSilentStart,
} from "../../shared/native/appLifecycle";
import { updateStore, useUpdateStore } from "../../shared/store/updateStore";
import { Button } from "../../shared/ui/Button";
import { Switch } from "../../shared/ui/Switch";
import { TitledCard } from "../../shared/ui/TitledCard";
import { useMessage } from "../../shared/ui/message";
import styles from "./AppLifecycleSettingsCard.module.scss";

export function AppLifecycleSettingsCard() {
  const message = useMessage();
  const native = hasNativeAppLifecycle();
  const dockVisibilitySetting = hasDockVisibilitySetting();
  const { availableVersion, checking, installing } = useUpdateStore();
  const [version, setVersion] = useState("…");
  const [autostart, setAutostart] = useState(false);
  const [loadingAutostart, setLoadingAutostart] = useState(native);
  const [silentStart, setSilentStart] = useState(false);
  const [dockIconVisible, setDockIconVisible] = useState(true);
  const [loadingDesktopSettings, setLoadingDesktopSettings] = useState(native);

  useEffect(() => {
    let disposed = false;
    void currentAppVersion().then((next) => { if (!disposed) setVersion(next); });
    if (native) {
      void readAutostart()
        .then((enabled) => { if (!disposed) setAutostart(enabled); })
        .catch((cause) => message(cause instanceof Error ? cause.message : String(cause)))
        .finally(() => { if (!disposed) setLoadingAutostart(false); });
      void readDesktopSettings()
        .then((settings) => {
          if (disposed) return;
          setSilentStart(settings.silent_start);
          setDockIconVisible(settings.show_dock_icon);
        })
        .catch(() => {})
        .finally(() => {
          if (disposed) return;
          setLoadingDesktopSettings(false);
        });
    }
    return () => { disposed = true; };
  }, [message, native]);

  const toggleAutostart = async (enabled: boolean) => {
    try {
      setLoadingAutostart(true);
      await writeAutostart(enabled);
      setAutostart(await readAutostart());
      message(enabled ? t("Launch at login enabled") : t("Launch at login disabled"));
    } catch (cause) {
      message(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoadingAutostart(false);
    }
  };

  const toggleSilentStart = async (enabled: boolean) => {
    try {
      setLoadingDesktopSettings(true);
      await writeSilentStart(enabled);
      setSilentStart(enabled);
      message(enabled ? t("Silent start enabled") : t("Silent start disabled"));
    } catch (cause) {
      message(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoadingDesktopSettings(false);
    }
  };

  const toggleDockIcon = async (visible: boolean) => {
    try {
      setLoadingDesktopSettings(true);
      await writeDockIconVisibility(visible);
      setDockIconVisible(visible);
      message(visible ? t("Dock icon shown") : t("Dock icon hidden"));
    } catch (cause) {
      message(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoadingDesktopSettings(false);
    }
  };

  const checkUpdate = async () => {
    try {
      const nextVersion = await updateStore.check();
      message(nextVersion ? t("Version {version} is available", { version: nextVersion }) : t("You're up to date"));
    } catch (cause) {
      const error = cause instanceof Error ? cause.message : String(cause);
      message(t("Failed to check for updates: {error}", { error }));
    }
  };

  const updateNow = async () => {
    try {
      await updateStore.install();
    } catch (cause) {
      const error = cause instanceof Error ? cause.message : String(cause);
      message(t("Failed to install update: {error}", { error }));
    }
  };

  return <TitledCard title={t("Application")}>
    <div className={styles.row}>
      <div>
        <strong>{t("Launch at login")}</strong>
        <small>{t("Start Cursor BYOK automatically after signing in.")}</small>
      </div>
      <Switch
        checked={autostart}
        disabled={!native || loadingAutostart}
        label={t("Launch at login")}
        onChange={(enabled) => void toggleAutostart(enabled)}
      />
    </div>
    {autostart && <div className={styles.row}>
      <div>
        <strong>{t("Silent start")}</strong>
        <small>{t("Hide the main window on startup and keep only the tray icon.")}</small>
      </div>
      <Switch
        checked={silentStart}
        disabled={!native || loadingDesktopSettings}
        label={t("Silent start")}
        onChange={(enabled) => void toggleSilentStart(enabled)}
      />
    </div>}
    {dockVisibilitySetting && <div className={styles.row}>
      <div>
        <strong>{t("Show in Dock")}</strong>
        <small>{t("Hide the Dock icon when disabled. You can still open the app from the menu bar icon.")}</small>
      </div>
      <Switch
        checked={dockIconVisible}
        disabled={loadingDesktopSettings}
        label={t("Show in Dock")}
        onChange={(visible) => void toggleDockIcon(visible)}
      />
    </div>}
    <div className={styles.row}>
      <div>
        <strong>{t("Software updates")}</strong>
        <small>{availableVersion
          ? t("Version {version} is ready to install", { version: availableVersion })
          : t("Current version {version}", { version })}</small>
      </div>
      {availableVersion
        ? <Button size="small" variant="primary" disabled={installing} onClick={() => void updateNow()}>
            {installing ? t("Installing…") : t("Download and install")}
            <span className={styles.updateDot} aria-hidden="true" />
          </Button>
        : <Button size="small" disabled={!native || checking} onClick={() => void checkUpdate()}>
            {checking ? t("Checking…") : t("Check for updates")}
          </Button>}
    </div>
  </TitledCard>;
}
