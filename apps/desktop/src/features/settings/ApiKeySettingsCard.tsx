import { useEffect, useMemo, useState } from "react";
import { api, type ApiKeySettings, type ApiKeyTestResult, type KeyProxy, type KeyProxyMode, type Model, type ProviderApiKey } from "../../shared/api";
import { useAppStore } from "../../shared/store/appStore";
import { Button } from "../../shared/ui/Button";
import { Checkbox } from "../../shared/ui/Checkbox";
import { FormField, SecretTextInput, TextInput } from "../../shared/ui/FormControls";
import { Icon } from "../../shared/ui/Icon";
import { addIcon, trashIcon } from "../../shared/ui/icons";
import { useMessage } from "../../shared/ui/message";
import { Select } from "../../shared/ui/Select";
import { Switch } from "../../shared/ui/Switch";
import { TitledCard } from "../../shared/ui/TitledCard";
import styles from "./ApiKeySettingsCard.module.scss";

const EMPTY_PROXY: KeyProxy = { mode: "inherit", address: "", auth_enabled: false, username: "", password: "" };
const EMPTY_SETTINGS: ApiKeySettings = { enabled: false, providers: {} };

/** The provider a model belongs to: the lowercase host of its server address. */
function providerHost(baseUrl: string): string | null {
  try {
    return new URL(baseUrl.trim()).hostname.toLowerCase() || null;
  } catch {
    return null;
  }
}

/** Parses pasted lines of `key`, `key | proxy URL` or `key | direct`. */
function parseBulkKeys(text: string): { keys: ProviderApiKey[]; invalid: number } {
  const keys: ProviderApiKey[] = [];
  let invalid = 0;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const [key = "", proxy = ""] = line.split("|").map((part) => part.trim());
    if (!key || /\s/.test(key) || key.includes("://")) {
      invalid += 1;
      continue;
    }
    if (!proxy) {
      keys.push({ key, enabled: true, proxy: { ...EMPTY_PROXY } });
    } else if (proxy.toLowerCase() === "direct") {
      keys.push({ key, enabled: true, proxy: { ...EMPTY_PROXY, mode: "direct" } });
    } else if (/^(https?|socks5h?):\/\/\S+$/i.test(proxy)) {
      keys.push({ key, enabled: true, proxy: { ...EMPTY_PROXY, mode: "custom", address: proxy } });
    } else {
      invalid += 1;
    }
  }
  return { keys, invalid };
}

type KeyTest = { running: true } | { running: false; result: ApiKeyTestResult } | { running: false; error: string };

/** Identifies a test by provider, key and proxy, so editing the key or proxy hides its old result. */
function testId(host: string, entry: ProviderApiKey): string {
  return JSON.stringify([host, entry.key.trim(), entry.proxy]);
}

function KeyTestLine({ test }: { test: KeyTest }) {
  if (test.running) return <small className={styles.note}>{t("测试中…")}</small>;
  if ("error" in test) return <small className={styles.testError}>{t("测试失败：{error}", { error: test.error })}</small>;
  const { result } = test;
  const status = result.http_status === null ? "" : `HTTP ${result.http_status}`;
  const summary = {
    ok: { className: styles.testOk, text: t("✓ 密钥可用（模型 {model}，{duration} ms）", { model: result.model ?? "", duration: result.duration_ms }) },
    rejected: { className: styles.testError, text: t("✗ 服务商拒绝了此密钥（{status}）", { status }) },
    request_failed: { className: styles.testWarn, text: t("! 已连接到服务商，但测试请求被拒绝（{status}），密钥不一定无效", { status }) },
    unreachable: { className: styles.testError, text: t("✗ 无法通过此连接访问服务商") },
    no_model: { className: styles.testWarn, text: t("没有模型使用此服务商，只检查了网络连接") },
  }[result.status];
  return <div className={styles.testResult}>
    <small className={summary.className}>{summary.text}</small>
    {result.detail && <small className={styles.testDetail}>{result.detail}</small>}
    <small className={styles.note}>
      {result.egress_ip
        ? t("服务商看到的出口 IP：{ip}", { ip: result.egress_ip })
        : t("无法获取出口 IP：{error}", { error: result.egress_error ?? "" })}
    </small>
  </div>;
}

