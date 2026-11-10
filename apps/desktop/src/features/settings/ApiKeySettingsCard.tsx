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
    } else if (proxy.includes(":")) {
      // Any proxy format the server reads (host:port:user:pass, user:pass@host:port, ...);
      // the server validates it on save and moves credentials into the auth fields.
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
  if (test.running) return <small className={styles.note}>{t("Testing…")}</small>;
  if ("error" in test) return <small className={styles.testError}>{t("Test failed: {error}", { error: test.error })}</small>;
  const { result } = test;
  const status = result.http_status === null ? "" : `HTTP ${result.http_status}`;
  const summary = {
    ok: { className: styles.testOk, text: t("✓ Key works (model {model}, {duration} ms)", { model: result.model ?? "", duration: result.duration_ms }) },
    rejected: { className: styles.testError, text: t("✗ The provider rejected this key ({status})", { status }) },
    request_failed: { className: styles.testWarn, text: t("! Reached the provider, but the test request was refused ({status}); the key is not necessarily invalid", { status }) },
    unreachable: { className: styles.testError, text: t("✗ Cannot reach the provider through this connection") },
    no_model: { className: styles.testWarn, text: t("No model uses this provider, so only the network connection was checked") },
  }[result.status];
  return <div className={styles.testResult}>
    <small className={summary.className}>{summary.text}</small>
    {result.detail && <small className={styles.testDetail}>{result.detail}</small>}
    <small className={styles.note}>
      {result.egress_ip
        ? t("Egress IP seen by the provider: {ip}", { ip: result.egress_ip })
        : t("Could not get the egress IP: {error}", { error: result.egress_error ?? "" })}
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
    message(t("Added {added} keys, skipped {skipped} lines", { added: fresh.length, skipped: invalid + parsed.length - fresh.length }));
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
      message(t("Multiple API key settings saved"));
    } catch (cause) {
      message(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  const proxyOptions: { value: KeyProxyMode; label: string }[] = [
    { value: "inherit", label: t("Follow proxy settings") },
    { value: "custom", label: t("Custom proxy") },
    { value: "direct", label: t("Direct (no proxy)") },
  ];

  return <TitledCard title={t("Multiple API keys")} action={<div className={styles.actionGroup}>
    {changed && <Button size="small" disabled={saving} onClick={() => saved && setDraft(saved)}>{t("Cancel")}</Button>}
    <Button size="small" variant="primary" disabled={!changed || saving} onClick={() => void save()}>
      {saving ? t("Saving…") : t("Save")}
    </Button>
  </div>}>
    <div className={styles.content}>
      <div className={styles.row}>
        <div className={styles.description}>
          <strong>{t("Enable multiple API keys")}</strong>
          <small>{t("Save several keys for the same provider, each with its own proxy. Every request starts with the next key in turn; when a key is rejected (401, 402, 403, 429, 5xx) or its proxy cannot connect, the next key is used automatically.")}</small>
        </div>
        <Switch label={t("Enable multiple API keys")} checked={draft.enabled} disabled={disabled}
          onChange={(enabled) => setDraft((current) => ({ ...current, enabled }))} />
      </div>

      {hosts.length === 0 ? <small className={styles.note}>{t("Add a model first; providers are grouped by the model server address.")}</small> : <>
        <FormField label={t("Provider")} hint={t("Grouped by the host of the model server address. Providers without keys keep using each model's own API key.")}>
          <Select ariaLabel={t("Provider")} value={host} disabled={disabled}
            options={hosts.map((item) => ({ value: item, label: t("{host} ({count} keys)", { host: item, count: draft.providers[item]?.length ?? 0 }) }))}
            onChange={setSelectedHost} />
        </FormField>
        <small className={styles.note}>
          {hostModels.length
            ? t("Models using this provider: {models}", { models: hostModels.join(", ") })
            : t("No model uses this provider yet.")}
        </small>

        {keys.map((entry, index) => {
          const test = tests[testId(host, entry)];
          return <div className={styles.key} key={index}>
            <div className={styles.keyHeader}>
              <strong>{t("Key {index}", { index: index + 1 })}</strong>
              <div className={styles.keyActions}>
                <Button size="small" disabled={!saved || !entry.key.trim() || test?.running}
                  onClick={() => void testKey(entry)}>{t("Test")}</Button>
                <Switch label={t("Enable this key")} checked={entry.enabled} disabled={disabled}
                  onChange={(enabled) => updateKey(index, { enabled })} />
                <button type="button" className={styles.iconButton} title={t("Delete key")} aria-label={t("Delete key")} disabled={disabled}
                  onClick={() => setKeys(keys.filter((_, position) => position !== index))}>
                  <Icon icon={trashIcon} size="1.1em" />
                </button>
              </div>
            </div>
            <div className={styles.grid}>
              <FormField label={t("API key")}>
                <SecretTextInput value={entry.key} autoComplete="off" disabled={disabled}
                  onChange={(event) => updateKey(index, { key: event.target.value })} />
              </FormField>
              <FormField label={t("Proxy")}>
                <Select ariaLabel={t("Proxy")} value={entry.proxy.mode} disabled={disabled} options={proxyOptions}
                  onChange={(mode) => updateProxy(index, { mode: mode as KeyProxyMode })} />
              </FormField>
            </div>
            {entry.proxy.mode === "custom" && <div className={styles.proxyFields}>
              <FormField label={t("Proxy address")} hint={t("Supports http://, https://, socks5:// and socks5h://. You can also put user:pass@ in the address.")}>
                <TextInput value={entry.proxy.address} placeholder="socks5://127.0.0.1:1080" autoComplete="off" disabled={disabled}
                  onChange={(event) => updateProxy(index, { address: event.target.value })} />
              </FormField>
              <Checkbox checked={entry.proxy.auth_enabled} disabled={disabled} label={t("Proxy requires authentication")}
                onChange={(auth_enabled) => updateProxy(index, { auth_enabled })} />
              {entry.proxy.auth_enabled && <div className={styles.grid}>
                <FormField label={t("Username")}>
                  <TextInput value={entry.proxy.username} autoComplete="off" disabled={disabled}
                    onChange={(event) => updateProxy(index, { username: event.target.value })} />
                </FormField>
                <FormField label={t("Password")}>
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
            <Icon icon={addIcon} size="1em" /> {t("Add key")}
          </Button>
          {keys.length > 1 && <Button size="small" disabled={!saved || testableKeys.length === 0}
            onClick={() => void Promise.all(testableKeys.map(testKey))}>{t("Test all")}</Button>}
        </div>
        <small className={styles.note}>{t("“Test” uses the key and proxy as currently entered (no need to save first), sends one very short request to this provider's first model, and shows the egress IP the provider sees.")}</small>

        <FormField label={t("Bulk add")} hint={t("One key per line, as: key | proxy. Leave the proxy empty (follow proxy settings), write direct (no proxy), or give a proxy address.")}>
          <textarea className={styles.bulk} rows={4} value={bulkText} disabled={disabled} spellCheck={false}
            placeholder={"sk-key-1\nsk-key-2 | socks5://user:pass@127.0.0.1:1080\nsk-key-3 | direct"}
            onChange={(event) => setBulkText(event.target.value)} />
        </FormField>
        <div className={styles.addRow}>
          <Button size="small" disabled={disabled || !host || !bulkText.trim()} onClick={addBulk}>{t("Add to list")}</Button>
        </div>
        <small className={styles.note}>{t("Changes take effect after you click Save at the top right.")}</small>
      </>}
    </div>
  </TitledCard>;
}
