import { useEffect, useMemo, useState } from "react";
import { api, type ApiKeySettings, type ApiKeyTestResult, type KeyOrder, type KeyProxy, type KeyProxyMode, type Model, type ProviderApiKey } from "../../shared/api";
import { useAppStore } from "../../shared/store/appStore";
import { Button } from "../../shared/ui/Button";
import { Checkbox } from "../../shared/ui/Checkbox";
import { FormField, SecretTextInput, TextInput } from "../../shared/ui/FormControls";
import { Icon } from "../../shared/ui/Icon";
import { addIcon, chevronDownIcon, windowCloseIcon } from "../../shared/ui/icons";
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

/** Asterisks that stand for the hidden middle of a key. */
const KEY_MASK = "******";

/** A key as shown in its collapsed row: the first and last three characters around six
 * asterisks, so keys can be told apart without showing them. Keys too short to keep most of
 * them hidden show only the asterisks. */
function maskedKey(key: string): string {
  const trimmed = key.trim();
  return trimmed.length > 12 ? `${trimmed.slice(0, 3)}${KEY_MASK}${trimmed.slice(-3)}` : KEY_MASK;
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
  /** The positions of the keys whose settings are open, per provider. Keys start collapsed. */
  const [expanded, setExpanded] = useState<Record<string, number[]>>({});

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

  const open = new Set(expanded[host] ?? []);
  const setOpen = (indexes: number[]) => setExpanded((current) => ({ ...current, [host]: indexes }));
  const toggleKey = (index: number) =>
    setOpen(open.has(index) ? [...open].filter((position) => position !== index) : [...open, index]);
  const addKey = () => {
    setKeys([...keys, { key: "", enabled: true, proxy: { ...EMPTY_PROXY } }]);
    setOpen([...open, keys.length]);
  };
  // Keys after the removed one move up a place, and stay open or closed as they were.
  const removeKey = (index: number) => {
    setKeys(keys.filter((_, position) => position !== index));
    setOpen([...open].filter((position) => position !== index).map((position) => (position > index ? position - 1 : position)));
  };

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
    {changed && <Button size="small" disabled={saving} onClick={() => {
      if (!saved) return;
      setDraft(saved);
      setExpanded({});
    }}>{t("Cancel")}</Button>}
    <Button size="small" variant="primary" disabled={!changed || saving} onClick={() => void save()}>
      {saving ? t("Saving…") : t("Save")}
    </Button>
  </div>}>
    <div className={styles.content}>
      <div className={styles.row}>
        <div className={styles.description}>
          <strong>{t("Enable multiple API keys")}</strong>
          <small>{t("Save several keys for the same provider, each with its own proxy. While a provider has keys here, its models use only these keys, not the API key in the model. When the provider refuses a key, or its proxy cannot connect, the same request moves on to the next key at once.")}</small>
        </div>
        <Switch label={t("Enable multiple API keys")} checked={draft.enabled} disabled={disabled}
          onChange={(enabled) => setDraft((current) => ({ ...current, enabled }))} />
      </div>

      <FormField label={t("Key order")} hint={t("Round robin spreads requests evenly over the keys that are ready. In order keeps using the key that last worked and moves on only when that key runs out, so keys are used up one after another. Either way, a key that is out of credit or quota is skipped for 5 minutes, growing to 1 hour, or until the time the provider gives; a rate-limited key until the provider's retry time; a key the provider does not accept for 30 minutes. A key whose server or proxy failed is tried after the other keys. When every key is resting, the one that recovers first is tried. Saving these settings clears all of this.")}>
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
          const isOpen = open.has(index);
          const label = t("Key {index}", { index: index + 1 });
          const bodyId = `api-key-${index}`;
          // Buttons in the row act on their own; a click anywhere else on the row opens or closes it.
          const own = (action: () => void) => (event: React.MouseEvent) => {
            event.stopPropagation();
            action();
          };
          return <div className={[styles.key, entry.enabled ? "" : styles.keyOff].filter(Boolean).join(" ")} key={index}>
            <div className={styles.keyRow} onClick={() => toggleKey(index)}
              title={isOpen ? t("Hide the settings of {key}", { key: label }) : t("Edit {key}", { key: label })}>
              <strong>{label}</strong>
              {entry.key.trim()
                ? <code className={styles.keyMask}>{maskedKey(entry.key)}</code>
                : <small className={styles.note}>{t("no key yet")}</small>}
              {!entry.enabled && <small className={styles.tag}>{t("Off")}</small>}
              <div className={styles.keyRowEnd}>
                {entry.proxy.mode === "custom" && <small className={styles.tag} title={t("This key connects through its own proxy")}>{t("Proxy")}</small>}
                <Button size="small" disabled={!saved || !entry.key.trim() || test?.running}
                  onClick={own(() => void testKey(entry))}>{test?.running ? t("Testing…") : t("Test")}</Button>
                <button type="button" className={styles.iconButton} aria-expanded={isOpen} aria-controls={bodyId}
                  aria-label={isOpen ? t("Hide the settings of {key}", { key: label }) : t("Edit {key}", { key: label })}
                  onClick={own(() => toggleKey(index))}>
                  <Icon icon={chevronDownIcon} size="1.2em" className={[styles.chevron, isOpen ? styles.chevronOpen : ""].filter(Boolean).join(" ")} />
                </button>
                <button type="button" className={[styles.iconButton, styles.removeButton].join(" ")} title={t("Remove {key}", { key: label })} aria-label={t("Remove {key}", { key: label })}
                  disabled={disabled} onClick={own(() => removeKey(index))}>
                  <Icon icon={windowCloseIcon} size="1.1em" />
                </button>
              </div>
            </div>
            {isOpen && <div className={styles.keyBody} id={bodyId}>
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
            <div className={styles.keyActions}>
              <Switch label={t("Enable this key")} checked={entry.enabled} disabled={disabled}
                onChange={(enabled) => updateKey(index, { enabled })} />
              <small className={styles.note}>{t("Use this key")}</small>
            </div>
            </div>}
            {test && <div className={styles.keyTest}><KeyTestLine test={test} /></div>}
          </div>;
        })}

        <div className={styles.addRow}>
          <Button size="small" disabled={disabled || !host} onClick={addKey}>
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