export function ApiKeySettingsCard() {
  const message = useMessage();
  const { models: storeModels } = useAppStore();
  const [loadedModels, setLoadedModels] = useState<Model[] | null>(null);
  const models = loadedModels ?? storeModels;
  const [saved, setSaved] = useState<ApiKeySettings | null>(null);
  const [draft, setDraft] = useState<ApiKeySettings>(EMPTY_SETTINGS);
  const [selectedHost, setSelectedHost] = useState("");
  const [bulkText, setBulkText] = useState("");
  const [saving, setSaving] = useState(false);
  const [tests, setTests] = useState<Record<string, KeyTest>>({});

  useEffect(() => {
    void api.apiKeySettings().then((settings) => {
      setSaved(settings);
      setDraft(settings);
    }).catch((cause: unknown) => message(cause instanceof Error ? cause.message : String(cause)));
    void api.models().then(setLoadedModels).catch(() => undefined);
  }, [message]);

  const modelsByHost = useMemo(() => {
    const result = new Map<string, string[]>();
    for (const model of models) {
      const host = providerHost(model.base_url);
      if (!host) continue;
      result.set(host, [...(result.get(host) ?? []), model.display_name]);
    }
    return result;
  }, [models]);
  const hosts = useMemo(
    () => [...new Set([...modelsByHost.keys(), ...Object.keys(draft.providers)])].sort(),
    [modelsByHost, draft.providers],
  );
  const host = hosts.includes(selectedHost) ? selectedHost : hosts[0] ?? "";
  const keys = draft.providers[host] ?? [];
  const hostModels = modelsByHost.get(host) ?? [];
  const changed = saved !== null && JSON.stringify(saved) !== JSON.stringify(draft);
  const disabled = !saved || saving;

  const setKeys = (next: ProviderApiKey[]) =>
    setDraft((current) => ({ ...current, providers: { ...current.providers, [host]: next } }));
  const updateKey = (index: number, patch: Partial<ProviderApiKey>) =>
    setKeys(keys.map((entry, position) => (position === index ? { ...entry, ...patch } : entry)));
  const updateProxy = (index: number, patch: Partial<KeyProxy>) =>
    updateKey(index, { proxy: { ...keys[index].proxy, ...patch } });

  const addBulk = () => {
    const { keys: parsed, invalid } = parseBulkKeys(bulkText);
    const existing = new Set(keys.map((entry) => entry.key.trim()));
    const fresh = parsed.filter((entry) => !existing.has(entry.key) && existing.add(entry.key));
    if (fresh.length) setKeys([...keys, ...fresh]);
    setBulkText("");
    message(t("已添加 {added} 个密钥，跳过 {skipped} 行", { added: fresh.length, skipped: invalid + parsed.length - fresh.length }));
  };

  const testKey = async (entry: ProviderApiKey) => {
    const id = testId(host, entry);
    setTests((current) => ({ ...current, [id]: { running: true } }));
    let test: KeyTest;
    try {
      test = { running: false, result: await api.testApiKey(host, entry) };
    } catch (cause) {
      test = { running: false, error: cause instanceof Error ? cause.message : String(cause) };
    }
    setTests((current) => ({ ...current, [id]: test }));
  };
  const testableKeys = keys.filter((entry) => entry.key.trim() && !tests[testId(host, entry)]?.running);

  const save = async () => {
    try {
      setSaving(true);
      const settings = await api.setApiKeySettings(draft);
      setSaved(settings);
      setDraft(settings);
      message(t("多密钥设置已保存"));
    } catch (cause) {
      message(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  const proxyOptions: { value: KeyProxyMode; label: string }[] = [
    { value: "inherit", label: t("跟随代理设置") },
    { value: "custom", label: t("自定义代理") },
    { value: "direct", label: t("直连（不使用代理）") },
  ];

  return <TitledCard title={t("多 API 密钥")} action={<div className={styles.actionGroup}>
    {changed && <Button size="small" disabled={saving} onClick={() => saved && setDraft(saved)}>{t("取消")}</Button>}
    <Button size="small" variant="primary" disabled={!changed || saving} onClick={() => void save()}>
      {saving ? t("保存中…") : t("保存")}
    </Button>
  </div>}>
    <div className={styles.content}>
      <div className={styles.row}>
        <div className={styles.description}>
          <strong>{t("开启多 API 密钥")}</strong>
          <small>{t("为同一服务商保存多个密钥，每个密钥可使用独立代理。每次请求轮流使用下一个密钥；密钥被拒绝（401、402、403、429、5xx）或代理无法连接时，自动改用下一个密钥。")}</small>
        </div>
        <Switch label={t("开启多 API 密钥")} checked={draft.enabled} disabled={disabled}
          onChange={(enabled) => setDraft((current) => ({ ...current, enabled }))} />
      </div>

      {hosts.length === 0 ? <small className={styles.note}>{t("请先添加模型；服务商按模型的服务地址分组。")}</small> : <>
        <FormField label={t("服务商")} hint={t("按模型服务地址的主机名分组。没有密钥的服务商继续使用模型自己的 API 密钥。")}>
          <Select ariaLabel={t("服务商")} value={host} disabled={disabled}
            options={hosts.map((item) => ({ value: item, label: t("{host}（{count} 个密钥）", { host: item, count: draft.providers[item]?.length ?? 0 }) }))}
            onChange={setSelectedHost} />
        </FormField>
        <small className={styles.note}>
          {hostModels.length
            ? t("使用此服务商的模型：{models}", { models: hostModels.join(", ") })
            : t("当前没有模型使用此服务商。")}
        </small>

        {keys.map((entry, index) => {
          const test = tests[testId(host, entry)];
          return <div className={styles.key} key={index}>
            <div className={styles.keyHeader}>
              <strong>{t("密钥 {index}", { index: index + 1 })}</strong>
              <div className={styles.keyActions}>
                <Button size="small" disabled={!saved || !entry.key.trim() || test?.running}
                  onClick={() => void testKey(entry)}>{t("测试")}</Button>
                <Switch label={t("启用此密钥")} checked={entry.enabled} disabled={disabled}
                  onChange={(enabled) => updateKey(index, { enabled })} />
                <button type="button" className={styles.iconButton} title={t("删除密钥")} aria-label={t("删除密钥")} disabled={disabled}
                  onClick={() => setKeys(keys.filter((_, position) => position !== index))}>
                  <Icon icon={trashIcon} size="1.1em" />
                </button>
              </div>
            </div>
            <div className={styles.grid}>
              <FormField label={t("API 密钥")}>
                <SecretTextInput value={entry.key} autoComplete="off" disabled={disabled}
                  onChange={(event) => updateKey(index, { key: event.target.value })} />
              </FormField>
              <FormField label={t("代理")}>
                <Select ariaLabel={t("代理")} value={entry.proxy.mode} disabled={disabled} options={proxyOptions}
                  onChange={(mode) => updateProxy(index, { mode: mode as KeyProxyMode })} />
              </FormField>
            </div>
            {entry.proxy.mode === "custom" && <div className={styles.proxyFields}>
              <FormField label={t("代理地址")} hint={t("支持 http://、https://、socks5://、socks5h://，也可以在地址中写 user:pass@。")}>
                <TextInput value={entry.proxy.address} placeholder="socks5://127.0.0.1:1080" autoComplete="off" disabled={disabled}
                  onChange={(event) => updateProxy(index, { address: event.target.value })} />
              </FormField>
              <Checkbox checked={entry.proxy.auth_enabled} disabled={disabled} label={t("代理需要认证")}
                onChange={(auth_enabled) => updateProxy(index, { auth_enabled })} />
              {entry.proxy.auth_enabled && <div className={styles.grid}>
                <FormField label={t("用户名")}>
                  <TextInput value={entry.proxy.username} autoComplete="off" disabled={disabled}
                    onChange={(event) => updateProxy(index, { username: event.target.value })} />
                </FormField>
                <FormField label={t("密码")}>
                  <SecretTextInput value={entry.proxy.password} autoComplete="new-password" disabled={disabled}
                    onChange={(event) => updateProxy(index, { password: event.target.value })} />
                </FormField>
              </div>}
            </div>}
            {test && <KeyTestLine test={test} />}
          </div>;
        })}

        <div className={styles.addRow}>
          <Button size="small" disabled={disabled || !host}
            onClick={() => setKeys([...keys, { key: "", enabled: true, proxy: { ...EMPTY_PROXY } }])}>
            <Icon icon={addIcon} size="1em" /> {t("添加密钥")}
          </Button>
          {keys.length > 1 && <Button size="small" disabled={!saved || testableKeys.length === 0}
            onClick={() => void Promise.all(testableKeys.map(testKey))}>{t("全部测试")}</Button>}
        </div>
        <small className={styles.note}>{t("“测试”使用当前填写的密钥和代理（无需先保存），向此服务商的第一个模型发送一条很短的请求，并显示服务商看到的出口 IP。")}</small>

        <FormField label={t("批量添加")} hint={t("每行一个密钥，格式：密钥 | 代理。代理可留空（跟随代理设置）、填 direct（直连）或填代理地址。")}>
          <textarea className={styles.bulk} rows={4} value={bulkText} disabled={disabled} spellCheck={false}
            placeholder={"sk-key-1\nsk-key-2 | socks5://user:pass@127.0.0.1:1080\nsk-key-3 | direct"}
            onChange={(event) => setBulkText(event.target.value)} />
        </FormField>
        <div className={styles.addRow}>
          <Button size="small" disabled={disabled || !host || !bulkText.trim()} onClick={addBulk}>{t("添加到列表")}</Button>
        </div>
        <small className={styles.note}>{t("修改后需点击右上角的“保存”才会生效。")}</small>
      </>}
    </div>
  </TitledCard>;
}
