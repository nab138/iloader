import React, { createContext, useContext, useEffect, useState } from "react";
import { Modal } from "./components/Modal";
import "./ErrorContext.css";
import { toast } from "sonner";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Trans, useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";

export const ErrorContext = createContext<{
  err: (msg: string, err: string | null) => string;
}>({ err: () => "" });

export const ErrorProvider: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const { t } = useTranslation();
  const [msg, setMsg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [simpleError, setSimpleError] = useState<string | null>(null);
  const [moreDetailsOpen, setMoreDetailsOpen] = useState<boolean>(false);

  const lowerError = error?.toLowerCase() ?? "";
  const hasPairingError =
    lowerError.includes("pairing") ||
    lowerError.includes("lockdown session") ||
    lowerError.includes("mobiledevicepairing") ||
    lowerError.includes("invalidhostid") ||
    lowerError.includes("invalid host") ||
    lowerError.includes("does not have pairing file") ||
    lowerError.includes("devicelocked") ||
    lowerError.includes("device lockded") ||
    lowerError.includes("sessioninactive") ||
    lowerError.includes("re-pair") ||
    lowerError.includes("pair record");
  const hasInvalidHostId =
    lowerError.includes("invalidhostid") ||
    lowerError.includes("invalid host") ||
    lowerError.includes("does not have pairing file");
  const hasDeviceLocked =
    lowerError.includes("devicelocked") ||
    lowerError.includes("device lockded") ||
    lowerError.includes("unlock the device");

  useEffect(() => {
    if (!error) {
      setSimpleError(null);
      return;
    }
    // a little bit gross but it gets the job done.
    let lines = error?.split("\n").filter((line) => line.includes("●")) ?? [];
    console.log(lines);
    if (lines.length > 0) {
      setSimpleError(lines[lines.length - 1].replace(/●\s*/, "").trim());
    }
  }, [error]);

  return (
    <ErrorContext.Provider
      value={{
        err: (msg: string, err: string | null) => {
          setMsg(msg);
          setError(err);
          setMoreDetailsOpen(false);
          return msg;
        },
      }}
    >
      <Modal
        zIndex={999999999}
        isOpen={error !== null || msg !== null}
        close={() => {
          setError(null);
          setMsg(null);
          setMoreDetailsOpen(false);
        }}
      >
        <div className="error-outer">
          <div className="error-header">
            <h2>{t("error.title", { msg: msg ?? t("error.unknown") })}</h2>
            <button
              onClick={() => {
                navigator.clipboard.writeText(
                  "```\n" +
                  (error?.replace(/^\n+/, "") ?? t("common.no_error")) +
                  "\n```",
                );
                toast.success(t("common.copied_success"));
              }}
            >
              {t("common.copy_to_clipboard")}
            </button>
          </div>
          {simpleError && <pre className="error-inner">{simpleError}</pre>}
          <p style={simpleError ? {} : { marginTop: "0.5rem" }}>
            <Trans
              i18nKey="error.support_message"
              components={{
                discord: (
                  <span
                    onClick={() => openUrl("https://discord.gg/EA6yVgydBz")}
                    role="link"
                    className="error-link"
                  />
                ),
                github: (
                  <span
                    onClick={() =>
                      openUrl("https://github.com/nab138/iloader/issues")
                    }
                    role="link"
                    className="error-link"
                  />
                ),
              }}
            />
          </p>
          {simpleError && (
            <p
              className="error-more-details"
              role="button"
              tabIndex={0}
              onClick={() => setMoreDetailsOpen(!moreDetailsOpen)}
            >
              {t("common.more_details")} {moreDetailsOpen ? "▲" : "▼"}
            </p>
          )}
          {simpleError && !moreDetailsOpen && (
            <pre className="error-inner error-details-measure">
              {error?.replace(/^\n+/, "")}
            </pre>
          )}
          {(moreDetailsOpen || !simpleError) && (
            <pre
              className={`error-inner${simpleError ? " error-details" : ""}`}
            >
              {error?.replace(/^\n+/, "")}
            </pre>
          )}
          {hasPairingError && (
            <div style={{ marginBottom: "0.75rem" }}>
              <p style={{ margin: "0 0 0.25rem 0" }}>
                <strong>{hasInvalidHostId ? "InvalidHostID — Trust broken" : hasDeviceLocked ? "Device Locked" : "Pairing diagnostics"}</strong>
              </p>
              <ul style={{ margin: 0, paddingLeft: "1.2rem" }}>
                {hasInvalidHostId ? (
                  <>
                    <li>This computer's pairing with the device is broken or missing.</li>
                    <li>Click <strong>Re-pair Device</strong> below — unlock the device and tap <strong>Trust</strong> if prompted.</li>
                    <li>If that fails: on iOS go to <em>Settings → General → Transfer or Reset → Reset Location & Privacy</em>, replug USB, then click Re-pair again.</li>
                  </>
                ) : hasDeviceLocked ? (
                  <>
                    <li><strong>Unlock</strong> the device screen (enter passcode / Face ID / Touch ID).</li>
                    <li>Keep the device on the home screen while connected.</li>
                    <li>Retry the operation in iLoader.</li>
                  </>
                ) : (
                  <>
                    <li>Unlock the iPhone/iPad and keep it on the home screen.</li>
                    <li>Reconnect USB and tap Trust if the prompt appears.</li>
                    <li>Retry Export or Place pairing after refreshing installed apps.</li>
                    <li>If this repeats, unpair/trust again from iOS and reconnect.</li>
                  </>
                )}
              </ul>
              {hasPairingError && (
                <button
                  style={{ marginTop: "0.5rem" }}
                  onClick={async () => {
                    toast.info("Re-pairing device — unlock the screen and tap Trust if prompted...");
                    try {
                      await invoke("repair_cmd");
                      toast.success("Device re-paired successfully! Retry your operation.");
                      setError(null);
                      setMsg(null);
                    } catch (e: any) {
                      toast.error("Re-pair failed: " + (e?.toString() ?? "unknown error"));
                    }
                  }}
                >
                  Re-pair Device
                </button>
              )}
            </div>
          )}
          <button
            onClick={() => {
              setError(null);
              setMsg(null);
            }}
          >
            {t("common.dismiss")}
          </button>
        </div>
      </Modal>
      {children}
    </ErrorContext.Provider>
  );
};

export const useError = () => {
  return useContext(ErrorContext);
};
