import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { LanguageProvider } from "@/i18n";
import Home from "@/pages/home";
import "@/styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <LanguageProvider>
      <Home />
    </LanguageProvider>
  </StrictMode>,
);
