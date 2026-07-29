import { useCallback, useEffect, useRef, useState } from "react";
import "./App.css";
import { AppleID } from "./AppleID";
import { Device, DeviceInfo } from "./Device";
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  sideloadOperation,
  installSideStoreOperation,
  installLiveContainerOperation,
  Operation,
  OperationState,
  OperationUpdate,
} from "./components/operations";
import { listen } from "@tauri-apps/api/event";
import OperationView from "./components/OperationView";
import { toast } from "sonner";
import { Modal } from "./components/Modal";
import { Certificates } from "./pages/Certificates";
import { AppIds } from "./pages/AppIds";
import { Settings } from "./pages/Settings";
import { Pairing } from "./pages/Pairing";
import { getVersion } from "@tauri-apps/api/app";
import { checkForUpdates } from "./update";
import logo from "./iloader.svg";
import { GlassCard } from "./components/GlassCard";
import { useTranslation } from "react-i18next";
import { usePlatform } from "./PlatformContext";

function App() {
  const { t } = useTranslation();

  const [operationState, setOperationState] = useState<OperationState | null>(
    null,
  );
  const [loggedInAs, setLoggedInAs] = useState<string | null>(null);
  const [selectedDevice, setSelectedDevice] = useState<DeviceInfo | null>(null);
  const [openModal, setOpenModal] = useState<
    null | "certificates" | "appids" | "pairing"
  >(null);
  const [version, setVersion] = useState<string>("");

  const refreshDevicesRef = useRef<(() => void) | null>(null);

  const [noKeyringAvailable, setNoKeyringAvailable] = useState<boolean>(false);
  const { platform } = usePlatform();

  const checkKeyring = useCallback(async () => {
    try {
      let available = await invoke<boolean>("keyring_available");
      setNoKeyringAvailable(!available);
    } catch (e) {
      console.error("Unable to check keyring availability:", e);
      setNoKeyringAvailable(true);
    }
  }, []);

  useEffect(() => {
    checkKeyring();
  }, [checkKeyring]);

  useEffect(() => {
    const fetchVersion = async () => {
      const version = await getVersion();
      setVersion(version);
    };
    fetchVersion();
  }, []);

  useEffect(() => {
    checkForUpdates();
  }, []);

  const shortcutLabel = useCallback(
    (mac: string, windows: string, linux?: string) => {
      if (platform === "mac") return mac;
      if (platform === "linux") return linux ?? windows;
      return windows;
    },
    [platform],
  );

  const startOperation = useCallback(
    async (
      operation: Operation,
      params: { [key: string]: any },
    ): Promise<void> => {
      setOperationState({
        current: operation,
        started: [],
        failed: [],
        completed: [],
        progress: {},
        transferBytes: {},
      });
      return new Promise<void>(async (resolve, reject) => {
        const unlistenFn = await listen<OperationUpdate>(
          "operation_" + operation.id,
          (event) => {
            setOperationState((old) => {
              if (old == null) return null;
              if (event.payload.updateType === "started") {
                return {
                  ...old,
                  started: [...old.started, event.payload.stepId],
                };
              } else if (event.payload.updateType === "finished") {
                return {
                  ...old,
                  completed: [...old.completed, event.payload.stepId],
                  progress: {
                    ...old.progress,
                    [event.payload.stepId]: 1,
                  },
                };
              } else if (event.payload.updateType === "failed") {
                return {
                  ...old,
                  failed: [
                    ...old.failed,
                    {
                      stepId: event.payload.stepId,
                      extraDetails: event.payload.extraDetails,
                    },
                  ],
                };
              } else if (event.payload.updateType === "progress") {
                const raw = event.payload.progress ?? 0;
                const normalized = Math.max(0, Math.min(1, raw));
                const prevProgress = old.progress[event.payload.stepId] ?? 0;
                const monotonicProgress = Math.max(prevProgress, normalized);

                const nextTransferBytes =
                  event.payload.uploadedBytes !== undefined &&
                  event.payload.totalBytes !== undefined
                    ? (() => {
                        const prev = old.transferBytes[event.payload.stepId];
                        const nextUploaded = Math.max(
                          prev?.uploaded ?? 0,
                          Math.max(0, event.payload.uploadedBytes),
                        );
                        const nextTotal = Math.max(
                          prev?.total ?? 0,
                          Math.max(1, event.payload.totalBytes),
                        );

                        return {
                          ...old.transferBytes,
                          [event.payload.stepId]: {
                            uploaded: Math.min(nextUploaded, nextTotal),
                            total: nextTotal,
                          },
                        };
                      })()
                    : old.transferBytes;

                return {
                  ...old,
                  progress: {
                    ...old.progress,
                    [event.payload.stepId]: monotonicProgress,
                  },
                  transferBytes: nextTransferBytes,
                };
              }
              return old;
            });
          },
        );
        try {
          await invoke(operation.id + "_operation", params);
          unlistenFn();
          resolve();
        } catch (e) {
          unlistenFn();
          reject(e);
        }
      });
    },
    [setOperationState],
  );

  const ensuredLoggedIn = useCallback((): boolean => {
    if (loggedInAs) return true;
    toast.error(t("app.must_be_logged_in"));
    return false;
  }, [loggedInAs, t]);

  const ensureSelectedDevice = useCallback((): boolean => {
    if (selectedDevice) return true;
    toast.error(t("app.must_select_device"));
    return false;
  }, [selectedDevice, t]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key === undefined) return;
      const key = event.key.toLowerCase();
      const primaryPressed = platform === "mac" ? event.metaKey : event.ctrlKey;
      if (!primaryPressed) return;

      if (!event.shiftKey && key === "p") {
        event.preventDefault();
        if (!ensureSelectedDevice()) return;
        setOpenModal("pairing");
      } else if (event.shiftKey && key === "c") {
        event.preventDefault();
        if (!ensuredLoggedIn()) return;
        setOpenModal("certificates");
      } else if (event.shiftKey && key === "a") {
        event.preventDefault();
        if (!ensuredLoggedIn()) return;
        setOpenModal("appids");
      } else if (!event.shiftKey && key === "r") {
        event.preventDefault();
        refreshDevicesRef.current?.();
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [platform, ensureSelectedDevice, ensuredLoggedIn]);

  return (
    <main className="workspace">
      <header className="workspace-header">
        <div className="header-left">
          <div className="title-block">
            <img src={logo} alt={t("app.logo_alt")} className="logo" />
            <div>
              <h1 className="title">iloader</h1>
              <p className="subtitle">{t("subtitle")}</p>
            </div>
          </div>
          <span className="version-pill">
            {t("version")} {version}
          </span>
        </div>
        <div className="header-actions">
          <button
            className="toolbar-button"
            onClick={async () => {
              try {
                await openUrl("https://github.com/nab138/iloader");
              } catch (error) {
                console.error("Failed to open GitHub link", error);
                toast.error(t("app.open_github_failed"));
              }
            }}
          >
            {t("app.github")}
          </button>
        </div>
      </header>
      <div className="workspace-body">
        <aside className="workspace-sidebar">
          <section className="workspace-section">
            <div className="section-header">
              <p className="section-label">{t("app.section_account")}</p>
              {/* here to ensure spacing and stuff is correct */}
              <span className="section-hint placeholder" aria-hidden="true">
                Placeholder
              </span>
            </div>
            <GlassCard className="panel">
              <AppleID
                loggedInAs={loggedInAs}
                setLoggedInAs={setLoggedInAs}
                noKeyringAvailable={noKeyringAvailable}
              />
            </GlassCard>
          </section>
          <section className="workspace-section">
            <p className="section-label">{t("app.section_management")}</p>
            <div className="workspace-list">
              <button
                className="workspace-list-item"
                onClick={() => {
                  if (!ensureSelectedDevice()) return;
                  setOpenModal("pairing");
                }}
              >
                {t("app.manage_pairing_file")}{" "}
                <span aria-hidden="true">{shortcutLabel("⌘P", "Ctrl+P")}</span>
              </button>
              <button
                className="workspace-list-item"
                onClick={() => {
                  refreshDevicesRef.current?.();
                }}
              >
                {t("app.refresh_devices")}{" "}
                <span aria-hidden="true">{shortcutLabel("⌘R", "Ctrl+R")}</span>
              </button>
              <button
                className="workspace-list-item"
                onClick={() => {
                  if (!ensuredLoggedIn()) return;
                  setOpenModal("certificates");
                }}
              >
                {t("app.certificates")}{" "}
                <span aria-hidden="true">
                  {shortcutLabel("⌘⇧C", "Ctrl+Shift+C")}
                </span>
              </button>
              <button
                className="workspace-list-item"
                onClick={() => {
                  if (!ensuredLoggedIn()) return;
                  setOpenModal("appids");
                }}
              >
                {t("app.app_ids")}{" "}
                <span aria-hidden="true">
                  {shortcutLabel("⌘⇧A", "Ctrl+Shift+A")}
                </span>
              </button>
            </div>
          </section>
        </aside>
        <section className="workspace-content">
          <section className="workspace-section">
            <div className="section-header">
              <p className="section-label">{t("app.devices")}</p>
              <span className="section-hint">
                {selectedDevice
                  ? t("app.active_device", {
                      name: `${selectedDevice.name} (${selectedDevice.version})`,
                    })
                  : t("app.select_device")}
              </span>
            </div>
            <GlassCard className="panel">
              <Device
                selectedDevice={selectedDevice}
                setSelectedDevice={setSelectedDevice}
                registerRefresh={(fn) => {
                  refreshDevicesRef.current = fn ?? null;
                }}
              />
            </GlassCard>
          </section>
          <section className="workspace-section">
            <div className="section-header">
              <p className="section-label">{t("app.installers")}</p>
              <span className="section-hint">{t("app.choose_build")}</span>
            </div>
            <GlassCard className="panel">
              <div className="action-row single-row">
                <button
                  onClick={() => {
                    if (!ensuredLoggedIn() || !ensureSelectedDevice()) return;
                    startOperation(installSideStoreOperation, {
                      nightly: false,
                      liveContainer: false,
                    }).catch((e) => {
                      console.log(e.type);
                      console.error(e.message);
                    });
                  }}
                >
                  {t("app.sidestore_stable")}
                </button>
                <button
                  onClick={() => {
                    if (!ensuredLoggedIn() || !ensureSelectedDevice()) return;
                    startOperation(installSideStoreOperation, {
                      nightly: true,
                      liveContainer: false,
                    }).catch((e) => {
                      console.log(e.type);
                      console.error(e.message);
                    });
                  }}
                >
                  {t("app.sidestore_nightly")}
                </button>
                <button
                  onClick={() => {
                    if (!ensuredLoggedIn() || !ensureSelectedDevice()) return;
                    startOperation(installLiveContainerOperation, {
                      nightly: false,
                      liveContainer: true,
                    }).catch((e) => {
                      console.log(e.type);
                      console.error(e.message);
                    });
                  }}
                >
                  {t("app.livecontainer_sidestore_stable")}
                </button>
                <button
                  onClick={() => {
                    if (!ensuredLoggedIn() || !ensureSelectedDevice()) return;
                    startOperation(installLiveContainerOperation, {
                      nightly: true,
                      liveContainer: true,
                    }).catch((e) => {
                      console.log(e.type);
                      console.error(e.message);
                    });
                  }}
                >
                  {t("app.livecontainer_sidestore_nightly")}
                </button>
                <button
                  onClick={async () => {
                    if (!ensuredLoggedIn() || !ensureSelectedDevice()) return;
                    let path = await openFileDialog({
                      multiple: false,
                      filters: [
                        { name: t("app.ipa_files"), extensions: ["ipa"] },
                      ],
                    });
                    if (!path) return;
                    startOperation(sideloadOperation, {
                      appPath: path as string,
                    }).catch((e) => {
                      console.log(e.type);
                      console.error(e.message);
                    });
                  }}
                >
                  {t("app.import_ipa")}
                </button>
              </div>
            </GlassCard>
          </section>
          <section className="workspace-section">
            <p className="section-label">{t("app.settings")}</p>
            <GlassCard className="panel settings-panel">
              <Settings
                ensureSelectedDevice={ensureSelectedDevice}
                setSelectedDevice={setSelectedDevice}
                platform={platform}
                shortcutLabel={shortcutLabel}
                checkKeyring={checkKeyring}
              />
            </GlassCard>
          </section>
          {operationState && (
            <OperationView
              operationState={operationState}
              closeMenu={() => setOperationState(null)}
            />
          )}
        </section>
      </div>
      <Modal
        isOpen={openModal === "certificates"}
        close={() => setOpenModal(null)}
      >
        <Certificates />
      </Modal>
      <Modal isOpen={openModal === "appids"} close={() => setOpenModal(null)}>
        <AppIds />
      </Modal>
      <Modal isOpen={openModal === "pairing"} close={() => setOpenModal(null)}>
        <Pairing />
      </Modal>
    </main>
  );
}

export default App;
