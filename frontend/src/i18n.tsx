import * as React from "react";

export type Lang = "zh" | "en";
const STORAGE_KEY = "lug_lang";

function detect(): Lang {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === "zh" || saved === "en") return saved;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

const LangContext = React.createContext<{
  lang: Lang;
  setLang: (l: Lang) => void;
}>({
  lang: "zh",
  setLang: () => {},
});

export function LanguageProvider({ children }: { children: React.ReactNode }) {
  const [lang, setLangState] = React.useState<Lang>(detect);
  React.useEffect(() => {
    document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
  }, [lang]);
  const setLang = React.useCallback((l: Lang) => {
    setLangState(l);
    localStorage.setItem(STORAGE_KEY, l);
  }, []);
  return (
    <LangContext.Provider value={{ lang, setLang }}>
      {children}
    </LangContext.Provider>
  );
}

export function useI18n() {
  const { lang, setLang } = React.useContext(LangContext);
  return {
    lang,
    setLang,
    t: (zh: React.ReactNode, en: React.ReactNode): React.ReactNode =>
      lang === "zh" ? zh : en,
    ts: (zh: string, en: string): string => (lang === "zh" ? zh : en),
  };
}

export function useTitle(title: { zh: string; en: string }) {
  const { lang } = React.useContext(LangContext);
  React.useEffect(() => {
    document.title = lang === "zh" ? title.zh : title.en;
  }, [lang, title.en, title.zh]);
}

export function locale(lang: Lang) {
  return lang === "zh" ? "zh-CN" : "en-US";
}
