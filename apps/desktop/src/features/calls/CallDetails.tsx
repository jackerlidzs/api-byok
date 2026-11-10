import type { CallDetail } from "../../shared/api";
import { JsonEditor } from "../../shared/ui/JsonEditor";
import { Tabs, type TabItem } from "../../shared/ui/Tabs";
import styles from "./CallDetails.module.scss";

const show = (value: string | number | null) => value ?? "-";
const timing = (value: number | null) => value == null ? "-" : `${value} ms`;

export function CallDetails({ detail }: { detail: CallDetail }) {
  const { call, request, response_chunks: chunks, cursor_trace: cursorTrace } = detail;
  const responseBody = chunks.map((chunk) => chunk.data).join("");
  const responseBytes = chunks.reduce((total, chunk) => total + chunk.byte_count, 0);
  const fields: Array<[string, string | number]> = [
    ["Call ID", call.call_id],
    [t("Call type"), call.call_kind === "cursor_official" ? t("Cursor official") : "LLM"],
    [t("Route"), call.route === "cursor_official" ? t("Cursor official") : "BYOK"],
    ["Run ID", call.run_id],
    ["Conversation ID", call.conversation_id],
    [t("Provider call sequence"), call.provider_call_index],
    ["Model Hash", show(call.model_hash)],
    [t("Provider type"), call.provider_type],
    [t("Provider URL"), call.provider_url],
    [t("Final request type"), call.request_type],
    [t("Final request URL"), call.request_url],
    ["Model ID", call.model_id],
    [t("Display name"), call.display_name],
    [t("Reasoning effort"), show(call.reasoning_effort)],
    ["Fast", call.fast == null ? "-" : call.fast ? t("Yes") : t("No")],
    [t("Status"), call.status],
    ["Finish Reason", show(call.finish_reason)],
    ["HTTP Status", show(call.http_status)],
    ["Created At", `${call.created_at_ms} · ${new Date(call.created_at_ms).toLocaleString()}`],
    [t("Duration"), timing(call.duration_ms)],
    ["TTFB", timing(call.ttfb_ms)],
    ["TTFR", timing(call.ttfr_ms)],
    ["TTFT", timing(call.ttft_ms)],
    ["Input Token", show(call.input_tokens)],
    ["Output Token", show(call.output_tokens)],
    ["Total Token", show(call.total_tokens)],
    ["Cache Read Token", show(call.cache_read_tokens)],
    ["Cache Write Token", show(call.cache_write_tokens)],
    ["Reasoning Token", show(call.reasoning_tokens)],
    [t("Messages"), call.message_count],
    [t("Tools"), call.tool_count],
    [t("Detailed records"), call.detailed ? t("Yes") : t("No")],
    ["Error Kind", show(call.error_kind)],
    ["Error Message", show(call.error_message)],
  ];

  const tabs: TabItem[] = [
    { value: "call", label: t("Call information"), content: <section>
      <dl className={styles.details}>{fields.map(([label, value]) => <div key={label}><dt>{label}</dt><dd><code>{value}</code></dd></div>)}</dl>
    </section> },
    { value: "request", label: t("Request"), content: <section>
      {request ? <>
        <div className={styles.meta}>{t("Bytes")}: {request.byte_count}</div>
        <h4>{t("Request headers")}</h4><JsonEditor ariaLabel={t("Request headers")} value={JSON.stringify(request.headers)} readOnly />
        <h4>{t("Request body")}</h4><JsonEditor ariaLabel={t("Request body")} value={JSON.stringify(request.body)} readOnly detail />
      </> : <div className={styles.empty}>{t("Request content was not recorded. Enable detailed records and try again.")}</div>}
    </section> },
    { value: "response", label: t("Response stream"), content: <section>
      {chunks.length > 0 ? <>
        <div className={styles.meta}>{t("Chunks")}: {chunks.length} · {t("Bytes")}: {responseBytes}</div>
        <JsonEditor ariaLabel={t("Response stream")} value={responseBody} readOnly detail />
      </> : <div className={styles.empty}>{t("Response content was not recorded. Enable detailed records and try again.")}</div>}
    </section> },
  ];

  if (cursorTrace) tabs.push({ value: "cursor-trace", label: t("Cursor tracing"), content: <section>
      <div className={styles.meta}>
        Request ID: {cursorTrace.trace.request_id} · {t("Artifacts")}: {cursorTrace.artifacts.length}
      </div>
      <JsonEditor
        ariaLabel={t("Cursor tracing")}
        value={JSON.stringify({ trace: cursorTrace.trace, artifacts: cursorTrace.artifacts })}
        readOnly
        detail
      />
    </section> });

  return <div className={styles.root}><Tabs items={tabs} /></div>;
}
