import { useCallback, useEffect, useMemo, useState } from "react";
import { api, pluginText, type CommitSettingsView } from "../../shared/api";
import { LOCALE } from "../../i18n/runtime";
import { useAppStore } from "../../shared/store/appStore";
import { Button } from "../../shared/ui/Button";
import { Modal } from "../../shared/ui/Modal";
import { ModelSelect, type ModelSelectOption } from "../../shared/ui/ModelSelect";
import { claudeIcon, flatColorOrganizationIcon, openAiIcon } from "../../shared/ui/icons";
import { TitledCard } from "../../shared/ui/TitledCard";
import { useMessage } from "../../shared/ui/message";
import controls from "../../shared/ui/Controls.module.scss";
import { modelProviderName } from "../../shared/utils/modelProvider";
import styles from "./CommitSettingsCard.module.scss";

function errorText(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

export function CommitSettingsCard() {
  const { models, plugins } = useAppStore();
  const message = useMessage();
  const [view, setView] = useState<CommitSettingsView | null>(null);
  const [modelDraft, setModelDraft] = useState("");
  const [editing, setEditing] = useState(false);
  const [savingModel, setSavingModel] = useState(false);
  const [promptOpen, setPromptOpen] = useState(false);
  const [promptDraft, setPromptDraft] = useState("");
  const [savingPrompt, setSavingPrompt] = useState(false);

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        let loaded = await api.commitSettings(LOCALE);
        // Settings saved by a build with a Chinese interface keep the Chinese built-in prompt.
        if (!loaded.prompt.trim() && loaded.prompt_locale !== LOCALE) {
          loaded = await api.setCommitSettings({
            model_id: loaded.model_id,
            prompt: "",
            prompt_locale: LOCALE,
          });
        }
        if (active) {
          setView(loaded);
          setModelDraft(loaded.model_id);
        }
      } catch (cause) {
        if (active) message(errorText(cause));
      }
    })();
    return () => {
      active = false;
    };
  }, [message]);

  const modelOptions = useMemo(() => {
    const options: ModelSelectOption[] = [{ value: "", label: t("Direct"), group: "Cursor" }];
    const seen = new Set<string>();
    for (const model of models) {
      seen.add(model.model_hash);
      options.push({
        value: model.model_hash,
        label: model.display_name && model.display_name !== model.model_id
          ? `${model.display_name} (${model.model_id})`
          : model.display_name || model.model_id,
        group: modelProviderName(model),
        icon: model.type === "anthropic" ? claudeIcon : openAiIcon,
      });
    }
    for (const plugin of plugins) {
      for (const provider of plugin.providers) {
        if (!provider.configured) continue;
        const group = pluginText(provider.displayName, LOCALE) || plugin.name;
        for (const model of provider.models.filter((model) => model.enabled)) {
          seen.add(model.id);
          options.push({
            value: model.id,
            label: model.displayName,
            group,
            iconSrc: model.icon || undefined,
            icon: model.icon ? undefined : flatColorOrganizationIcon,
          });
        }
      }
    }
    if (view?.model_id && !seen.has(view.model_id)) {
      options.push({ value: view.model_id, label: view.model_id, group: "Cursor" });
    }
    return options;
  }, [models, plugins, view]);

  const persist = useCallback(
    async (modelId: string, prompt: string) => {
      if (!view) return null;
      const normalizedPrompt =
        prompt.trim() === view.default_prompt.trim() ? "" : prompt.trim();
      return api.setCommitSettings({
        model_id: modelId,
        prompt: normalizedPrompt,
        prompt_locale: LOCALE,
      });
    },
    [view],
  );

  const editModel = useCallback(() => {
    if (!view) return;
    setModelDraft(view.model_id);
    setEditing(true);
  }, [view]);

  const cancelModelEdit = useCallback(() => {
    setModelDraft(view?.model_id ?? "");
    setEditing(false);
  }, [view]);

  const saveModel = useCallback(async () => {
    if (!view) return;
    setSavingModel(true);
    try {
      const saved = await persist(modelDraft, view.prompt);
      if (saved) {
        setView(saved);
        setModelDraft(saved.model_id);
        setEditing(false);
      }
    } catch (cause) {
      message(errorText(cause));
    } finally {
      setSavingModel(false);
    }
  }, [modelDraft, view, persist, message]);

  const openPrompt = useCallback(() => {
    if (!view) return;
    setPromptDraft(view.prompt || view.default_prompt);
    setPromptOpen(true);
  }, [view]);

  const savePrompt = useCallback(async () => {
    if (!view) return;
    setSavingPrompt(true);
    try {
      const saved = await persist(view.model_id, promptDraft);
      if (saved) setView(saved);
      setPromptOpen(false);
      message(t("Prompt settings saved"));
    } catch (cause) {
      message(errorText(cause));
    } finally {
      setSavingPrompt(false);
    }
  }, [view, promptDraft, persist, message]);

  const resetPrompt = useCallback(() => {
    if (!view) return;
    setPromptDraft(view.default_prompt);
  }, [view]);

  const selectedModelLabel = modelOptions.find((option) => option.value === view?.model_id)?.label
    ?? view?.model_id
    ?? t("Loading…");
  const action = editing ? (
    <div className={styles.actionGroup}>
      <Button size="small" disabled={savingModel} onClick={cancelModelEdit}>{t("Cancel")}</Button>
      <Button variant="primary" size="small" disabled={savingModel} onClick={() => void saveModel()}>
        {savingModel ? t("Saving…") : t("Save")}
      </Button>
    </div>
  ) : (
    <div className={styles.actionGroup}>
      <button type="button" className={styles.textButton} disabled={!view} onClick={openPrompt}>
        {t("Prompt settings")}
      </button>
      <button type="button" className={styles.textButton} disabled={!view} onClick={editModel}>
        {t("Edit")}
      </button>
    </div>
  );

  return (
    <>
      <TitledCard title={t("Commit Message Model Settings")} action={action}>
        <div className={styles.content}>
          <div className={styles.row}>
            <div className={styles.details}>
              <strong>{t("Generation model")}</strong>
            </div>
            {editing ? <div className={styles.select}>
              <ModelSelect
                mode="single"
                value={modelDraft}
                options={modelOptions}
                disabled={savingModel}
                label={t("Generation model")}
                onChange={setModelDraft}
              />
            </div> : <span className={styles.value}>{selectedModelLabel}</span>}
          </div>
        </div>
      </TitledCard>

      <Modal
        open={promptOpen}
        title={t("Prompt settings")}
        wide
        fullHeight
        busy={savingPrompt}
        onClose={() => setPromptOpen(false)}
        onSubmit={() => void savePrompt()}
        submitLabel={t("Save")}
        closeLabel={t("Cancel")}
        secondaryAction={
          <button type="button" className={controls.secondary} onClick={resetPrompt}>
            {t("Restore default")}
          </button>
        }
      >
        <textarea
          className={styles.promptEditor}
          value={promptDraft}
          spellCheck={false}
          aria-label={t("Commit message prompt")}
          onChange={(event) => setPromptDraft(event.target.value)}
        />
      </Modal>
    </>
  );
}
