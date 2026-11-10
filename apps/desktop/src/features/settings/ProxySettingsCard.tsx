import { useState } from "react";
import { api, type ProxySettings, type ProxySettingsInput, type ProxyTestResult } from "../../shared/api";
import { Button } from "../../shared/ui/Button";
import { Checkbox } from "../../shared/ui/Checkbox";
import { TextInput } from "../../shared/ui/FormControls";
import { Select } from "../../shared/ui/Select";
import { TitledCard } from "../../shared/ui/TitledCard";
import styles from "./ProxySettingsCard.module.scss";

/** A test of the draft identified by `id`; editing the draft hides an older result. */
type ProxyTest =
  | { id: string; running: true }
  | { id: string; running: false; result: ProxyTestResult }
  | { id: string; running: false; error: string };

function ProxyTestLines({ test }: { test: ProxyTest }) {
  if (test.running) return <small className={styles.note}>{t("Testing…")}</small>;
  if ("error" in test) return <small className={styles.testError}>{t("Test failed: {error}", { error: test.error })}</small>;
  const { result } = test;
  return <div className={styles.testResult}>
    {result.egress_ip
      ? <small className={styles.testOk}>{t("✓ Connected, egress IP: {ip} ({duration} ms)", { ip: result.egress_ip, duration: result.duration_ms })}</small>
      : <small className={styles.testError}>{t("✗ Cannot reach the internet through this connection: {error}", { error: result.egress_error ?? "" })}</small>}
    {result.address && <small className={styles.note}>{result.auth_enabled
      ? t("Recognized as {address}, username {username}", { address: result.address, username: result.username })
      : t("Recognized as {address}", { address: result.address })}</small>}
    {result.providers.map((provider) => provider.reachable
      ? <small key={provider.host} className={styles.testOk}>{t("✓ {host}: responded HTTP {status} ({duration} ms)", { host: provider.host, status: provider.http_status ?? "", duration: provider.duration_ms })}</small>
      : <small key={provider.host} className={styles.testError}>{t("✗ {host} unreachable: {error}", { host: provider.host, error: provider.error ?? "" })}</small>)}
  </div>;
}

export function ProxySettingsCard({
  settings,
  draft,
  editing,
  saving,
  onDraftChange,
  onEdit,
  onCancel,
  onSave,
}: {
  settings: ProxySettings | null;
  draft: ProxySettingsInput;
  editing: boolean;
  saving: boolean;
  onDraftChange: (draft: ProxySettingsInput) => void;
  onEdit: () => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  const [test, setTest] = useState<ProxyTest | null>(null);
  const custom = draft.mode === "custom";
  const draftId = JSON.stringify(draft);
  const shownTest = test?.id === draftId ? test : null;
  const testing = shownTest?.running ?? false;
  const canTest = Boolean(settings) && !saving && !testing && (!custom || draft.address.trim() !== "");
  const runTest = async () => {
    const id = draftId;
    setTest({ id, running: true });
    try {
      const result = await api.testProxySettings({ ...draft, password: draft.password || undefined });
      setTest({ id, running: false, result });
    } catch (cause) {
      setTest({ id, running: false, error: cause instanceof Error ? cause.message : String(cause) });
    }
  };
  const modeLabel = (mode: ProxySettingsInput["mode"]) => mode === "default" ? t("Default") : t("Custom");
  const action = editing ? (
    <div className={styles.actionGroup}>
      <Button size="small" disabled={!canTest} onClick={() => void runTest()}>{t("Test")}</Button>
      <Button size="small" disabled={saving} onClick={onCancel}>{t("Cancel")}</Button>
      <Button variant="primary" size="small" disabled={saving} onClick={onSave}>{saving ? t("Saving…") : t("Save")}</Button>
    </div>
  ) : (
    <div className={styles.actionGroup}>
      <button type="button" className={styles.headerAction} disabled={!canTest} onClick={() => void runTest()}>
        {t("Test")}
      </button>
      <button type="button" className={styles.headerAction} disabled={!settings} onClick={onEdit}>
        {t("Edit")}
      </button>
    </div>
  );

  return <TitledCard title={t("Proxy settings")} action={action}>
    <div className={styles.content}>
      {editing ? <>
        <div className={styles.row}>
          <strong>{t("Proxy mode")}</strong>
          <div className={styles.control}><Select ariaLabel={t("Proxy mode")} value={draft.mode} options={[{ value: "default", label: t("Default") }, { value: "custom", label: t("Custom") }]} onChange={(mode) => onDraftChange({ ...draft, mode: mode as ProxySettingsInput["mode"] })} /></div>
        </div>
        {custom && <div className={styles.customFields}>
          <div className={styles.row}>
            <strong>{t("Proxy address")}</strong>
            <div className={styles.control}><TextInput value={draft.address} placeholder="http://127.0.0.1:7890" autoComplete="off" onChange={(event) => onDraftChange({ ...draft, address: event.target.value })} /></div>
          </div>
          <small className={styles.note}>{t("You can paste http://user:password@host:port, host:port:user:password or host:port user password; the username and password are recognized on save.")}</small>
          <div className={styles.row}>
            <strong>{t("Authentication")}</strong>
            <Checkbox checked={draft.auth_enabled} label={t("Proxy requires authentication")} onChange={(auth_enabled) => onDraftChange({ ...draft, auth_enabled })} />
          </div>
          {draft.auth_enabled && <div className={styles.customFields}>
            <div className={styles.row}>
              <strong>{t("Username")}</strong>
              <div className={styles.control}><TextInput value={draft.username} autoComplete="off" onChange={(event) => onDraftChange({ ...draft, username: event.target.value })} /></div>
            </div>
            <div className={styles.row}>
              <strong>{t("Password")}</strong>
              <div className={styles.control}><TextInput type="password" value={draft.password ?? ""} autoComplete="new-password" placeholder={settings?.has_password ? t("Leave blank to keep the current password") : ""} onChange={(event) => onDraftChange({ ...draft, password: event.target.value })} /></div>
            </div>
          </div>}
        </div>}
      </> : <>
        <div className={styles.row}><strong>{t("Proxy mode")}</strong><span className={styles.value}>{settings ? modeLabel(settings.mode) : t("Loading…")}</span></div>
        {settings?.mode === "custom" && <div className={styles.customFields}>
          <div className={styles.row}><strong>{t("Proxy address")}</strong><span className={styles.value}>{settings.address}</span></div>
          <div className={styles.row}><strong>{t("Authentication")}</strong><span className={styles.value}>{settings.auth_enabled ? t("Enabled") : t("Disabled")}</span></div>
          {settings.auth_enabled && <>
            <div className={styles.row}><strong>{t("Username")}</strong><span className={styles.value}>{settings.username || "—"}</span></div>
            <div className={styles.row}><strong>{t("Password")}</strong><span className={styles.value}>{settings.has_password ? t("Set") : t("Not set")}</span></div>
          </>}
        </div>}
      </>}
      {shownTest && <ProxyTestLines test={shownTest} />}
    </div>
  </TitledCard>;
}
