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
  if (test.running) return <small className={styles.note}>{t("测试中…")}</small>;
  if ("error" in test) return <small className={styles.testError}>{t("测试失败：{error}", { error: test.error })}</small>;
  const { result } = test;
  return <div className={styles.testResult}>
    {result.egress_ip
      ? <small className={styles.testOk}>{t("✓ 连接正常，出口 IP：{ip}（{duration} ms）", { ip: result.egress_ip, duration: result.duration_ms })}</small>
      : <small className={styles.testError}>{t("✗ 无法通过此连接访问外网：{error}", { error: result.egress_error ?? "" })}</small>}
    {result.address && <small className={styles.note}>{result.auth_enabled
      ? t("识别为 {address}，用户名 {username}", { address: result.address, username: result.username })
      : t("识别为 {address}", { address: result.address })}</small>}
    {result.providers.map((provider) => provider.reachable
      ? <small key={provider.host} className={styles.testOk}>{t("✓ {host}：已响应 HTTP {status}（{duration} ms）", { host: provider.host, status: provider.http_status ?? "", duration: provider.duration_ms })}</small>
      : <small key={provider.host} className={styles.testError}>{t("✗ {host} 无法访问：{error}", { host: provider.host, error: provider.error ?? "" })}</small>)}
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
  const modeLabel = (mode: ProxySettingsInput["mode"]) => mode === "default" ? t("默认") : t("自定义");
  const action = editing ? (
    <div className={styles.actionGroup}>
      <Button size="small" disabled={!canTest} onClick={() => void runTest()}>{t("测试")}</Button>
      <Button size="small" disabled={saving} onClick={onCancel}>{t("取消")}</Button>
      <Button variant="primary" size="small" disabled={saving} onClick={onSave}>{saving ? t("保存中…") : t("保存")}</Button>
    </div>
  ) : (
    <div className={styles.actionGroup}>
      <button type="button" className={styles.headerAction} disabled={!canTest} onClick={() => void runTest()}>
        {t("测试")}
      </button>
      <button type="button" className={styles.headerAction} disabled={!settings} onClick={onEdit}>
        {t("编辑")}
      </button>
    </div>
  );

  return <TitledCard title={t("代理设置")} action={action}>
    <div className={styles.content}>
      {editing ? <>
        <div className={styles.row}>
          <strong>{t("代理方式")}</strong>
          <div className={styles.control}><Select ariaLabel={t("代理方式")} value={draft.mode} options={[{ value: "default", label: t("默认") }, { value: "custom", label: t("自定义") }]} onChange={(mode) => onDraftChange({ ...draft, mode: mode as ProxySettingsInput["mode"] })} /></div>
        </div>
        {custom && <div className={styles.customFields}>
          <div className={styles.row}>
            <strong>{t("代理地址")}</strong>
            <div className={styles.control}><TextInput value={draft.address} placeholder="http://127.0.0.1:7890" autoComplete="off" onChange={(event) => onDraftChange({ ...draft, address: event.target.value })} /></div>
          </div>
          <small className={styles.note}>{t("可直接粘贴 http://用户名:密码@主机:端口、主机:端口:用户名:密码 或 主机:端口 用户名 密码；保存时会自动识别用户名和密码。")}</small>
          <div className={styles.row}>
            <strong>{t("认证")}</strong>
            <Checkbox checked={draft.auth_enabled} label={t("代理需要认证")} onChange={(auth_enabled) => onDraftChange({ ...draft, auth_enabled })} />
          </div>
          {draft.auth_enabled && <div className={styles.customFields}>
            <div className={styles.row}>
              <strong>{t("用户名")}</strong>
              <div className={styles.control}><TextInput value={draft.username} autoComplete="off" onChange={(event) => onDraftChange({ ...draft, username: event.target.value })} /></div>
            </div>
            <div className={styles.row}>
              <strong>{t("密码")}</strong>
              <div className={styles.control}><TextInput type="password" value={draft.password ?? ""} autoComplete="new-password" placeholder={settings?.has_password ? t("留空表示保留当前密码") : ""} onChange={(event) => onDraftChange({ ...draft, password: event.target.value })} /></div>
            </div>
          </div>}
        </div>}
      </> : <>
        <div className={styles.row}><strong>{t("代理方式")}</strong><span className={styles.value}>{settings ? modeLabel(settings.mode) : t("加载中…")}</span></div>
        {settings?.mode === "custom" && <div className={styles.customFields}>
          <div className={styles.row}><strong>{t("代理地址")}</strong><span className={styles.value}>{settings.address}</span></div>
          <div className={styles.row}><strong>{t("认证")}</strong><span className={styles.value}>{settings.auth_enabled ? t("已启用") : t("未启用")}</span></div>
          {settings.auth_enabled && <>
            <div className={styles.row}><strong>{t("用户名")}</strong><span className={styles.value}>{settings.username || "—"}</span></div>
            <div className={styles.row}><strong>{t("密码")}</strong><span className={styles.value}>{settings.has_password ? t("已设置") : t("未设置")}</span></div>
          </>}
        </div>}
      </>}
      {shownTest && <ProxyTestLines test={shownTest} />}
    </div>
  </TitledCard>;
}
