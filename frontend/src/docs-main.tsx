import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { LanguageProvider } from "@/i18n";
import Docs from "@/pages/docs";
import "@/styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <LanguageProvider>
      <Docs />
    </LanguageProvider>
  </StrictMode>,
);
