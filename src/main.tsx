import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { Toaster } from "sonner";
import { StoreProvider } from "./StoreContext";
import { LogProvider } from "./LogContext";
import { ErrorProvider } from "./ErrorContext";
import { DialogProvider } from "./DialogContext";
import "./i18next";
import { PlatformProvider } from "./PlatformContext";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <PlatformProvider>
      <StoreProvider>
        <ErrorProvider>
          <DialogProvider>
            <LogProvider>
              <App />
            </LogProvider>
          </DialogProvider>
        </ErrorProvider>
      </StoreProvider>
    </PlatformProvider>
    <Toaster richColors expand />
  </React.StrictMode>,
);
