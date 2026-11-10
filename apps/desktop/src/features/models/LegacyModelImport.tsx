import { useState, type ReactNode } from "react";
import { api, type LegacyModelImportPreview } from "../../shared/api";
import { appStore } from "../../shared/store/appStore";
import { ConfirmDialog } from "../../shared/ui/ConfirmDialog";
import { useMessage } from "../../shared/ui/message";
import styles from "./LegacyModelImport.module.scss";

type LegacyModelImportControl = {
  busy: boolean;
  previewing: boolean;
  open: () => void;
};

export function LegacyModelImport({ children }: { children: (control: LegacyModelImportControl) => ReactNode }) {
  const message = useMessage();
  const [preview, setPreview] = useState<LegacyModelImportPreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [importing, setImporting] = useState(false);

  const open = async () => {
    try {
      setPreviewing(true);
      setPreview(await api.previewV0049Models());
    } catch (cause) {
      message(errorText(cause));
    } finally {
      setPreviewing(false);
    }
  };

  const confirm = async () => {
    try {
      setImporting(true);
      const result = await appStore.importV0049Models();
      if (!result) {
        const error = appStore.getSnapshot().error;
        if (error) message(error);
        return;
      }
      setPreview(null);
      message(result.imported > 0
        ? t("Import complete: added {imported} models and skipped {skipped} existing models", { imported: result.imported, skipped: result.skipped })
        : t("All {count} models in this configuration already exist; nothing needs to be imported", { count: result.skipped }));
    } catch (cause) {
      message(errorText(cause));
    } finally {
      setImporting(false);
    }
  };

  return <>
    {children({ busy: previewing || importing, previewing, open: () => void open() })}
    <ConfirmDialog
      open={preview !== null}
      title={t("Confirm legacy model configuration import")}
      busy={importing}
      wide
      cancelLabel={t("Cancel")}
      confirmLabel={t("Confirm import")}
      onCancel={() => setPreview(null)}
      onConfirm={() => void confirm()}
    >
      {preview && <div className={styles.content}>
        <div className={styles.source}>
          <strong>{t("Configuration file")}</strong>
          <code>{preview.source}</code>
        </div>
        <div className={styles.counts}>
          <div><strong>{preview.total}</strong><small>{t("Configured models")}</small></div>
          <div><strong>{preview.new_models}</strong><small>{t("To add")}</small></div>
          <div><strong>{preview.existing_models}</strong><small>{t("Existing")}</small></div>
        </div>
        <div className={styles.models}>
          {preview.models.map((model) => <div className={styles.model} key={model.model_hash}>
            <div>
              <strong>{model.display_name}</strong>
              <small>{model.model_id} · {model.type === "openai" ? "OpenAI" : "Anthropic"}</small>
            </div>
            <span className={model.existing ? styles.existing : styles.new}>
              {model.existing ? t("Existing, skipped") : t("New")}
            </span>
          </div>)}
        </div>
        <small className={styles.hint}>{t("Importing the same configuration again will not create duplicate models. Existing models are skipped automatically.")}</small>
      </div>}
    </ConfirmDialog>
  </>;
}

function errorText(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}
