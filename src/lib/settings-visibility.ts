import type { AppConfig } from './types';

type MainKey = 'claude' | 'codex' | 'gemini' | 'cursor' | 'copilot' | 'nan' | 'openrouter' | 'anthropicApi' | 'openaiApi';

/** Clave de configuración de cada cuenta principal; el id del informe puede ser distinto. */
export const CONFIG_KEY: Readonly<Record<string, MainKey>> = {
  claude: 'claude', codex: 'codex', gemini: 'gemini', cursor: 'cursor',
  copilot: 'copilot', nan: 'nan', openrouter: 'openrouter',
  anthropic_api: 'anthropicApi', openai_api: 'openaiApi',
};

export const isAccountVisible = (cfg: Pick<AppConfig, 'hiddenProviders'>, id: string, enabled: boolean) =>
  enabled && !cfg.hiddenProviders.includes(id);

/** Borrador manual: activar recupera solo este círculo, sin alterar otras ocultaciones. */
export function setAccountVisibility(cfg: AppConfig, id: string, enabled: boolean): AppConfig {
  let next: AppConfig;
  if (cfg.accounts.some((a) => a.id === id)) {
    next = { ...cfg, accounts: cfg.accounts.map((a) => a.id === id ? { ...a, enabled } : a) };
  } else {
    if (!Object.prototype.hasOwnProperty.call(CONFIG_KEY, id)) return cfg;
    const key = CONFIG_KEY[id];
    next = { ...cfg, [key]: { ...cfg[key], enabled } };
  }
  return { ...next, hiddenProviders: enabled ? cfg.hiddenProviders.filter((x) => x !== id) : cfg.hiddenProviders };
}
