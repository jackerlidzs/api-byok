import { createContext, useContext, type ReactNode } from "react";
import { useAppStore } from "../../shared/store/appStore";
import controls from "../../shared/ui/Controls.module.scss";
import styles from "./CursorSettings.module.scss";

const CaReady = createContext(false);
const ModelsReady = createContext(false);

export function CursorCaProvider({ children }: { children: ReactNode }) {
  const { cursorHarness } = useAppStore();
  return <CaReady.Provider value={cursorHarness?.ca === "ready"}>{children}</CaReady.Provider>;
}

export function CursorCaGate({ busy, waitingForRefresh, onInitialize, onRefresh, children }: { busy: boolean; waitingForRefresh: boolean; onInitialize: () => void; onRefresh: () => void; children: ReactNode }) {
  const ready = useContext(CaReady);
  const { cursorHarness } = useAppStore();
  if (ready) return children;
  const installedLocally = cursorHarness?.ca === "untrusted";
  return <div className={styles.gate}>
    <strong>{installedLocally ? t("The local CA must be trusted by the system") : t("Initialize the local CA first")}</strong>
    <span>{installedLocally ? t("Paste the authorization command into the terminal and enter your password, then click the button below") : t("The CA is stored only on this device and is used to securely inspect Cursor HTTPS requests.")}</span>
    <button className={controls.primary} disabled={busy} onClick={waitingForRefresh ? onRefresh : onInitialize}>{busy ? t("Refreshing…") : waitingForRefresh ? t("I've initialized it — refresh") : installedLocally ? t("Open terminal to install CA") : t("Initialize CA")}</button>
  </div>;
}

export function CursorModelProvider({ children }: { children: ReactNode }) {
  const { models, plugins } = useAppStore();
  const hasConfiguredPlugin = plugins.some((plugin) => plugin.providers.some((provider) => provider.configured));
  return <ModelsReady.Provider value={models.length > 0 || hasConfiguredPlugin}>{children}</ModelsReady.Provider>;
}

export function CursorModelGate({ busy, previewingImport, onAdd, onImport, children }: { busy: boolean; previewingImport: boolean; onAdd: () => void; onImport: () => void; children: ReactNode }) {
  const ready = useContext(ModelsReady);
  if (ready) return children;
  return <div className={styles.gate}>
    <strong>{t("No models are available to Cursor yet")}</strong>
    <span>{t("Cursor integration is active. Add a model configuration to use a BYOK model.")}</span>
    <div className={styles.gateActions}>
      <button className={controls.primary} disabled={busy} onClick={onAdd}>{t("Add model")}</button>
      <button className={controls.secondary} disabled={busy} onClick={onImport}>{previewingImport ? t("Reading…") : t("Import legacy configuration")}</button>
    </div>
  </div>;
}
