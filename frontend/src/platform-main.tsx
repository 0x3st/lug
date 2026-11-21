import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { LanguageProvider } from "@/i18n";
import Platform from "@/pages/platform";
import "@/styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <LanguageProvider>
      <Platform />
    </LanguageProvider>
  </StrictMode>,
);
