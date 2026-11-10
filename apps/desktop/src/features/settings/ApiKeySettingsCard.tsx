import { useEffect, useMemo, useState } from "react";
import { api, type ApiKeySettings, type ApiKeyTestResult, type KeyOrder, type KeyProxy, type KeyProxyMode, type Model, type ProviderApiKey } from "../../shared/api";
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
const EMPTY_SETTINGS: ApiKeySettings = { enabled: false, order: "round_robin", providers: {} };

/** The provider a model belongs to: the lowercase host of its server address. */
function providerHost(baseUrl: string): string | null {
  try {
    return new URL(baseUrl.trim()).hostname.toLowerCase() || null;
  } catch {
    return null;
  }
}

type BulkResult = { added: ProviderApiKey[]; rejected: { line: string; reason: string }[] };

/**
 * Reads pasted lines of `key`, `key | direct` or `key | proxy`. Keys already in `existing`, or
 * earlier in the paste, are not added again. Lines that cannot be added come back with the
 * reason, so they can be fixed instead of disappearing.
 */
function parseBulkKeys(text: string, existing: ReadonlySet<string>): BulkResult {
  const added: ProviderApiKey[] = [];
  const rejected: BulkResult["rejected"] = [];
  const seen = new Set(existing);
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const entry = parseBulkLine(line);
    if (typeof entry === "string") {
      rejected.push({ line, reason: entry });
    } else if (seen.has(entry.key)) {
      rejected.push({ line, reason: t("this key is already in the list") });
    } else {
      seen.add(entry.key);
      added.push(entry);
    }
  }
  return { added, rejected };
}

/**
 * One pasted line as a key, or the reason it cannot be added. Everything after the first `|` is
 * the proxy, so a password containing `|` stays whole. The proxy may be in any format Proxy
 * settings accepts; the server validates it on save and moves credentials into the auth fields.
 */
function parseBulkLine(line: string): ProviderApiKey | string {
  const separator = line.indexOf("|");
  const key = (separator < 0 ? line : line.slice(0, separator)).trim();
  const proxy = separator < 0 ? "" : line.slice(separator + 1).trim();
  if (!key) return t("the key is empty");
  if (/\s/.test(key)) return t("the key contains spaces; put | between the key and the proxy");
  if (key.includes("://")) return t("the line starts with a proxy; write the key first: key | proxy");
  if (!proxy) return { key, enabled: true, proxy: { ...EMPTY_PROXY } };
  if (proxy.toLowerCase() === "direct") return { key, enabled: true, proxy: { ...EMPTY_PROXY, mode: "direct" } };
  // Every proxy format the server reads has a ":" or a space (host:port, host port user pass, ...).
  if (!/[:\s]/.test(proxy)) return t("unknown proxy; leave it empty, write direct, or give an address such as host:port");
  return { key, enabled: true, proxy: { ...EMPTY_PROXY, mode: "custom", address: proxy } };
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
  const [bulkProblems, setBulkProblems] = useState<string[]>([]);
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
    const { added, rejected } = parseBulkKeys(bulkText, new Set(keys.map((entry) => entry.key.trim())));
    if (added.length) setKeys([...keys, ...added]);
    // Only the lines that were added leave the box; the rest stay, numbered as they now appear.
    setBulkText(rejected.map((entry) => entry.line).join("\n"));
    setBulkProblems(rejected.map((entry, index) => t("Line {line}: {reason}", { line: index + 1, reason: entry.reason })));
    message(rejected.length
      ? t("Added {added} keys; {rejected} lines were not added and are still in the box", { added: added.length, rejected: rejected.length })
      : t("Added {added} keys", { added: added.length }));
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
  const orderOptions: { value: KeyOrder; label: string }[] = [
    { value: "round_robin", label: t("Round robin: each request starts with the next key") },
    { value: "in_order", label: t("In order: stay on one key until it is refused, then move to the next") },
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
          <small>{t("Save several keys for the same provider, each with its own proxy. While a provider has keys here, its models use only these keys, not the API key in the model. When a key is rejected (401, 402, 403, 429, 5xx) or its proxy cannot connect, the same request moves on to the next key.")}</small>
        </div>
        <Switch label={t("Enable multiple API keys")} checked={draft.enabled} disabled={disabled}
          onChange={(enabled) => setDraft((current) => ({ ...current, enabled }))} />
      </div>

      <FormField label={t("Key order")} hint={t("Round robin spreads requests over every key. In order keeps using the key that last worked and only moves on when the provider refuses it, so keys are used up one after another. Either way, a key refused with 401, 402 or 403 (invalid key, out of credit) is tried last for the next 10 minutes; saving these settings clears that.")}>
        <Select ariaLabel={t("Key order")} value={draft.order} disabled={disabled} options={orderOptions}
          onChange={(order) => setDraft((current) => ({ ...current, order: order as KeyOrder }))} />
      </FormField>

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

        <FormField label={t("Bulk add")} hint={t("One key per line: key | proxy. Everything after the first | is the proxy: leave it empty to follow Proxy settings, write direct for no proxy, or paste the proxy in any format Proxy settings accepts.")}>
          <textarea className={styles.bulk} rows={4} value={bulkText} disabled={disabled} spellCheck={false}
            placeholder={"sk-key-1\nsk-key-2 | direct\nsk-key-3 | socks5://user:pass@127.0.0.1:1080\nsk-key-4 | host:port:user:pass"}
            onChange={(event) => {
              setBulkText(event.target.value);
              setBulkProblems([]);
            }} />
        </FormField>
        {bulkProblems.length > 0 && <div className={styles.testResult}>
          <small className={styles.testError}>{t("Not added (fix these lines in the box and add again):")}</small>
          {bulkProblems.map((problem, index) => <small key={index} className={styles.testError}>{problem}</small>)}
        </div>}
        <div className={styles.addRow}>
          <Button size="small" disabled={disabled || !host || !bulkText.trim()} onClick={addBulk}>{t("Add to list")}</Button>
        </div>
        <small className={styles.note}>{t("Changes take effect after you click Save at the top right.")}</small>
      </>}
    </div>
  </TitledCard>;
}
