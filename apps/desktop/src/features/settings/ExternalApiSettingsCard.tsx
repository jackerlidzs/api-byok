import { useEffect, useState } from "react";
import { api, type ExternalApiSettings } from "../../shared/api";
import { Button } from "../../shared/ui/Button";
import { FormField, SecretTextInput } from "../../shared/ui/FormControls";
import { Switch } from "../../shared/ui/Switch";
import { TitledCard } from "../../shared/ui/TitledCard";
import { useMessage } from "../../shared/ui/message";
import styles from "./ExternalApiSettingsCard.module.scss";

export function ExternalApiSettingsCard({ servicePort }: { servicePort: number }) {
  const message = useMessage();
  const [saved, setSaved] = useState<ExternalApiSettings | null>(null);
  const [draft, setDraft] = useState<ExternalApiSettings>({ enabled: false, api_key: "" });
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    void api.externalApiSettings().then((settings) => {
      setSaved(settings);
      setDraft(settings);
    }).catch((cause: unknown) => message(cause instanceof Error ? cause.message : String(cause)));
  }, [message]);

  const save = async () => {
    try {
      setSaving(true);
      const settings = await api.setExternalApiSettings(draft);
      setSaved(settings);
      setDraft(settings);
      message(t("External API settings saved"));
    } catch (cause) {
      message(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  const address = `http://127.0.0.1:${servicePort}/byok/v1`;
  const changed = saved && (saved.enabled !== draft.enabled || saved.api_key !== draft.api_key);

  return <TitledCard title={t("External API")} action={<Button size="small" variant="primary" disabled={!changed || saving} onClick={() => void save()}>
    {saving ? t("Saving…") : t("Save")}
  </Button>}>
    <div className={styles.content}>
      <div className={styles.row}>
        <div className={styles.description}>
          <strong>{t("Enable external API")}</strong>
          <small>{t("Allow local apps to call configured models, including plugin models, with an API key.")}</small>
        </div>
        <Switch label={t("Enable external API")} checked={draft.enabled} disabled={!saved || saving}
          onChange={(enabled) => setDraft((current) => ({ ...current, enabled }))} />
      </div>
      <FormField label={t("API key")} hint={t("All external requests must provide this key when enabled.")}> 
        <SecretTextInput value={draft.api_key} autoComplete="off" disabled={!saved || saving}
          onChange={(event) => setDraft((current) => ({ ...current, api_key: event.target.value }))} />
      </FormField>
      <div className={styles.address}>
        <strong>{t("Base URL")}</strong>
        <code>{address}</code>
      </div>
    </div>
  </TitledCard>;
}
