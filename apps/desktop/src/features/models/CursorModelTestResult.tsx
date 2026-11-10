import type { ModelConnectivityResult } from "../../shared/api";
import { Icon } from "../../shared/ui/Icon";
import { TooltipTrigger } from "../../shared/ui/TooltipTrigger";
import { informationOutlineIcon } from "../../shared/ui/icons";
import styles from "./CursorModelTestResult.module.scss";

export type CursorModelTestState =
  | { status: "success"; result: ModelConnectivityResult }
  | { status: "error"; error: string }
  | { status: "cancelled" };

export function CursorModelTestResult({ state, testing = false, compact = false }: {
  state?: CursorModelTestState;
  testing?: boolean;
  /** Compact badge for list rows: nothing until tested, details in the tooltip. */
  compact?: boolean;
}) {
  if (testing) {
    return compact
      ? <span className={`${styles.compact} ${styles.testing}`}>{t("Testing…")}</span>
      : <div className={`${styles.root} ${styles.testing}`}><span className={styles.summary}>{t("Testing…")}</span></div>;
  }
  if (!state) {
    return compact ? null : <div className={`${styles.root} ${styles.idle}`}><span className={styles.summary}>{t("Not tested")}</span></div>;
  }
  if (state.status === "cancelled") {
    return compact
      ? <span className={`${styles.compact} ${styles.idle}`}>{t("Test cancelled")}</span>
      : <div className={`${styles.root} ${styles.idle}`}><span className={styles.summary}>{t("Test cancelled")}</span></div>;
  }

  const success = state.status === "success";
  const summary = success
    ? t("Speed: {speed} tokens/s", { speed: formatSpeed(state.result.tokens_per_second) })
    : t("Error: {error}", { error: state.error });
  const detail = success
    ? t("Speed {speed} tokens/s · first token {firstText} ms · total {duration} ms · output {tokens} tokens{estimated} · response: {output}", {
      speed: formatSpeed(state.result.tokens_per_second),
      firstText: state.result.first_valid_response_ms ?? "--",
      duration: state.result.duration_ms,
      tokens: state.result.output_tokens,
      estimated: state.result.tokens_estimated ? t(" (estimated)") : "",
      output: state.result.output || "--",
    })
    : t("Test failed: {error}", { error: state.error });

  if (compact) {
    return <TooltipTrigger label={detail}>
      <span className={`${styles.compact} ${success ? styles.success : styles.error}`}>
        {success ? `${formatSpeed(state.result.tokens_per_second)} tokens/s` : t("Test failed")}
        <Icon icon={informationOutlineIcon} size="1em" />
      </span>
    </TooltipTrigger>;
  }

  return <div className={`${styles.root} ${success ? styles.success : styles.error}`}>
    <span className={styles.summary}>{summary}</span>
    <TooltipTrigger label={detail}><button type="button" className={styles.details}>{t("View details")}<Icon icon={informationOutlineIcon} size="1.1em" /></button></TooltipTrigger>
  </div>;
}

function formatSpeed(value: number) {
  return Number.isFinite(value) ? value.toFixed(1) : "0.0";
}
