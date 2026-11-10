// The interface is English only. t() keeps every user-facing string in one recognizable call
// and fills {name} placeholders; see plugins/static-i18n-plugin.ts.
export type Locale = "en-US";
/** Language of the built-in commit prompt; older saved settings may still say zh-CN. */
export type CommitPromptLocale = "zh-CN" | "en-US";

export const LOCALE: Locale = "en-US";

export type TranslationValue = string | number;
export type TranslationParams = Readonly<Record<string, TranslationValue>>;

export function t(source: string, params?: TranslationParams): string {
  if (!params) return source;
  return source.replace(/\{([A-Za-z_][A-Za-z0-9_]*)\}/g, (_match, name: string) => {
    if (!Object.prototype.hasOwnProperty.call(params, name)) {
      throw new Error(`Missing text parameter: ${name}`);
    }
    return String(params[name]);
  });
}
